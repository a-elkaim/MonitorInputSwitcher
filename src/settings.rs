//! Settings window (egui), run as a short-lived child process of the tray app.
//!
//! It edits config.json, then asks the running app to reload. DDC operations (detect, read,
//! "Switch now") go through the running app so they stay serialized with everything else.

use crate::config::{APP_NAME, AppConfig, InputEntry, autostart};
use crate::ddc::{self, DisplayInfo};
use crate::hotkey::{DIGIT_ROW, Hotkey};
use crate::ipc::{self, Request};
use crate::platform::{self, ICON_PNG};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, Sender};

type Target = (Option<String>, Option<String>);

/// Scan codes of the punctuation keys (PC set 1, which Linux evdev codes match for these keys), by
/// the egui key of their US-layout position.
const PUNCTUATION_SCANCODES: &[(egui::Key, u32)] = &[
    (egui::Key::Backtick, 0x29),
    (egui::Key::Minus, 0x0C),
    (egui::Key::Equals, 0x0D),
    (egui::Key::OpenBracket, 0x1A),
    (egui::Key::CloseBracket, 0x1B),
    (egui::Key::Backslash, 0x2B),
    (egui::Key::Semicolon, 0x27),
    (egui::Key::Quote, 0x28),
    (egui::Key::Comma, 0x33),
    (egui::Key::Period, 0x34),
    (egui::Key::Slash, 0x35),
];

struct Row {
    name: String,
    value: u32,
    hotkey: Option<Hotkey>,
    in_next: bool,
    testing: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Capture {
    Row(usize),
    Toggle,
}

enum Msg {
    Displays(Result<Vec<DisplayInfo>, String>),
    Current(Result<u32, String>),
    Tested(usize, Result<(), String>),
    Capabilities(Result<String, String>),
}

struct SettingsApp {
    config: AppConfig,
    rows: Vec<Row>,
    toggle: Option<Hotkey>,
    autostart: bool,
    notifications: bool,
    startup_notification: bool,
    displays: Option<Vec<DisplayInfo>>,
    /// (label, target) entries of the monitor picker.
    monitors: Vec<(String, Target)>,
    selected: usize,
    current_text: String,
    inputs_text: String,
    detecting: bool,
    /// Content height the window was last grown to fit (see `fit_to_content`).
    fitted_height: f32,
    error: Option<String>,
    capture: Option<Capture>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

// --- DDC through the running app, or directly when it isn't running ----------------------------

fn remote(req: Request) -> Option<Result<ipc::Response, String>> {
    ipc::send(&req).map(|r| if r.ok { Ok(r) } else { Err(r.message) })
}

fn list_displays() -> Result<Vec<DisplayInfo>, String> {
    match remote(Request {
        cmd: "list".into(),
        ..Default::default()
    }) {
        Some(r) => r.map(|r| r.displays.unwrap_or_default()),
        None => ddc::list_displays(),
    }
}

fn get_input(target: Target) -> Result<u32, String> {
    match remote(Request {
        cmd: "get".into(),
        monitor: Some(target.clone()),
        ..Default::default()
    }) {
        Some(r) => r.and_then(|r| r.value.ok_or_else(|| "No value returned".into())),
        None => ddc::Monitor::new(target.0, target.1).get_input(),
    }
}

fn capabilities(target: Target) -> Result<String, String> {
    match remote(Request {
        cmd: "caps".into(),
        monitor: Some(target.clone()),
        ..Default::default()
    }) {
        Some(r) => r.and_then(|r| r.caps.ok_or_else(|| "No capabilities returned".into())),
        None => ddc::Monitor::new(target.0, target.1).capabilities(),
    }
}

fn set_input(target: Target, value: u32) -> Result<(), String> {
    match remote(Request {
        cmd: "set".into(),
        monitor: Some(target.clone()),
        value: Some(value),
        ..Default::default()
    }) {
        Some(r) => r.map(|_| ()),
        None => ddc::Monitor::new(target.0, target.1).set_input(value),
    }
}

impl SettingsApp {
    fn new(ctx: &egui::Context) -> Self {
        let config = AppConfig::load_or_default();
        let (tx, rx) = mpsc::channel();
        let mut app = SettingsApp {
            rows: config
                .inputs
                .iter()
                .map(|e| Row {
                    name: e.name.clone(),
                    value: e.value,
                    hotkey: Hotkey::parse_opt(&e.hotkey),
                    in_next: e.include_in_next,
                    testing: false,
                })
                .collect(),
            toggle: Hotkey::parse_opt(&config.toggle_hotkey),
            autostart: autostart::is_enabled(),
            notifications: config.show_notifications,
            startup_notification: config.show_startup_notification,
            config,
            displays: None,
            monitors: Vec::new(),
            selected: 0,
            current_text: String::new(),
            inputs_text: String::new(),
            detecting: false,
            fitted_height: 0.0,
            error: None,
            capture: None,
            tx,
            rx,
        };
        app.fill_monitors(&[]);
        app.load_monitors(ctx);
        app
    }

    fn spawn(&self, ctx: &egui::Context, job: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    fn load_monitors(&mut self, ctx: &egui::Context) {
        self.displays = None;
        self.spawn(ctx, || Msg::Displays(list_displays()));
    }

    fn fill_monitors(&mut self, displays: &[DisplayInfo]) {
        let (name, device) = (
            self.config
                .monitor_name
                .clone()
                .unwrap_or_default()
                .to_lowercase(),
            self.config.monitor_device.clone(),
        );
        self.monitors = displays
            .iter()
            .map(|d| (d.label(), (Some(d.model.clone()), Some(d.bus.clone()))))
            .collect();
        let found = displays
            .iter()
            .position(|d| {
                d.model.to_lowercase() == name && device.as_ref().is_none_or(|dev| *dev == d.bus)
            })
            .or_else(|| displays.iter().position(|d| d.model.to_lowercase() == name));
        self.selected = match found {
            Some(i) => i,
            None if self.config.monitor_name.is_some() || self.config.monitor_device.is_some() => {
                let label = self
                    .config
                    .monitor_name
                    .clone()
                    .or(self.config.monitor_device.clone())
                    .unwrap_or_default();
                self.monitors.push((
                    format!("{label}  (not detected)"),
                    (
                        self.config.monitor_name.clone(),
                        self.config.monitor_device.clone(),
                    ),
                ));
                self.monitors.len() - 1
            }
            None => 0,
        };
    }

    fn target(&self) -> Target {
        self.monitors
            .get(self.selected)
            .map(|(_, t)| t.clone())
            .unwrap_or((
                self.config.monitor_name.clone(),
                self.config.monitor_device.clone(),
            ))
    }

    fn set_capture(&mut self, capture: Option<Capture>) {
        if self.capture.is_some() != capture.is_some() {
            // While recording, global shortcuts are suspended so the keys reach this window.
            platform::block_shortcuts_for_capture(capture.is_some());
        }
        self.capture = capture;
    }

    fn handle_capture(&mut self, ctx: &egui::Context) {
        let Some(capture) = self.capture else { return };
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } = ev
            else {
                continue;
            };
            let plain = !(modifiers.ctrl || modifiers.alt || modifiers.shift);
            let slot = match capture {
                Capture::Row(i) => match self.rows.get_mut(i) {
                    Some(r) => &mut r.hotkey,
                    None => return self.set_capture(None),
                },
                Capture::Toggle => &mut self.toggle,
            };
            match key {
                egui::Key::Escape if plain => {}
                egui::Key::Backspace | egui::Key::Delete if plain => *slot = None,
                _ => {
                    // Digit-row keys are recorded by position, as on Windows: Shift+1 stays "Shift+1" on AZERTY.
                    let digit = physical_key
                        .map(|k| k.name())
                        .filter(|n| n.len() == 1 && DIGIT_ROW.iter().any(|d| n.starts_with(*d)));
                    // Punctuation keys are recorded as the character the layout puts on them. egui
                    // has no name for characters like "²", so it reports such keys by their US
                    // position instead (the "²" key of a French keyboard arrives as Backtick).
                    let punctuation = physical_key
                        .and_then(|p| PUNCTUATION_SCANCODES.iter().find(|(k, _)| *k == p))
                        .and_then(|(_, code)| platform::layout_char(*code))
                        .map(String::from);
                    let raw = digit
                        .map(str::to_string)
                        .or(punctuation)
                        .unwrap_or_else(|| key.name().to_string());
                    let Some(name) = crate::hotkey::canonical_key(&raw) else {
                        continue;
                    };
                    *slot = Some(Hotkey {
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                        meta: false,
                        key: name,
                    });
                }
            }
            return self.set_capture(None);
        }
    }

    fn hotkey_button(&mut self, ui: &mut egui::Ui, which: Capture) {
        let recording = self.capture == Some(which);
        let value = match which {
            Capture::Row(i) => &self.rows[i].hotkey,
            Capture::Toggle => &self.toggle,
        };
        let text = if recording {
            "Press keys... (Esc cancels)".to_string()
        } else {
            value
                .as_ref()
                .map_or("(none) - click to set".into(), Hotkey::to_string)
        };
        let button = egui::Button::new(text)
            .selected(recording)
            .min_size(egui::vec2(200.0, 0.0));
        let resp = ui
            .add(button)
            .on_hover_text("Click, then press the key combination. Backspace clears it.");
        if resp.clicked() {
            self.set_capture(if recording { None } else { Some(which) });
        }
    }

    /// Adds a row for every input the monitor lists that isn't configured yet.
    fn add_detected_inputs(&mut self, caps: &str) {
        let found = crate::autoconfig::input_entries(&ddc::caps_inputs(caps));
        if found.is_empty() {
            self.inputs_text =
                "The monitor's capabilities list no input codes; add them by hand.".into();
            return;
        }
        let mut added = Vec::new();
        for (value, name) in &found {
            if self
                .rows
                .iter()
                .any(|r| r.value == *value || ddc::matches(r.value, *value))
            {
                continue;
            }
            let used: Vec<Hotkey> = self
                .rows
                .iter()
                .filter_map(|r| r.hotkey.clone())
                .chain(self.toggle.clone())
                .collect();
            let hotkey = (0..9)
                .filter_map(|i| {
                    crate::autoconfig::default_hotkey(i).and_then(|h| Hotkey::parse(&h))
                })
                .find(|h| !used.contains(h));
            self.rows.push(Row {
                name: name.clone(),
                value: *value,
                hotkey,
                in_next: true,
                testing: false,
            });
            added.push(name.clone());
        }
        let all: Vec<String> = found.iter().map(|(v, n)| format!("{n} ({v})")).collect();
        self.inputs_text = format!(
            "The monitor lists {} input(s): {}. {}",
            found.len(),
            all.join(", "),
            if added.is_empty() {
                "All are already configured.".to_string()
            } else {
                format!(
                    "Added: {}. Untick In Next for inputs with nothing plugged in.",
                    added.join(", ")
                )
            }
        );
    }

    fn validate(&self) -> Result<(), String> {
        if self.rows.is_empty() {
            return Err("Add at least one input.".into());
        }
        if self.rows.iter().any(|r| r.name.trim().is_empty()) {
            return Err("Every input needs a name.".into());
        }
        let all: Vec<&Hotkey> = self
            .rows
            .iter()
            .filter_map(|r| r.hotkey.as_ref())
            .chain(self.toggle.as_ref())
            .collect();
        if (1..all.len()).any(|i| all[..i].contains(&all[i])) {
            return Err("The same shortcut is assigned more than once.".into());
        }
        Ok(())
    }

    fn save(&mut self, ctx: &egui::Context) {
        if let Err(e) = self.validate() {
            self.error = Some(e);
            return;
        }
        let (name, device) = self.target();
        let cfg = &mut self.config;
        cfg.monitor_name = name;
        cfg.monitor_device = device;
        cfg.inputs = self
            .rows
            .iter()
            .map(|r| InputEntry {
                name: r.name.trim().into(),
                value: r.value,
                hotkey: r.hotkey.as_ref().map(Hotkey::to_string),
                include_in_next: r.in_next,
            })
            .collect();
        cfg.toggle_hotkey = self.toggle.as_ref().map(Hotkey::to_string);
        cfg.show_notifications = self.notifications;
        cfg.show_startup_notification = self.startup_notification;
        if let Err(e) = cfg.save() {
            self.error = Some(format!("Could not save settings: {e}"));
            return;
        }
        if let Err(e) = autostart::set_enabled(self.autostart) {
            self.error = Some(format!("Could not update startup setting: {e}"));
            return;
        }
        if let Some(r) = ipc::send_cmd("reload")
            && !r.ok
        {
            self.error = Some(r.message);
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl SettingsApp {
    /// Grows the window when its content got taller than the window (more inputs, a status line,
    /// a wrapped hint), up to 90% of the monitor height. It only reacts to content growth, so a
    /// window the user made smaller stays that way until something new is added.
    fn fit_to_content(&mut self, ctx: &egui::Context, content: f32, visible: f32) {
        let overflow = content - visible;
        if overflow <= 1.0 || content <= self.fitted_height + 1.0 {
            return;
        }
        self.fitted_height = content;
        let (inner, monitor) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().monitor_size));
        let Some(inner) = inner else { return };
        let mut height = inner.height() + overflow;
        if let Some(monitor) = monitor {
            height = height.min(monitor.y * 0.9);
        }
        if height > inner.height() + 1.0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                inner.width(),
                height,
            )));
        }
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Displays(Ok(d)) => {
                    self.fill_monitors(&d);
                    self.displays = Some(d);
                }
                Msg::Displays(Err(e)) => {
                    self.fill_monitors(&[]);
                    self.displays = Some(Vec::new());
                    self.current_text = format!("Detection failed: {e}");
                }
                Msg::Current(Ok(v)) => {
                    let name = self
                        .rows
                        .iter()
                        .find(|r| ddc::matches(v, r.value))
                        .map(|r| format!(" = {}", r.name))
                        .unwrap_or_default();
                    self.current_text = format!("Current: {v} (0x{v:02X}){name}");
                }
                Msg::Current(Err(e)) => self.current_text = format!("Error: {e}"),
                Msg::Capabilities(result) => {
                    self.detecting = false;
                    match result {
                        Ok(caps) => self.add_detected_inputs(&caps),
                        Err(e) => {
                            self.inputs_text =
                                format!("Could not read the monitor's capabilities: {e}")
                        }
                    }
                }
                Msg::Tested(i, result) => {
                    if let Some(r) = self.rows.get_mut(i) {
                        r.testing = false;
                    }
                    if let Err(e) = result {
                        self.error = Some(format!("Switch failed: {e}"));
                    }
                }
            }
        }
        self.handle_capture(&ctx);

        // Save and Cancel live in their own bottom panel so they stay visible however tall the rest
        // gets; the rest scrolls if the window is still too small.
        egui::Panel::bottom("buttons").show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            // Align::Min, not Center: a centered row claims the whole height the panel offers, and
            // the auto-sized panel then grows a little every frame until it fills the window.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Cancel").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Save").clicked() {
                    self.save(&ctx);
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let scroll = egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.strong("Monitor");
                    ui.horizontal(|ui| {
                        let label = if self.displays.is_none() {
                            "Detecting displays...".to_string()
                        } else {
                            self.monitors.get(self.selected).map(|m| m.0.clone()).unwrap_or_else(|| "(no displays)".into())
                        };
                        ui.add_enabled_ui(self.displays.is_some(), |ui| {
                            egui::ComboBox::from_id_salt("monitor").width(330.0).selected_text(label).show_ui(ui, |ui| {
                                for (i, (text, _)) in self.monitors.iter().enumerate() {
                                    ui.selectable_value(&mut self.selected, i, text);
                                }
                            });
                        });
                        if ui.button("Refresh").clicked() {
                            self.load_monitors(&ctx);
                        }
                        if ui.button("Read current input").clicked() {
                            self.current_text = "Reading...".into();
                            let target = self.target();
                            self.spawn(&ctx, move || Msg::Current(get_input(target)));
                        }
                    });
                    if !self.current_text.is_empty() {
                        ui.label(&self.current_text);
                    }
                });

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.strong("Inputs");
                    let mut remove = None;
                    egui::Grid::new("inputs").num_columns(7).spacing([8.0, 6.0]).show(ui, |ui| {
                        for h in ["Name", "Value", "", "Shortcut", "In Next", "", ""] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for i in 0..self.rows.len() {
                            ui.add_sized([160.0, 22.0], egui::TextEdit::singleline(&mut self.rows[i].name));
                            ui.add(egui::DragValue::new(&mut self.rows[i].value).range(0..=0xFFFF));
                            ui.weak(ddc::input_name(self.rows[i].value));
                            self.hotkey_button(ui, Capture::Row(i));
                            ui.checkbox(&mut self.rows[i].in_next, "").on_hover_text(
                                "Include this input when cycling with \"Next input\". Untick inputs with nothing plugged in.",
                            );
                            let testing = self.rows[i].testing;
                            if ui.add_enabled(!testing, egui::Button::new("Switch now")).clicked() {
                                self.rows[i].testing = true;
                                let (target, value) = (self.target(), self.rows[i].value);
                                self.spawn(&ctx, move || Msg::Tested(i, set_input(target, value)));
                            }
                            if ui.button("Remove").clicked() {
                                remove = Some(i);
                            }
                            ui.end_row();
                        }
                    });
                    if let Some(i) = remove {
                        self.rows.remove(i);
                        self.set_capture(None);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Add input").clicked() {
                            self.rows.push(Row { name: "New input".into(), value: 0, hotkey: None, in_next: true, testing: false });
                        }
                        let detect = egui::Button::new(if self.detecting { "Detecting..." } else { "Detect inputs" });
                        if ui
                            .add_enabled(!self.detecting, detect)
                            .on_hover_text("Ask the selected monitor which inputs it has, and add the missing ones")
                            .clicked()
                        {
                            self.detecting = true;
                            self.inputs_text.clear();
                            let target = self.target();
                            self.spawn(&ctx, move || Msg::Capabilities(capabilities(target)));
                        }
                    });
                    if !self.inputs_text.is_empty() {
                        ui.label(&self.inputs_text);
                    }
                    ui.weak(format!(
                        "Value = the input code (VCP 0x60) sent to the monitor; standard codes are named next to it. \
                         \"Next input\" only cycles through inputs ticked In Next.{}",
                        platform::SHORTCUTS_HINT
                    ));
                });

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.strong("Options");
                    ui.horizontal(|ui| {
                        ui.label("Next-input shortcut (cycles through the list):");
                        self.hotkey_button(ui, Capture::Toggle);
                    });
                    ui.checkbox(&mut self.autostart, platform::START_WITH_LOGIN);
                    ui.checkbox(&mut self.notifications, "Show a notification after switching");
                    ui.checkbox(
                        &mut self.startup_notification,
                        "Show a notification when the app starts",
                    );
                });
            });
            self.fit_to_content(&ctx, scroll.content_size.y, scroll.inner_rect.height());
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.capture.is_some() {
            platform::block_shortcuts_for_capture(false);
        }
    }
}

pub fn run() -> i32 {
    let icon = image::load_from_memory(ICON_PNG).ok().map(|img| {
        let rgba = img.to_rgba8();
        egui::IconData {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
        }
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("{APP_NAME} - Settings"))
        .with_app_id(crate::config::APP_ID)
        .with_inner_size([800.0, 470.0])
        .with_min_inner_size([700.0, 400.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    match eframe::run_native(
        &format!("{APP_NAME} - Settings"),
        options,
        Box::new(|cc| Ok(Box::new(SettingsApp::new(&cc.egui_ctx)))),
    ) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Could not open the Settings window: {e}");
            1
        }
    }
}
