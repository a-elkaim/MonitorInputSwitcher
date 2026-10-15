//! Single instance: an flock on $XDG_RUNTIME_DIR/monitor-input-switcher.lock.
//! CLI transport: a Unix socket next to it.

use crate::config::APP_ID;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

pub type Stream = UnixStream;

/// Held for the process lifetime.
pub struct InstanceLock(#[allow(dead_code)] File);

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/{APP_ID}-{}", unsafe { libc::getuid() })))
}

fn socket_path() -> PathBuf {
    runtime_dir().join(format!("{APP_ID}.sock"))
}

/// None if another instance holds the lock.
pub fn instance_lock() -> Option<InstanceLock> {
    let dir = runtime_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = File::create(dir.join(format!("{APP_ID}.lock"))).ok()?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (rc == 0).then_some(InstanceLock(file))
}

/// A connection to the running instance, if there is one.
pub fn connect() -> Option<Stream> {
    let stream = UnixStream::connect(socket_path()).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    Some(stream)
}

/// Accepts CLI connections on a background thread; each one is handled on its own thread.
pub fn listen(on_client: impl Fn(Stream) + Send + Sync + Clone + 'static) -> Result<(), String> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path); // stale: we hold the instance lock
    let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let on_client = on_client.clone();
            std::thread::spawn(move || on_client(stream));
        }
    });
    Ok(())
}

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}
