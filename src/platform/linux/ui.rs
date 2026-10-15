//! Tray (StatusNotifierItem via ksni), notifications (org.freedesktop.Notifications) and global
//! shortcuts (kglobalaccel). ksni runs the tray on its own thread; menu clicks and shortcut
//! presses arrive on the app's event channel.

use super::kglobalaccel::{self, GlobalShortcuts};
use super::qtkeys;
use crate::config::{APP_ID, APP_NAME};
use crate::hotkey::Hotkey;
use crate::platform::{ICON_PNG, START_WITH_LOGIN, ShortcutAction, TrayModel, UiEvent};
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use zbus::zvariant::Value;

struct Tray {
    tx: Sender<UiEvent>,
    model: TrayModel,
    icon: Vec<ksni::Icon>,
}

impl Tray {
    fn send(&self, ev: UiEvent) {
        let _ = self.tx.send(ev);
    }
}

impl ksni::Tray for Tray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        APP_ID.into()
    }
    fn title(&self) -> String {
        APP_NAME.into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: APP_NAME.into(),
            ..Default::default()
        }
    }
    fn menu_about_to_show(&mut self) {
        self.send(UiEvent::MenuOpened);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let shortcut = |hk: &Option<Hotkey>| {
            hk.as_ref()
                .map(qtkeys::dbusmenu_shortcut)
                .unwrap_or_default()
        };
        let m = &self.model;
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: m.header.replace('_', "__"),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        for (i, (name, hk, checked)) in m.inputs.iter().enumerate() {
            items.push(
                CheckmarkItem {
                    label: name.replace('_', "__"),
                    checked: *checked,
                    shortcut: shortcut(hk),
                    activate: Box::new(move |t: &mut Self| t.send(UiEvent::Switch(i))),
                    ..Default::default()
                }
                .into(),
            );
        }
        if m.inputs.len() > 1 {
            items.push(
                StandardItem {
                    label: "Next input".into(),
                    shortcut: shortcut(&m.toggle),
                    activate: Box::new(|t: &mut Self| t.send(UiEvent::Next)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Settings...".into(),
                icon_name: "configure".into(),
                activate: Box::new(|t: &mut Self| t.send(UiEvent::OpenSettings)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            CheckmarkItem {
                label: START_WITH_LOGIN.into(),
                checked: m.autostart,
                activate: Box::new(|t: &mut Self| {
                    t.model.autostart = !t.model.autostart;
                    t.send(UiEvent::SetAutostart(t.model.autostart));
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Exit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.send(UiEvent::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

fn tray_icons() -> Vec<ksni::Icon> {
    let Ok(img) = image::load_from_memory(ICON_PNG) else {
        return Vec::new();
    };
    [22u32, 32, 48, 64, 128]
        .into_iter()
        .map(|size| {
            let rgba = image::imageops::resize(
                &img.to_rgba8(),
                size,
                size,
                image::imageops::FilterType::Lanczos3,
            );
            // StatusNotifierItem wants ARGB32 in network byte order.
            let data = rgba
                .pixels()
                .flat_map(|p| [p[3], p[0], p[1], p[2]])
                .collect();
            ksni::Icon {
                width: size as i32,
                height: size as i32,
                data,
            }
        })
        .collect()
}

struct Notifier {
    conn: Option<zbus::blocking::Connection>,
    last_id: u32,
}

impl Notifier {
    fn show(&mut self, summary: &str, body: &str, critical: bool, timeout_ms: i32) {
        let Some(conn) = &self.conn else {
            eprintln!("{summary}: {body}");
            return;
        };
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", Value::from(APP_ID));
        hints.insert("urgency", Value::U8(if critical { 2 } else { 1 }));
        if !critical {
            hints.insert("transient", Value::Bool(true));
        }
        // Successive "switched" notifications replace each other instead of stacking up.
        let replaces = if critical { 0 } else { self.last_id };
        let reply = conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                APP_NAME,
                replaces,
                APP_ID,
                summary,
                body,
                Vec::<&str>::new(),
                hints,
                timeout_ms,
            ),
        );
        match reply.and_then(|m| m.body().deserialize::<u32>()) {
            Ok(id) if !critical => self.last_id = id,
            Ok(_) => {}
            Err(e) => eprintln!("notification failed ({e}): {summary}: {body}"),
        }
    }
}

pub struct Ui {
    tray: ksni::blocking::Handle<Tray>,
    notifier: Notifier,
    shortcuts: Result<GlobalShortcuts, String>,
}

impl Ui {
    pub fn start(tx: Sender<UiEvent>) -> Result<Ui, String> {
        let tray = Tray {
            tx: tx.clone(),
            model: TrayModel::default(),
            icon: tray_icons(),
        }
        .spawn()
        .map_err(|e| format!("Could not create the tray icon: {e}"))?;
        let shortcuts = GlobalShortcuts::connect(move |e| {
            let _ = tx.send(match e {
                kglobalaccel::Event::Pressed(a) => UiEvent::Hotkey(a),
                kglobalaccel::Event::Changed(a, keys) => {
                    UiEvent::HotkeyChanged(a, keys.first().and_then(|k| qtkeys::from_qt(*k)))
                }
            });
        })
        .map_err(|e| {
            format!(
                "KDE's kglobalaccel service could not be reached ({e}). Use the menu, or bind \
                 '{APP_ID} next' to a shortcut."
            )
        });
        Ok(Ui {
            tray,
            notifier: Notifier {
                conn: zbus::blocking::Connection::session().ok(),
                last_id: 0,
            },
            shortcuts,
        })
    }

    pub fn update(&self, model: TrayModel) {
        self.tray.update(move |t| t.model = model);
    }

    pub fn notify(&mut self, summary: &str, body: &str, critical: bool, timeout_ms: i32) {
        self.notifier.show(summary, body, critical, timeout_ms);
    }

    /// Registers (or updates) every shortcut. Returns the ids whose keys couldn't be assigned.
    pub fn register_shortcuts(
        &mut self,
        actions: &[ShortcutAction],
    ) -> Result<Vec<String>, String> {
        let shortcuts = self.shortcuts.as_mut().map_err(|e| e.clone())?;
        let request: Vec<(String, String, Vec<i32>)> = actions
            .iter()
            .map(|a| {
                (
                    a.id.clone(),
                    a.friendly.clone(),
                    a.hotkey.as_ref().map(qtkeys::variants).unwrap_or_default(),
                )
            })
            .collect();
        let assigned = shortcuts.register(&request).map_err(|e| e.to_string())?;
        Ok(actions
            .iter()
            .filter(|a| a.hotkey.is_some() && assigned.get(&a.id).is_none_or(Vec::is_empty))
            .map(|a| a.id.clone())
            .collect())
    }

    /// Not needed with kglobalaccel: the Settings window blocks shortcuts itself while recording.
    pub fn suspend_shortcuts(&mut self, _suspended: bool) {}

    pub fn shutdown(self) {
        if let Ok(s) = &self.shortcuts {
            s.deactivate();
        }
        self.tray.shutdown().wait();
    }
}
