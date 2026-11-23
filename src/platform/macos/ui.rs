//! Menu bar item (NSStatusItem + NSMenu), global hotkeys (Carbon RegisterEventHotKey, which needs
//! no Accessibility permission) and notifications (`osascript`, which works without an app
//! bundle or code signing).
//!
//! AppKit objects live on the main thread, which runs NSApplication (`run_with_app_kit`). The app's
//! event loop runs on another thread and reaches the UI by dispatching closures to the main queue.

use super::keys;
use crate::config::APP_NAME;
use crate::hotkey::Hotkey;
use crate::platform::{ICON_PNG, START_WITH_LOGIN, ShortcutAction, TrayModel, UiEvent};
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSControlStateValueOff, NSControlStateValueOn,
    NSEventModifierFlags, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSObject, NSSize, NSString};
use std::cell::RefCell;
use std::ffi::c_void;
use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

const ID_NEXT: isize = 10;
const ID_SETTINGS: isize = 11;
const ID_STARTUP: isize = 12;
const ID_EXIT: isize = 13;
const ID_INPUT_BASE: isize = 100;

// --- main queue ------------------------------------------------------------------------------------

#[repr(C)]
struct DispatchQueue {
    _private: [u8; 0],
}

unsafe extern "C" {
    static _dispatch_main_q: DispatchQueue;
    fn dispatch_async_f(
        queue: *const DispatchQueue,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
}

type Job = Box<dyn FnOnce() + Send>;

/// Runs `f` on the main thread, from any thread.
fn on_main(f: impl FnOnce() + Send + 'static) {
    unsafe extern "C" fn run(context: *mut c_void) {
        let job = unsafe { Box::from_raw(context as *mut Job) };
        job();
    }
    let job: Box<Job> = Box::new(Box::new(f));
    unsafe {
        dispatch_async_f(&raw const _dispatch_main_q, Box::into_raw(job) as *mut c_void, run);
    }
}

/// Starts NSApplication on the main thread (menu bar only, no Dock icon) and runs `app` on a
/// second thread; the process exits with its return code.
pub fn run_with_app_kit(app: fn() -> i32) -> i32 {
    let Some(mtm) = MainThreadMarker::new() else {
        return app();
    };
    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    std::thread::Builder::new()
        .name("app".into())
        .spawn(move || std::process::exit(app()))
        .expect("spawn app thread");
    ns_app.run();
    0
}

// --- Carbon hotkeys --------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

type EventHandler = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: EventHandler,
        num_types: u32,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: *mut c_void,
        options: u32,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
}

const fn four_cc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const SIGNATURE: u32 = four_cc(b"MISw");
const EVENT_CLASS_KEYBOARD: u32 = four_cc(b"keyb");
const EVENT_HOTKEY_PRESSED: u32 = 5;
const PARAM_DIRECT_OBJECT: u32 = four_cc(b"----");
const TYPE_HOTKEY_ID: u32 = four_cc(b"hkid");

unsafe extern "C" fn hotkey_handler(_: *mut c_void, event: *mut c_void, _: *mut c_void) -> i32 {
    let mut id = EventHotKeyId {
        signature: 0,
        id: 0,
    };
    let rc = unsafe {
        GetEventParameter(
            event,
            PARAM_DIRECT_OBJECT,
            TYPE_HOTKEY_ID,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>(),
            std::ptr::null_mut(),
            &mut id as *mut _ as *mut c_void,
        )
    };
    if rc == 0 && id.signature == SIGNATURE {
        with_state(|s| {
            if let Some((_, action, _)) = s.registered.iter().find(|(i, _, _)| *i == id.id) {
                s.send(UiEvent::Hotkey(action.clone()));
            }
        });
    }
    0
}

// --- menu target -----------------------------------------------------------------------------------

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MonitorInputSwitcherMenuTarget"]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    unsafe impl NSMenuDelegate for MenuTarget {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            with_state(|s| s.send(UiEvent::MenuOpened));
        }
    }

    impl MenuTarget {
        #[unsafe(method(itemClicked:))]
        fn item_clicked(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            with_state(|s| s.handle_command(tag));
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

// --- state (main thread) ---------------------------------------------------------------------------

struct State {
    mtm: MainThreadMarker,
    tx: Sender<UiEvent>,
    _item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    target: Retained<MenuTarget>,
    model: TrayModel,
    /// (tag, item) of the items with a check mark, to update them in place.
    checkable: Vec<(isize, Retained<NSMenuItem>)>,
    /// What the menu was last built from, to rebuild it only when its layout changes.
    built: Option<Vec<String>>,
    actions: Vec<ShortcutAction>,
    /// (hotkey id, action id, Carbon ref)
    registered: Vec<(u32, String, *mut c_void)>,
    suspended: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` with the UI state, unless it is already borrowed (re-entrant call).
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

fn status_image() -> Option<Retained<NSImage>> {
    let symbol = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("display.2"),
        Some(&NSString::from_str(APP_NAME)),
    );
    if symbol.is_some() {
        return symbol;
    }
    let data = NSData::with_bytes(ICON_PNG);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    image.setSize(NSSize::new(18.0, 18.0));
    Some(image)
}

impl State {
    fn send(&self, ev: UiEvent) {
        let _ = self.tx.send(ev);
    }

    fn handle_command(&mut self, tag: isize) {
        match tag {
            ID_NEXT => self.send(UiEvent::Next),
            ID_SETTINGS => self.send(UiEvent::OpenSettings),
            ID_STARTUP => {
                self.model.autostart = !self.model.autostart;
                self.send(UiEvent::SetAutostart(self.model.autostart));
                self.refresh_checks();
            }
            ID_EXIT => self.send(UiEvent::Quit),
            t if t >= ID_INPUT_BASE && ((t - ID_INPUT_BASE) as usize) < self.model.inputs.len() => {
                self.send(UiEvent::Switch((t - ID_INPUT_BASE) as usize))
            }
            _ => {}
        }
    }

    fn checked(&self, tag: isize) -> bool {
        match tag {
            ID_STARTUP => self.model.autostart,
            t => self
                .model
                .inputs
                .get((t - ID_INPUT_BASE) as usize)
                .is_some_and(|i| i.2),
        }
    }

    fn refresh_checks(&self) {
        for (tag, item) in &self.checkable {
            item.setState(if self.checked(*tag) {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }

    fn add_item(&mut self, title: &str, tag: isize, hotkey: Option<&Hotkey>) -> Retained<NSMenuItem> {
        let key = hotkey.and_then(keys::key_equivalent).unwrap_or_default();
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(title),
                Some(sel!(itemClicked:)),
                &NSString::from_str(&key),
            )
        };
        if let Some(hk) = hotkey.filter(|_| !key.is_empty()) {
            let mut mask = NSEventModifierFlags::empty();
            for (on, flag) in [
                (hk.ctrl, NSEventModifierFlags::Control),
                (hk.alt, NSEventModifierFlags::Option),
                (hk.shift, NSEventModifierFlags::Shift),
                (hk.meta, NSEventModifierFlags::Command),
            ] {
                if on {
                    mask |= flag;
                }
            }
            item.setKeyEquivalentModifierMask(mask);
        }
        unsafe { item.setTarget(Some(&self.target)) };
        item.setTag(tag);
        self.menu.addItem(&item);
        item
    }

    fn separator(&self) {
        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
    }

    /// Rebuilds the menu if its items changed, otherwise only updates the check marks (the menu
    /// may be open: the current input is read when it opens).
    fn set_model(&mut self, model: TrayModel) {
        self.model = model;
        let m = &self.model;
        let layout: Vec<String> = std::iter::once(m.header.clone())
            .chain(
                m.inputs
                    .iter()
                    .map(|(n, hk, _)| format!("{n}\t{hk:?}")),
            )
            .chain(std::iter::once(format!("{:?}", m.toggle)))
            .collect();
        if self.built.as_ref() == Some(&layout) {
            self.refresh_checks();
            return;
        }
        self.built = Some(layout);
        self.menu.removeAllItems();
        self.checkable.clear();
        let model = self.model.clone();

        let header = self.add_item(&model.header, 0, None);
        header.setEnabled(false);
        self.separator();
        for (i, (name, hk, _)) in model.inputs.iter().enumerate() {
            let tag = ID_INPUT_BASE + i as isize;
            let item = self.add_item(name, tag, hk.as_ref());
            self.checkable.push((tag, item));
        }
        if model.inputs.len() > 1 {
            self.add_item("Next input", ID_NEXT, model.toggle.as_ref());
        }
        self.separator();
        self.add_item("Settings...", ID_SETTINGS, None);
        let startup = self.add_item(START_WITH_LOGIN, ID_STARTUP, None);
        self.checkable.push((ID_STARTUP, startup));
        self.separator();
        self.add_item(&format!("Quit {APP_NAME}"), ID_EXIT, None);
        self.refresh_checks();
    }

    fn unregister_all(&mut self) {
        for (_, _, r) in self.registered.drain(..) {
            unsafe {
                UnregisterEventHotKey(r);
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
            let id = i as u32 + 1;
            let mut out = std::ptr::null_mut();
            let ok = keys::key_code(hk).is_some_and(|code| {
                let rc = unsafe {
                    RegisterEventHotKey(
                        code,
                        keys::modifiers(hk),
                        EventHotKeyId {
                            signature: SIGNATURE,
                            id,
                        },
                        GetApplicationEventTarget(),
                        0,
                        &mut out,
                    )
                };
                rc == 0
            });
            if ok {
                self.registered.push((id, action.id.clone(), out));
            } else {
                failed.push(action.id.clone());
            }
        }
        failed
    }
}

// --- notifications ---------------------------------------------------------------------------------

/// An AppleScript string literal.
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn show_notification(title: &str, body: &str, critical: bool) {
    let script = format!(
        "display notification {} with title {}{}",
        applescript_string(body),
        applescript_string(title),
        if critical { " sound name \"Basso\"" } else { "" }
    );
    match Command::new("/usr/bin/osascript").arg("-e").arg(script).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("notification failed ({e}): {title}: {body}"),
    }
}

// --- public API ------------------------------------------------------------------------------------

pub struct Ui {}

impl Ui {
    pub fn start(tx: Sender<UiEvent>) -> Result<Ui, String> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        on_main(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                let _ = ready_tx.send(Err("Not on the main thread".into()));
                return;
            };
            let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
            if let Some(button) = item.button(mtm) {
                button.setImage(status_image().as_deref());
                button.setToolTip(Some(&NSString::from_str(APP_NAME)));
            }
            let menu = NSMenu::new(mtm);
            menu.setAutoenablesItems(false);
            let target = MenuTarget::new(mtm);
            menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
            item.setMenu(Some(&menu));

            let types = [EventTypeSpec {
                event_class: EVENT_CLASS_KEYBOARD,
                event_kind: EVENT_HOTKEY_PRESSED,
            }];
            let rc = unsafe {
                InstallEventHandler(
                    GetApplicationEventTarget(),
                    hotkey_handler,
                    1,
                    types.as_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if rc != 0 {
                eprintln!("Could not install the hotkey handler (error {rc})");
            }
            STATE.with(|cell| {
                *cell.borrow_mut() = Some(State {
                    mtm,
                    tx,
                    _item: item,
                    menu,
                    target,
                    model: TrayModel::default(),
                    checkable: Vec::new(),
                    built: None,
                    actions: Vec::new(),
                    registered: Vec::new(),
                    suspended: false,
                })
            });
            let _ = ready_tx.send(Ok(()));
        });
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "The menu bar item could not be created".to_string())??;
        Ok(Ui {})
    }

    /// Runs `f` with the UI state on the main thread.
    fn post(&self, f: impl FnOnce(&mut State) + Send + 'static) {
        on_main(move || {
            with_state(f);
        });
    }

    pub fn update(&self, model: TrayModel) {
        self.post(move |s| s.set_model(model));
    }

    pub fn notify(&mut self, title: &str, body: &str, critical: bool, _timeout_ms: i32) {
        show_notification(title, body, critical);
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
            .map_err(|_| "The menu bar thread did not answer".to_string())
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

    pub fn shutdown(self) {
        let (tx, rx) = mpsc::channel::<()>();
        on_main(move || {
            with_state(|s| s.unregister_all());
            if let Some(state) = STATE.with(|cell| cell.borrow_mut().take()) {
                NSStatusBar::systemStatusBar().removeStatusItem(&state._item);
            }
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
}
