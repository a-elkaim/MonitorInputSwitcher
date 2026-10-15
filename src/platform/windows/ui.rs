//! Tray icon, menu, balloon notifications and global hotkeys, all owned by a hidden window on a
//! dedicated thread (Win32 requires RegisterHotKey and Shell_NotifyIcon to go through the thread
//! that owns the window). The app talks to it by queueing closures and posting WM_APP_CALL.

use super::keys;
use crate::config::APP_NAME;
use crate::hotkey::Hotkey;
use crate::platform::{ICON_PNG, START_WITH_LOGIN, ShortcutAction, TrayModel, UiEvent};
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, RegisterHotKey, SendInput, UnregisterHotKey,
    VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIIF_NOSOUND, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, HICON, HMENU, IDI_APPLICATION, LR_DEFAULTCOLOR, LoadIconW, MF_CHECKED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, SetMenuDefaultItem, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP,
    WM_CLOSE, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, w};

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_CALL: u32 = WM_APP + 2;
const TRAY_ID: u32 = 1;

const ID_NEXT: u32 = 10;
const ID_SETTINGS: u32 = 11;
const ID_STARTUP: u32 = 12;
const ID_EXIT: u32 = 13;
const ID_INPUT_BASE: u32 = 100;

/// Modifier virtual keys whose key-up we inject (L/R Shift, Ctrl, Alt, Win).
const MODIFIER_VKS: [u16; 8] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C];

type Call = Box<dyn FnOnce(&mut State) + Send>;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

struct State {
    hwnd: HWND,
    tx: Sender<UiEvent>,
    calls: Arc<Mutex<Vec<Call>>>,
    model: TrayModel,
    icon: HICON,
    actions: Vec<ShortcutAction>,
    /// (RegisterHotKey id, action id, hotkey)
    registered: Vec<(i32, String, Hotkey)>,
    suspended: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` with the window state, unless it is already borrowed (re-entrant message).
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Copies `s` into a fixed-size, NUL-terminated UTF-16 field (truncating).
fn fill(dst: &mut [u16], s: &str) {
    let max = dst.len() - 1;
    let mut n = 0;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16()).take(max) {
        *d = c;
        n += 1;
    }
    dst[n] = 0;
}

impl State {
    fn send(&self, ev: UiEvent) {
        let _ = self.tx.send(ev);
    }

    fn notify_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            ..Default::default()
        }
    }

    fn add_icon(&self) {
        let mut nid = self.notify_data();
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = self.icon;
        fill(&mut nid.szTip, APP_NAME);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        }
    }

    fn remove_icon(&self) {
        let nid = self.notify_data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    fn balloon(&self, title: &str, body: &str, critical: bool) {
        let mut nid = self.notify_data();
        nid.uFlags = NIF_INFO;
        fill(&mut nid.szInfoTitle, title);
        fill(&mut nid.szInfo, body);
        nid.dwInfoFlags = if critical {
            NIIF_ERROR
        } else {
            NIIF_INFO | NIIF_NOSOUND
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn unregister_all(&mut self) {
        for (id, _, _) in self.registered.drain(..) {
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), id);
            }
        }
    }

    /// Registers `self.actions`; returns the ids that could not be registered.
    fn register(&mut self) -> Vec<String> {
        self.unregister_all();
        if self.suspended {
            return Vec::new();
        }
        let mut failed = Vec::new();
        for (i, action) in self.actions.iter().enumerate() {
            let Some(hk) = &action.hotkey else { continue };
            let id = i as i32 + 1;
            let ok = keys::vk(hk).is_some_and(|vk| unsafe {
                RegisterHotKey(Some(self.hwnd), id, keys::modifiers(hk), u32::from(vk)).is_ok()
            });
            if ok {
                self.registered.push((id, action.id.clone(), hk.clone()));
            } else {
                failed.push(action.id.clone());
            }
        }
        failed
    }

    fn build_menu(&self) -> Option<HMENU> {
        let m = &self.model;
        let menu = unsafe { CreatePopupMenu() }.ok()?;
        let text = |s: &str| HSTRING::from(s.replace('&', "&&"));
        let with_shortcut = |name: &str, hk: &Option<Hotkey>| match hk {
            Some(hk) => HSTRING::from(format!("{}\t{hk}", name.replace('&', "&&"))),
            None => text(name),
        };
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &text(&m.header));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            for (i, (name, hk, checked)) in m.inputs.iter().enumerate() {
                let flags = if *checked {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let _ = AppendMenuW(
                    menu,
                    flags,
                    (ID_INPUT_BASE as usize) + i,
                    &with_shortcut(name, hk),
                );
            }
            if m.inputs.len() > 1 {
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    ID_NEXT as usize,
                    &with_shortcut("Next input", &m.toggle),
                );
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS as usize, w!("Settings..."));
            let _ = SetMenuDefaultItem(menu, ID_SETTINGS, 0);
            let startup = if m.autostart {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(
                menu,
                startup,
                ID_STARTUP as usize,
                &HSTRING::from(START_WITH_LOGIN),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, ID_EXIT as usize, w!("Exit"));
        }
        Some(menu)
    }

    fn handle_command(&mut self, cmd: u32) {
        match cmd {
            0 => {}
            ID_NEXT => self.send(UiEvent::Next),
            ID_SETTINGS => self.send(UiEvent::OpenSettings),
            ID_STARTUP => {
                self.model.autostart = !self.model.autostart;
                self.send(UiEvent::SetAutostart(self.model.autostart));
            }
            ID_EXIT => self.send(UiEvent::Quit),
            c if c >= ID_INPUT_BASE && ((c - ID_INPUT_BASE) as usize) < self.model.inputs.len() => {
                self.send(UiEvent::Switch((c - ID_INPUT_BASE) as usize))
            }
            _ => {}
        }
    }
}

/// The keyboard sits on the monitor's USB hub, which the KVM moves along with the input: switching
/// on key-down disconnects it while the keys are held, and Windows never sees the key-ups. So tell
/// Windows the keys were released before switching, without waiting for the user.
fn release_held_keys(hk: &Hotkey) {
    let vks: Vec<u16> = MODIFIER_VKS
        .iter()
        .copied()
        .chain(keys::vk(hk))
        .filter(|vk| unsafe { GetAsyncKeyState(i32::from(*vk)) } as u16 & 0x8000 != 0)
        .collect();
    if vks.is_empty() {
        return;
    }
    let inputs: Vec<INPUT> = vks
        .iter()
        .map(|vk| {
            let extended = matches!(vk, 0xA3 | 0xA5 | 0x5B | 0x5C);
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(*vk),
                        dwFlags: KEYEVENTF_KEYUP
                            | if extended {
                                KEYEVENTF_EXTENDEDKEY
                            } else {
                                KEYBD_EVENT_FLAGS(0)
                            },
                        ..Default::default()
                    },
                },
            }
        })
        .collect();
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
    // Give the key-ups time to be processed before the USB hub disconnects.
    std::thread::sleep(Duration::from_millis(30));
}

fn show_menu(hwnd: HWND) {
    let Some(Some(menu)) = with_state(|s| {
        s.send(UiEvent::MenuOpened);
        s.build_menu()
    }) else {
        return;
    };
    let mut pt = POINT::default();
    let cmd = unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        // TrackPopupMenu runs a modal loop: no state borrow may be held here.
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd.0 as u32
    };
    with_state(|s| s.handle_command(cmd));
}

fn on_hotkey(id: i32) {
    let Some(Some((action, hk))) = with_state(|s| {
        s.registered
            .iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, a, hk)| (a.clone(), hk.clone()))
    }) else {
        return;
    };
    release_held_keys(&hk);
    with_state(|s| s.send(UiEvent::Hotkey(action)));
}

fn run_calls() {
    let calls: Vec<Call> =
        with_state(|s| std::mem::take(&mut *s.calls.lock().unwrap())).unwrap_or_default();
    for call in calls {
        with_state(call);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_TRAY => {
            let mouse = (lparam.0 & 0xFFFF) as u32;
            if mouse == WM_LBUTTONUP || mouse == WM_RBUTTONUP {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_APP_CALL => {
            run_calls();
            LRESULT(0)
        }
        WM_HOTKEY => {
            on_hotkey(wparam.0 as i32);
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            with_state(|s| {
                s.unregister_all();
                s.remove_icon();
            });
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer restarted: the tray icon is gone, add it again.
            with_state(|s| s.add_icon());
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn make_icon() -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
    let png = image::load_from_memory(ICON_PNG).ok().and_then(|img| {
        let small = img.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
        let mut bytes = Vec::new();
        small
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .ok()?;
        Some(bytes)
    });
    png.and_then(|bytes| unsafe {
        // Windows accepts PNG data as an icon image (Vista and later).
        CreateIconFromResourceEx(
            &bytes,
            true,
            0x0003_0000,
            size as i32,
            size as i32,
            LR_DEFAULTCOLOR,
        )
        .ok()
    })
    .or_else(|| unsafe { LoadIconW(None, IDI_APPLICATION) }.ok())
    .unwrap_or_default()
}

fn window_thread(
    tx: Sender<UiEvent>,
    calls: Arc<Mutex<Vec<Call>>>,
    ready: Sender<Result<isize, String>>,
) {
    let hwnd = unsafe {
        let instance = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(e.to_string()));
                return;
            }
        };
        let class = w!("MonitorInputSwitcherTray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );
        match CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            &HSTRING::from(APP_NAME),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("Could not create the tray window: {e}")));
                return;
            }
        }
    };

    let icon = make_icon();
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(State {
            hwnd,
            tx,
            calls,
            model: TrayModel::default(),
            icon,
            actions: Vec::new(),
            registered: Vec::new(),
            suspended: false,
        })
    });
    with_state(|s| s.add_icon());
    let _ = ready.send(Ok(hwnd.0 as isize));

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyIcon(icon);
    }
    STATE.with(|cell| cell.borrow_mut().take());
}

pub struct Ui {
    hwnd: isize,
    calls: Arc<Mutex<Vec<Call>>>,
    thread: Option<JoinHandle<()>>,
}

impl Ui {
    pub fn start(tx: Sender<UiEvent>) -> Result<Ui, String> {
        let calls: Arc<Mutex<Vec<Call>>> = Arc::default();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_calls = calls.clone();
        let thread = std::thread::Builder::new()
            .name("tray".into())
            .spawn(move || window_thread(tx, thread_calls, ready_tx))
            .map_err(|e| e.to_string())?;
        let hwnd = ready_rx
            .recv()
            .map_err(|_| "The tray thread stopped".to_string())??;
        Ok(Ui {
            hwnd,
            calls,
            thread: Some(thread),
        })
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    /// Runs `call` on the window thread.
    fn post(&self, call: impl FnOnce(&mut State) + Send + 'static) {
        self.calls.lock().unwrap().push(Box::new(call));
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_APP_CALL, WPARAM(0), LPARAM(0));
        }
    }

    pub fn update(&self, model: TrayModel) {
        self.post(move |s| s.model = model);
    }

    pub fn notify(&mut self, title: &str, body: &str, critical: bool, _timeout_ms: i32) {
        let (title, body) = (title.to_string(), body.to_string());
        self.post(move |s| s.balloon(&title, &body, critical));
    }

    /// Registers every shortcut. Returns the ids whose keys couldn't be registered.
    pub fn register_shortcuts(
        &mut self,
        actions: &[ShortcutAction],
    ) -> Result<Vec<String>, String> {
        let actions = actions.to_vec();
        let (tx, rx) = mpsc::channel();
        self.post(move |s| {
            s.actions = actions;
            let _ = tx.send(s.register());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "The tray thread did not answer".to_string())
    }

    /// Releases the hotkeys while the Settings window is open, so it can record them.
    pub fn suspend_shortcuts(&mut self, suspended: bool) {
        self.post(move |s| {
            s.suspended = suspended;
            if suspended {
                s.unregister_all();
            } else {
                s.register();
            }
        });
    }

    pub fn shutdown(mut self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
