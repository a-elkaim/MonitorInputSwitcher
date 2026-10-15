//! Global shortcuts through KDE's kglobalaccel D-Bus service.
//!
//! This is what KDE apps use. It works on Wayland, lets the app set the keys itself, and lists the
//! shortcuts in System Settings > Shortcuts, where they can be changed too; such changes are
//! reported back (`Event::Changed`) so the config stays in sync.

use std::collections::HashMap;
use std::thread;
use zbus::blocking::{Connection, Proxy, proxy::Builder};
use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedObjectPath;

pub const COMPONENT: &str = "monitor-input-switcher";
const COMPONENT_FRIENDLY: &str = "Monitor Input Switcher";
const SET_PRESENT: u32 = 2;
const NO_AUTOLOADING: u32 = 4;
const IS_DEFAULT: u32 = 8;

type Keys = Vec<(Vec<i32>,)>; // a(ai): QKeySequence = up to 4 combined keys

pub enum Event {
    Pressed(String),
    Changed(String, Vec<i32>),
}

fn to_seqs(keys: &[i32]) -> Keys {
    keys.iter().map(|k| (vec![*k, 0, 0, 0],)).collect()
}

fn from_seqs(seqs: Keys) -> Vec<i32> {
    seqs.into_iter()
        .filter_map(|(s,)| s.first().copied().filter(|k| *k != 0))
        .collect()
}

fn proxy<'a>(conn: &Connection, path: String, interface: &'static str) -> zbus::Result<Proxy<'a>> {
    Builder::new(conn)
        .destination("org.kde.kglobalaccel")?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
}

pub struct GlobalShortcuts {
    conn: Connection,
    kga: Proxy<'static>,
    registered: Vec<Vec<String>>,
    listening: bool,
    on_event: std::sync::Arc<dyn Fn(Event) + Send + Sync>,
}

impl GlobalShortcuts {
    pub fn connect(on_event: impl Fn(Event) + Send + Sync + 'static) -> zbus::Result<Self> {
        let conn = Connection::session()?;
        let kga = proxy(&conn, "/kglobalaccel".into(), "org.kde.KGlobalAccel")?;
        // Fails early when kglobalaccel isn't there (not a KDE session).
        let _: Vec<OwnedObjectPath> = kga.call("allComponents", &())?;
        Ok(GlobalShortcuts {
            conn,
            kga,
            registered: Vec::new(),
            listening: false,
            on_event: std::sync::Arc::new(on_event),
        })
    }

    /// actions: [(action id, friendly name, Qt key combos)]. Returns the combos kglobalaccel actually assigned.
    pub fn register(
        &mut self,
        actions: &[(String, String, Vec<i32>)],
    ) -> zbus::Result<HashMap<String, Vec<i32>>> {
        let component_id = vec![
            COMPONENT.to_string(),
            String::new(),
            COMPONENT_FRIENDLY.into(),
            String::new(),
        ];
        if let Ok(existing) = self
            .kga
            .call::<_, _, Vec<Vec<String>>>("allActionsForComponent", &(component_id,))
        {
            for id in existing {
                if let Some(unique) = id.get(1)
                    && !actions.iter().any(|(a, _, _)| a == unique)
                {
                    let _: zbus::Result<bool> =
                        self.kga.call("unregister", &(COMPONENT, unique.as_str()));
                }
            }
        }

        let mut assigned = HashMap::new();
        self.registered.clear();
        for (unique, friendly, keys) in actions {
            let id = vec![
                COMPONENT.to_string(),
                unique.clone(),
                COMPONENT_FRIENDLY.into(),
                friendly.clone(),
            ];
            self.kga.call::<_, _, ()>("doRegister", &(&id,))?;
            let _: Keys = self.kga.call(
                "setShortcutKeys",
                &(&id, to_seqs(keys), IS_DEFAULT | NO_AUTOLOADING),
            )?;
            let got: Keys = self.kga.call(
                "setShortcutKeys",
                &(&id, to_seqs(keys), SET_PRESENT | NO_AUTOLOADING),
            )?;
            assigned.insert(unique.clone(), from_seqs(got));
            self.registered.push(id);
        }
        if !self.listening {
            self.listen()?;
            self.listening = true;
        }
        Ok(assigned)
    }

    fn listen(&self) -> zbus::Result<()> {
        let path: OwnedObjectPath = self.kga.call("getComponent", &(COMPONENT,))?;
        let component = proxy(
            &self.conn,
            path.as_str().to_string(),
            "org.kde.kglobalaccel.Component",
        )?;
        let pressed = component.receive_signal("globalShortcutPressed")?;
        let changed = self.kga.receive_signal("yourShortcutsChanged")?;

        let cb = self.on_event.clone();
        thread::spawn(move || {
            for msg in pressed {
                if let Ok((comp, action, _ts)) = msg.body().deserialize::<(String, String, i64)>()
                    && comp == COMPONENT
                {
                    cb(Event::Pressed(action));
                }
            }
        });
        let cb = self.on_event.clone();
        thread::spawn(move || {
            for msg in changed {
                if let Ok((id, keys)) = msg.body().deserialize::<(Vec<String>, Keys)>()
                    && id.first().map(String::as_str) == Some(COMPONENT)
                    && let Some(action) = id.get(1)
                {
                    cb(Event::Changed(action.clone(), from_seqs(keys)));
                }
            }
        });
        Ok(())
    }

    /// Marks the shortcuts inactive (they stay listed in System Settings) when the app quits.
    pub fn deactivate(&self) {
        for id in &self.registered {
            let _: zbus::Result<()> = self.kga.call("setInactive", &(id,));
        }
    }
}

/// Suspends all global shortcuts while the Settings window records a key combination.
pub fn set_blocked(blocked: bool) {
    if let Ok(conn) = Connection::session()
        && let Ok(kga) = proxy(&conn, "/kglobalaccel".into(), "org.kde.KGlobalAccel")
    {
        let _: zbus::Result<()> = kga.call("blockGlobalShortcuts", &(blocked,));
    }
}
