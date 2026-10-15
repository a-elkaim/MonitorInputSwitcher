//! Monitor Input Switcher: switch the monitor input over DDC/CI, from the tray or a global
//! shortcut, on Linux (KDE Plasma) and Windows.
//!
//! No arguments starts the tray app. Subcommands control the running app, or act directly on the
//! monitor when it isn't running.

// A tray app: no console window on Windows (CLI commands attach to the parent's console).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autoconfig;
mod config;
mod daemon;
mod ddc;
mod hotkey;
mod ipc;
mod platform;
mod settings;

use config::{APP_ID, APP_NAME, AppConfig};
use ipc::Request;

const USAGE: &str = "\
Usage: monitor-input-switcher [COMMAND]

Without a command, starts the tray app (only one instance runs).

Commands:
  switch <INPUT>   switch to an input, by name or input code (e.g. \"USB-C\", HDMI-1, 27, 0x11)
  next             switch to the next configured input
  get              print the current input
  list             list detected displays
  capabilities     show the monitor's model and the input codes it accepts
  settings         open the Settings window of the running app
  quit             stop the running app
  help             show this help";

/// The tray app isn't running: talk to the monitor directly.
fn run_locally(cmd: &str, arg: &str) -> Result<String, String> {
    let config = AppConfig::load_or_default();
    let mut monitor = ddc::Monitor::new(config.monitor_name.clone(), config.monitor_device.clone());
    match cmd {
        "list" => ddc::list_displays().map(|d| ipc::displays_text(&d)),
        "caps" => monitor.capabilities().map(|c| ipc::caps_text(&c)),
        "get" => monitor.get_input().map(|v| ipc::describe_input(&config, v)),
        "switch" => {
            let (value, name) = match ipc::find_input(&config, arg) {
                Some(e) => (e.value, e.name.clone()),
                // A raw input code that isn't configured, e.g. "switch 17".
                None => {
                    let value = ipc::parse_value(arg).ok_or_else(|| {
                        format!(
                            "No input named or valued {arg:?}. Inputs: {}",
                            ipc::inputs_text(&config)
                        )
                    })?;
                    (value, ddc::input_name(value))
                }
            };
            monitor.set_input(value)?;
            Ok(format!("Switched to {name}"))
        }
        "next" => {
            let current = monitor.get_input().ok();
            let target = config::next_input(&config.inputs, current)?;
            monitor.set_input(target.value)?;
            Ok(format!("Switched to {}", target.name))
        }
        _ => Err(format!("{APP_NAME} is not running")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("run");
    let arg = args.get(1).cloned().unwrap_or_default();
    if !matches!(cmd, "run" | "settings-window") {
        platform::attach_console();
    }

    let code = match cmd {
        "run" => daemon::run(),
        "settings-window" => settings::run(),
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            0
        }
        "switch" if arg.is_empty() => {
            eprintln!("Usage: {APP_ID} switch <INPUT>");
            2
        }
        "switch" | "next" | "get" | "list" | "capabilities" | "caps" | "settings" | "quit" => {
            let cmd = if cmd == "capabilities" { "caps" } else { cmd };
            let req = Request {
                cmd: cmd.into(),
                arg: (!arg.is_empty()).then(|| arg.clone()),
                ..Default::default()
            };
            let result = match ipc::send(&req) {
                Some(r) if r.ok => Ok(r.message),
                Some(r) => Err(r.message),
                None => run_locally(cmd, &arg),
            };
            match result {
                Ok(m) => {
                    println!("{m}");
                    0
                }
                Err(m) => {
                    eprintln!("{m}");
                    1
                }
            }
        }
        other => {
            eprintln!("Unknown command {other:?}\n\n{USAGE}");
            2
        }
    };
    std::process::exit(code);
}
