//! The tray app. One event loop thread owns all state; DDC runs on a single worker thread so
//! operations never overlap and never block the menu.

use crate::autoconfig;
use crate::config::{APP_NAME, AppConfig, InputEntry, Loaded, autostart};
use crate::ddc::{self, Monitor};
use crate::hotkey::Hotkey;
use crate::ipc::{self, Request, Response};
use crate::platform::{self, ShortcutAction, TrayModel, Ui, UiEvent};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

const TOGGLE_ACTION: &str = "next-input";

type Reply = Sender<Response>;

enum Ev {
    Ui(UiEvent),
    SettingsClosed,
    Ipc(Request, Reply),
    InputKnown(u32),
    AutoConfigured(Result<autoconfig::Detected, String>),
    SwitchDone {
        entry: Option<InputEntry>,
        result: Result<InputEntry, String>,
        reply: Option<Reply>,
        from_hotkey: bool,
    },
}

fn input_action_id(index: usize) -> String {
    format!("switch-input-{}", index + 1)
}

// --- DDC worker ----------------------------------------------------------------------------------

/// The configured monitor plus any monitor the Settings window is trying out, with cached buses.
struct Monitors {
    current: Monitor,
    others: Vec<Monitor>,
}

impl Monitors {
    fn get(&mut self, target: Option<(Option<String>, Option<String>)>) -> &mut Monitor {
        let Some((name, device)) = target else {
            return &mut self.current;
        };
        if self.current.is(&name, &device) {
            return &mut self.current;
        }
        match self.others.iter().position(|m| m.is(&name, &device)) {
            Some(i) => &mut self.others[i],
            None => {
                self.others.push(Monitor::new(name, device));
                self.others.last_mut().unwrap()
            }
        }
    }
}

type Job = Box<dyn FnOnce(&mut Monitors) + Send>;

fn spawn_ddc_worker(config: &AppConfig) -> Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    let mut monitors = Monitors {
        current: Monitor::new(config.monitor_name.clone(), config.monitor_device.clone()),
        others: Vec::new(),
    };
    thread::Builder::new()
        .name("ddc".into())
        .spawn(move || {
            monitors.current.warm_up();
            for job in rx {
                job(&mut monitors);
            }
        })
        .expect("spawn ddc thread");
    tx
}

// --- app -----------------------------------------------------------------------------------------

struct App {
    config: AppConfig,
    last_known: Option<u32>,
    tx: Sender<Ev>,
    ddc: Sender<Job>,
    ui: Ui,
    hotkey_busy: bool,
    settings_open: bool,
}

pub fn run() -> i32 {
    let Some(_lock) = platform::ipc::instance_lock() else {
        // Already running: bring up its Settings window instead of starting a second copy.
        let reply = ipc::send_cmd("settings");
        eprintln!(
            "{APP_NAME} is already running{}",
            if reply.is_some() {
                ""
            } else {
                " (not responding)"
            }
        );
        return 0;
    };

    let (config, first_run, load_error) = match AppConfig::load() {
        Loaded::Ok(c) => (c, false, None),
        Loaded::FirstRun(c) => (c, true, None),
        Loaded::Error(c, e) => (c, false, Some(e)),
    };
    if first_run {
        // Non-fatal on failure: startup can be enabled from the menu.
        let _ = config.save();
        let _ = autostart::set_enabled(true);
    } else {
        autostart::refresh_path();
    }

    let (tx, rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>();
    let forward = tx.clone();
    thread::spawn(move || {
        for ev in ui_rx {
            if forward.send(Ev::Ui(ev)).is_err() {
                break;
            }
        }
    });
    let ui = match Ui::start(ui_tx) {
        Ok(ui) => ui,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let mut app = App {
        ddc: spawn_ddc_worker(&config),
        config,
        last_known: None,
        tx: tx.clone(),
        ui,
        hotkey_busy: false,
        settings_open: false,
    };
    app.update_tray();
    app.register_hotkeys(true);

    let ipc_tx = tx.clone();
    if let Err(e) = platform::ipc::listen(move |stream| {
        ipc::serve(stream, |req| {
            let (reply_tx, reply_rx) = mpsc::channel();
            let _ = ipc_tx.send(Ev::Ipc(req, reply_tx));
            reply_rx
                .recv()
                .unwrap_or_else(|_| Response::err("The app is exiting"))
        })
    }) {
        app.notify_error("Command-line control unavailable", &e);
    }

    if let Some(e) = load_error {
        app.notify_error("Settings not loaded", &e);
    } else if app.config.inputs.is_empty() {
        // First run (or nothing configured): set up whatever monitor is connected. Its
        // notification also says the app is running.
        app.start_autoconfig();
    } else if app.config.show_startup_notification || first_run {
        // Say the app started: otherwise it sits silently in the tray (often in the hidden
        // overflow area) and it isn't obvious that launching it did anything. Settings can turn
        // this off, except on the very first run.
        let mut body =
            "Running in the system tray: click its icon to switch input or open Settings."
                .to_string();
        if let Some(hk) = Hotkey::parse_opt(&app.config.toggle_hotkey) {
            body.push_str(&format!(" {hk} switches to the next input."));
        }
        if first_run {
            body.push_str(" It will also start at login.");
        }
        app.ui
            .notify(&format!("{APP_NAME} is running"), &body, false, 6000);
    }

    app.event_loop(rx);
    platform::ipc::cleanup();
    app.ui.shutdown();
    0
}

impl App {
    fn event_loop(&mut self, rx: Receiver<Ev>) {
        for ev in rx {
            match ev {
                Ev::Ui(UiEvent::Switch(i)) => self.switch(i, None, false),
                Ev::Ui(UiEvent::Next) => self.next_input(None, false),
                Ev::Ui(UiEvent::Hotkey(action)) => self.on_hotkey(&action),
                Ev::Ui(UiEvent::HotkeyChanged(action, hotkey)) => {
                    self.on_hotkey_changed(&action, hotkey)
                }
                Ev::Ui(UiEvent::MenuOpened) => self.refresh_current_input(),
                Ev::Ui(UiEvent::SetAutostart(on)) => {
                    if let Err(e) = autostart::set_enabled(on) {
                        self.notify_error("Could not change startup setting", &e.to_string());
                    }
                    self.update_tray();
                }
                Ev::Ui(UiEvent::OpenSettings) => self.open_settings(),
                Ev::Ui(UiEvent::Quit) => break,
                Ev::SettingsClosed => {
                    self.settings_open = false;
                    platform::block_shortcuts_for_capture(false); // in case Settings died while recording
                    self.ui.suspend_shortcuts(false);
                    self.update_tray();
                }
                Ev::Ipc(req, reply) => {
                    if self.handle_request(req, reply) {
                        break;
                    }
                }
                Ev::InputKnown(v) => {
                    self.last_known = Some(v);
                    self.update_tray();
                }
                Ev::SwitchDone {
                    entry,
                    result,
                    reply,
                    from_hotkey,
                } => self.switch_done(entry, result, reply, from_hotkey),
                Ev::AutoConfigured(result) => self.autoconfigured(result),
            }
        }
    }

    fn update_tray(&self) {
        let cfg = &self.config;
        let header = format!(
            "Monitor: {}",
            cfg.monitor_name
                .as_deref()
                .or(cfg.monitor_device.as_deref())
                .unwrap_or("(auto)")
        );
        let inputs: Vec<_> = cfg
            .inputs
            .iter()
            .map(|e| {
                (
                    e.name.clone(),
                    Hotkey::parse_opt(&e.hotkey),
                    self.last_known.is_some_and(|v| ddc::matches(v, e.value)),
                )
            })
            .collect();
        self.ui.update(TrayModel {
            header,
            inputs,
            toggle: Hotkey::parse_opt(&cfg.toggle_hotkey),
            autostart: autostart::is_enabled(),
        });
    }

    fn notify_error(&mut self, summary: &str, body: &str) {
        self.ui.notify(summary, body, true, 8000);
    }

    fn run_ddc(&self, job: impl FnOnce(&mut Monitors) + Send + 'static) {
        let _ = self.ddc.send(Box::new(job));
    }

    fn refresh_current_input(&self) {
        let tx = self.tx.clone();
        self.run_ddc(move |m| {
            if let Ok(v) = m.current.get_input() {
                let _ = tx.send(Ev::InputKnown(v)); // best-effort: no check mark on failure
            }
        });
    }

    // --- switching -------------------------------------------------------------------------------

    fn switch(&self, index: usize, reply: Option<Reply>, from_hotkey: bool) {
        if let Some(entry) = self.config.inputs.get(index).cloned() {
            self.switch_to(entry, reply, from_hotkey);
        }
    }

    fn switch_to(&self, entry: InputEntry, reply: Option<Reply>, from_hotkey: bool) {
        let tx = self.tx.clone();
        self.run_ddc(move |m| {
            let result = m.current.set_input(entry.value).map(|_| entry.clone());
            let _ = tx.send(Ev::SwitchDone {
                entry: Some(entry),
                result,
                reply,
                from_hotkey,
            });
        });
    }

    fn next_input(&self, reply: Option<Reply>, from_hotkey: bool) {
        let inputs = self.config.inputs.clone();
        let last = self.last_known;
        let tx = self.tx.clone();
        self.run_ddc(move |m| {
            let current = m.current.get_input().ok().or(last);
            let result = crate::config::next_input(&inputs, current)
                .cloned()
                .and_then(|target| m.current.set_input(target.value).map(|_| target));
            let _ = tx.send(Ev::SwitchDone {
                entry: None,
                result,
                reply,
                from_hotkey,
            });
        });
    }

    fn switch_done(
        &mut self,
        entry: Option<InputEntry>,
        result: Result<InputEntry, String>,
        reply: Option<Reply>,
        from_hotkey: bool,
    ) {
        if from_hotkey {
            self.hotkey_busy = false;
        }
        match result {
            Ok(target) => {
                self.last_known = Some(target.value);
                self.update_tray();
                let msg = format!("Switched to {}", target.name);
                if self.config.show_notifications {
                    self.ui.notify("Input switched", &msg, false, 1500);
                }
                if let Some(r) = reply {
                    let _ = r.send(Response::ok(msg));
                }
            }
            Err(e) => {
                let title = entry.map_or("Could not switch input".to_string(), |e| {
                    format!("Could not switch to {}", e.name)
                });
                self.notify_error(&title, &e);
                if let Some(r) = reply {
                    let _ = r.send(Response::err(e));
                }
            }
        }
    }

    // --- global shortcuts ------------------------------------------------------------------------

    /// (action id, friendly name, hotkey) for every shortcut the app owns.
    fn hotkey_actions(&self) -> Vec<(String, String, Option<String>)> {
        let mut out: Vec<_> = self
            .config
            .inputs
            .iter()
            .enumerate()
            .map(|(i, e)| {
                (
                    input_action_id(i),
                    format!("Switch to {}", e.name),
                    e.hotkey.clone(),
                )
            })
            .collect();
        if self.config.inputs.len() > 1 {
            out.push((
                TOGGLE_ACTION.into(),
                "Next input".into(),
                self.config.toggle_hotkey.clone(),
            ));
        }
        out
    }

    fn register_hotkeys(&mut self, show_errors: bool) {
        let actions: Vec<ShortcutAction> = self
            .hotkey_actions()
            .into_iter()
            .map(|(id, friendly, h)| ShortcutAction {
                id,
                friendly,
                hotkey: Hotkey::parse_opt(&h),
            })
            .collect();
        match self.ui.register_shortcuts(&actions) {
            Ok(failed_ids) => {
                let failed: Vec<String> = actions
                    .iter()
                    .filter(|a| failed_ids.contains(&a.id))
                    .map(|a| {
                        format!(
                            "{} ({})",
                            a.hotkey.as_ref().unwrap(),
                            a.friendly.trim_start_matches("Switch to ")
                        )
                    })
                    .collect();
                if show_errors && !failed.is_empty() {
                    self.notify_error(
                        "Some shortcuts are unavailable",
                        &format!("Already used by another shortcut: {}", failed.join(", ")),
                    );
                }
            }
            Err(e) if show_errors => self.notify_error("Global shortcuts unavailable", &e),
            Err(_) => {}
        }
    }

    fn on_hotkey(&mut self, action: &str) {
        if self.hotkey_busy || self.settings_open {
            return;
        }
        // Stuck modifiers (keyboard on the monitor's USB hub, moved by the KVM) are the platform's
        // job: Windows releases them before reporting the press, Linux needs nothing (the kernel
        // removes the device and libinput releases its keys).
        if action == TOGGLE_ACTION {
            self.hotkey_busy = true;
            self.next_input(None, true);
        } else if let Some(i) =
            (0..self.config.inputs.len()).find(|i| input_action_id(*i) == action)
        {
            self.hotkey_busy = true;
            self.switch(i, None, true);
        }
    }

    /// The user changed one of our shortcuts outside the app (KDE System Settings): keep the
    /// config in sync.
    fn on_hotkey_changed(&mut self, action: &str, hotkey: Option<Hotkey>) {
        let current = self
            .hotkey_actions()
            .into_iter()
            .find(|(a, _, _)| a == action)
            .map(|(_, _, h)| Hotkey::parse_opt(&h));
        let Some(current) = current else { return };
        if current == hotkey {
            return;
        }
        let new = hotkey.map(|h| h.to_string());
        if action == TOGGLE_ACTION {
            self.config.toggle_hotkey = new;
        } else if let Some(e) = (0..self.config.inputs.len())
            .find(|i| input_action_id(*i) == action)
            .map(|i| &mut self.config.inputs[i])
        {
            e.hotkey = new;
        }
        if let Err(e) = self.config.save() {
            self.notify_error("Could not save settings", &e.to_string());
        }
        self.update_tray();
        self.register_hotkeys(false);
    }

    // --- settings --------------------------------------------------------------------------------

    fn open_settings(&mut self) {
        if self.settings_open {
            return;
        }
        let exe = crate::config::self_exe();
        match Command::new(exe).arg("settings-window").spawn() {
            Ok(mut child) => {
                self.settings_open = true;
                // Windows: release our hotkeys so the Settings window can record them.
                self.ui.suspend_shortcuts(true);
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let _ = child.wait();
                    let _ = tx.send(Ev::SettingsClosed);
                });
            }
            Err(e) => self.notify_error("Could not open Settings", &e.to_string()),
        }
    }

    fn reload_config(&mut self) -> Response {
        let config = match AppConfig::load() {
            Loaded::Ok(c) => c,
            Loaded::FirstRun(_) => return Response::err("Config file missing"),
            Loaded::Error(_, e) => return Response::err(e),
        };
        self.apply_config(config);
        Response::ok("Settings reloaded")
    }

    fn start_autoconfig(&self) {
        let tx = self.tx.clone();
        self.run_ddc(move |_| {
            let _ = tx.send(Ev::AutoConfigured(autoconfig::detect()));
        });
    }

    fn autoconfigured(&mut self, result: Result<autoconfig::Detected, String>) {
        if !self.config.inputs.is_empty() {
            return; // configured in Settings meanwhile
        }
        let detected = match result {
            Ok(d) => d,
            Err(e) => {
                self.notify_error(
                    "No monitor set up",
                    &format!("{e}. Open Settings to configure one."),
                );
                return;
            }
        };
        let mut config = self.config.clone();
        let display = &detected.display;
        config.monitor_name = (!display.model.is_empty()).then(|| display.model.clone());
        config.monitor_device = display.model.is_empty().then(|| display.bus.clone());
        config.inputs = detected.inputs;
        if let Err(e) = config.save() {
            self.notify_error("Could not save settings", &e.to_string());
        }
        let summary: Vec<String> = config
            .inputs
            .iter()
            .map(|e| match &e.hotkey {
                Some(h) => format!("{} ({h})", e.name),
                None => e.name.clone(),
            })
            .collect();
        let model = config
            .monitor_name
            .clone()
            .unwrap_or_else(|| display.bus.clone());
        self.apply_config(config);
        let mut body = format!(
            "Running in the system tray. Inputs: {}.",
            summary.join(", ")
        );
        if let Some(note) = detected.note {
            body = format!("{body} {note}");
        }
        body.push_str(" In Settings, untick \"In Next\" for inputs with nothing plugged in.");
        self.ui
            .notify(&format!("Set up for {model}"), &body, false, 10000);
    }

    fn apply_config(&mut self, config: AppConfig) {
        if (&config.monitor_name, &config.monitor_device)
            != (&self.config.monitor_name, &self.config.monitor_device)
        {
            let (name, device) = (config.monitor_name.clone(), config.monitor_device.clone());
            self.run_ddc(move |m| {
                m.current = Monitor::new(name, device);
                m.current.warm_up();
            });
            self.last_known = None;
        }
        self.config = config;
        self.update_tray();
        self.register_hotkeys(true);
    }

    /// Returns true when the app should exit.
    fn handle_request(&mut self, req: Request, reply: Reply) -> bool {
        let send = |r: Response| {
            let _ = reply.send(r);
        };
        match req.cmd.as_str() {
            "switch" => {
                let arg = req.arg.unwrap_or_default();
                match self
                    .config
                    .inputs
                    .iter()
                    .position(|e| ipc::find_input(&self.config, &arg) == Some(e))
                {
                    Some(i) => self.switch(i, Some(reply), false),
                    // A raw input code that isn't configured, e.g. "switch 17".
                    None if ipc::parse_value(&arg).is_some() => {
                        let value = ipc::parse_value(&arg).unwrap();
                        let entry = InputEntry::new(ddc::input_name(value), value, None);
                        self.switch_to(entry, Some(reply), false);
                    }
                    None => send(Response::err(format!(
                        "No input named or valued {arg:?}. Inputs: {}",
                        ipc::inputs_text(&self.config)
                    ))),
                }
            }
            "next" => self.next_input(Some(reply), false),
            "get" => {
                let config = self.config.clone();
                let target = req.monitor;
                self.run_ddc(move |m| {
                    let _ = reply.send(match m.get(target).get_input() {
                        Ok(v) => Response {
                            value: Some(v),
                            ..Response::ok(ipc::describe_input(&config, v))
                        },
                        Err(e) => Response::err(e),
                    });
                });
            }
            "caps" => {
                let target = req.monitor;
                self.run_ddc(move |m| {
                    let _ = reply.send(match m.get(target).capabilities() {
                        Ok(caps) => Response {
                            caps: Some(caps.clone()),
                            ..Response::ok(ipc::caps_text(&caps))
                        },
                        Err(e) => Response::err(e),
                    });
                });
            }
            "set" => {
                let (target, value) = (req.monitor, req.value.unwrap_or(0));
                self.run_ddc(move |m| {
                    let _ = reply.send(match m.get(target).set_input(value) {
                        Ok(()) => Response::ok(format!("Set input {value}")),
                        Err(e) => Response::err(e),
                    });
                });
            }
            "list" => self.run_ddc(move |_| {
                let _ = reply.send(match ddc::list_displays() {
                    Ok(d) => Response {
                        displays: Some(d.clone()),
                        ..Response::ok(ipc::displays_text(&d))
                    },
                    Err(e) => Response::err(e),
                });
            }),
            "reload" => send(self.reload_config()),
            "settings" => {
                self.open_settings();
                send(Response::ok("Settings opened"));
            }
            "quit" => {
                send(Response::ok("Exiting"));
                return true;
            }
            "ping" => send(Response::ok("running")),
            other => send(Response::err(format!("Unknown command {other:?}"))),
        }
        false
    }
}
