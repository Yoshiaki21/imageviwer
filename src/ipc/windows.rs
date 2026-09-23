//! Windows single-instance control.
//!
//! A named mutex decides who is primary; the OS destroys it when the owning
//! process ends, so a crash never blocks the next launch. The winner serves a
//! named pipe that later launches write their path into.

use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use windows_sys::core::{BOOL, PCWSTR};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// Buffer sizes for the pipe. A path never approaches this.
const PIPE_BUFFER_SIZE: u32 = 64 * 1024;
const PIPE_DEFAULT_TIMEOUT_MS: u32 = 5_000;

pub enum Role {
    Primary(Primary),
    Secondary(Secondary),
}

pub struct Primary {
    /// Held for the lifetime of the process; releasing it would let a second
    /// viewer start.
    mutex: OwnedHandle,
    pipe_name: Vec<u16>,
}

pub struct Secondary {
    pipe_name: Vec<u16>,
}

pub fn acquire() -> Result<Role> {
    let mutex_name = wide(&format!(r"Local\imageviewer-{}", user_suffix()));
    let pipe_name = wide(&format!(r"\\.\pipe\imageviewer-{}", user_suffix()));

    // SAFETY: both pointers are valid, NUL-terminated wide strings that
    // outlive the call.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr() as PCWSTR) };
    if handle.is_null() {
        bail!("CreateMutexW failed: {}", last_error());
    }
    let mutex = OwnedHandle(handle);

    // SAFETY: no arguments; reads the calling thread's last error.
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_running {
        return Ok(Role::Secondary(Secondary { pipe_name }));
    }

    Ok(Role::Primary(Primary { mutex, pipe_name }))
}

impl Primary {
    pub fn serve(self, on_path: impl Fn(Option<PathBuf>) + Send + 'static) {
        let Primary { mutex, pipe_name } = self;

        std::thread::Builder::new()
            .name("imageviewer-ipc".into())
            .spawn(move || {
                // Moved into the thread so the mutex outlives `serve`.
                let _mutex = mutex;

                loop {
                    match accept_once(&pipe_name) {
                        Ok(path) => on_path(path),
                        Err(error) => {
                            log::error!("bad launch request: {error}");
                            // Avoid spinning if the pipe cannot be created at
                            // all; a failing launch is better than a busy loop.
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                    }
                }
            })
            .expect("spawning the IPC listener thread");
    }
}

impl Secondary {
    pub fn send(&self, path: Option<&Path>) -> Result<()> {
        // SAFETY: `pipe_name` is a valid NUL-terminated wide string.
        let handle = unsafe {
            CreateFileW(
                self.pipe_name.as_ptr() as PCWSTR,
                // `GENERIC_WRITE`, not `FILE_GENERIC_WRITE`: a named pipe's
                // security descriptor grants the generic right, and asking for
                // the expanded specific rights can come back ACCESS_DENIED.
                GENERIC_WRITE,
                FILE_SHARE_NONE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            bail!("cannot open the viewer's pipe: {}", last_error());
        }
        let pipe = OwnedHandle(handle);

        let payload = path.map(encode_path).unwrap_or_default();
        if payload.is_empty() {
            // An empty message still has to reach the primary, which learns
            // the request ended when the pipe closes below.
            return Ok(());
        }

        let mut written = 0u32;
        // SAFETY: the buffer outlives the call and its length fits in u32.
        let ok: BOOL = unsafe {
            WriteFile(
                pipe.0,
                payload.as_ptr(),
                payload.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            bail!("cannot send the path: {}", last_error());
        }
        Ok(())
    }
}

/// Creates one pipe instance, waits for a single launch request and returns
/// the path it carried.
fn accept_once(pipe_name: &[u16]) -> Result<Option<PathBuf>> {
    // SAFETY: `pipe_name` is a valid NUL-terminated wide string.
    let handle = unsafe {
        CreateNamedPipeW(
            pipe_name.as_ptr() as PCWSTR,
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_SIZE,
            PIPE_BUFFER_SIZE,
            PIPE_DEFAULT_TIMEOUT_MS,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        bail!("CreateNamedPipeW failed: {}", last_error());
    }
    let pipe = OwnedHandle(handle);

    // SAFETY: `pipe` is a valid pipe handle and the call is synchronous.
    let connected = unsafe { ConnectNamedPipe(pipe.0, std::ptr::null_mut()) };
    if connected == 0 {
        // A client that connected between creation and this call is already
        // attached, which Windows reports as an error.
        let error = unsafe { GetLastError() };
        if error != ERROR_PIPE_CONNECTED {
            bail!("ConnectNamedPipe failed: {error}");
        }
    }

    let bytes = read_to_end(&pipe);

    // SAFETY: `pipe` is a valid, connected pipe handle.
    unsafe { DisconnectNamedPipe(pipe.0) };

    Ok(decode_path(&bytes))
}

/// Reads until the client closes its end, which is how a request ends.
fn read_to_end(pipe: &OwnedHandle) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let mut read = 0u32;
        // SAFETY: `chunk` is a valid, writable buffer of the given length.
        let ok: BOOL = unsafe {
            ReadFile(
                pipe.0,
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            // A closed pipe is the normal end of a request.
            break;
        }
        bytes.extend_from_slice(&chunk[..read as usize]);
    }
    bytes
}

/// Paths travel as UTF-16, the encoding Windows itself uses, so nothing is
/// lost on the way.
fn encode_path(path: &Path) -> Vec<u8> {
    path.as_os_str()
        .encode_wide()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

fn decode_path(bytes: &[u8]) -> Option<PathBuf> {
    if bytes.len() < 2 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Some(PathBuf::from(std::ffi::OsString::from_wide(&units)))
}

/// A NUL-terminated wide string for the Win32 `*W` entry points.
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Keeps two accounts on one machine from sharing a mutex or a pipe.
fn user_suffix() -> String {
    std::env::var("USERNAME")
        .unwrap_or_else(|_| "default".to_string())
        .replace(['\\', '/', ':'], "_")
}

/// Closes its handle on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: the handle is owned by this value and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: a Win32 kernel handle is not bound to the thread that created it.
unsafe impl Send for OwnedHandle {}

fn last_error() -> u32 {
    // SAFETY: no arguments; reads the calling thread's last error.
    unsafe { GetLastError() }
}
