//! Single instance: a named mutex.
//! CLI transport: a per-user named pipe, local clients only.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::{HSTRING, w};

fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    format!(r"\\.\pipe\monitor-input-switcher-{user}")
}

/// Held for the process lifetime.
pub struct InstanceLock(HANDLE);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// None if another instance is running.
pub fn instance_lock() -> Option<InstanceLock> {
    let handle =
        unsafe { CreateMutexW(None, true, w!(r"Local\MonitorInputSwitcher.SingleInstance")) }
            .ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }
    Some(InstanceLock(handle))
}

/// One end of the pipe. The server end flushes and disconnects when dropped, so the client
/// gets the whole reply.
pub struct Stream {
    file: File,
    server: bool,
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        let handle = HANDLE(self.file.as_raw_handle());
        unsafe { FlushFileBuffers(handle) }.map_err(io::Error::other)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.server {
            let handle = HANDLE(self.file.as_raw_handle());
            unsafe {
                let _ = FlushFileBuffers(handle);
                let _ = DisconnectNamedPipe(handle);
            }
        }
    }
}

/// A connection to the running instance, if there is one.
pub fn connect() -> Option<Stream> {
    for _ in 0..20 {
        match OpenOptions::new().read(true).write(true).open(pipe_name()) {
            Ok(file) => {
                return Some(Stream {
                    file,
                    server: false,
                });
            }
            // All pipe instances busy: the server is about to create the next one.
            Err(e) if e.raw_os_error() == Some(231) => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            Err(_) => return None,
        }
    }
    None
}

/// Accepts CLI connections on a background thread; each one is handled on its own thread.
pub fn listen(on_client: impl Fn(Stream) + Send + Sync + Clone + 'static) -> Result<(), String> {
    let name = HSTRING::from(pipe_name());
    let create = move || unsafe {
        CreateNamedPipeW(
            &name,
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        )
    };
    let first = create();
    if first == INVALID_HANDLE_VALUE {
        return Err(format!(
            "Could not create {}: {}",
            pipe_name(),
            io::Error::last_os_error()
        ));
    }
    let first = first.0 as isize; // raw handles aren't Send
    std::thread::spawn(move || {
        let mut pipe = HANDLE(first as *mut _);
        loop {
            let connected = unsafe { ConnectNamedPipe(pipe, None) }.is_ok()
                || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            if connected {
                let file = unsafe { File::from_raw_handle(pipe.0) };
                let on_client = on_client.clone();
                std::thread::spawn(move || on_client(Stream { file, server: true }));
            } else {
                unsafe {
                    let _ = CloseHandle(pipe);
                }
            }
            pipe = create();
            if pipe == INVALID_HANDLE_VALUE {
                return;
            }
        }
    });
    Ok(())
}

pub fn cleanup() {}
