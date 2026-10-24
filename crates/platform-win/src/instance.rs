//! Single instance: a named mutex decides which process is the primary one, and
//! a named pipe lets later launches hand it a message ([`SHOW`] or
//! [`SETTINGS`], or a `spotlight://` link).
//!
//! Both names are per user and per Windows session, so two users on one
//! machine (or one user in two RDP sessions) each get their own launcher.

use std::thread;

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GENERIC_WRITE, GetLastError,
            HANDLE,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_NONE, OPEN_EXISTING,
            PIPE_ACCESS_INBOUND, ReadFile, WriteFile,
        },
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
            },
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{CreateMutexW, GetCurrentProcessId},
        },
        UI::WindowsAndMessaging::AllowSetForegroundWindow,
    },
    core::{HSTRING, Result},
};

/// Asks the primary instance to show its launcher window.
pub const SHOW: &str = "show";
/// Asks the primary instance to open its settings window.
pub const SETTINGS: &str = "settings";
/// Longer messages are cut off; this is plenty for a command or a deeplink.
const MAX_MESSAGE: usize = 4096;

pub enum Instance {
    /// This process owns the launcher. Keep the guard alive for the process lifetime.
    Primary(Guard),
    /// Another process already does.
    Secondary,
}

/// Holds the mutex; released when dropped (or when the process exits).
pub struct Guard(HANDLE);

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Decides whether this process is the primary instance.
///
/// If the mutex can't be created at all, this process acts as primary: a
/// launcher that runs twice is better than one that never starts.
pub fn acquire() -> Instance {
    let name = HSTRING::from(format!("Local\\{}", base_name()));
    unsafe {
        let mutex = CreateMutexW(None, false, &name);
        // Read right away: the last error is only meaningful directly after the call.
        let already_exists = GetLastError() == ERROR_ALREADY_EXISTS;
        match mutex {
            Ok(handle) if already_exists => {
                let _ = CloseHandle(handle);
                Instance::Secondary
            }
            Ok(handle) => Instance::Primary(Guard(handle)),
            Err(err) => {
                eprintln!("[instance] could not create the instance mutex: {err}");
                Instance::Primary(Guard(HANDLE::default()))
            }
        }
    }
}

/// Primary instance: starts a thread that receives messages from later
/// launches and forwards each one to `messages`.
pub fn listen(messages: async_channel::Sender<String>) {
    let name = pipe_name();
    let spawned = thread::Builder::new()
        .name("instance-pipe".into())
        .spawn(move || {
            // FIRST_PIPE_INSTANCE: fail instead of joining a pipe someone else created first.
            let mut first = true;
            loop {
                let pipe = match create_pipe(&name, first) {
                    Ok(pipe) => pipe,
                    Err(err) => {
                        eprintln!("[instance] could not create the instance pipe: {err}");
                        return;
                    }
                };
                first = false;
                let message = receive(pipe);
                unsafe {
                    let _ = DisconnectNamedPipe(pipe);
                    let _ = CloseHandle(pipe);
                }
                if let Some(message) = message
                    && messages.send_blocking(message).is_err()
                {
                    return;
                }
            }
        });
    if let Err(err) = spawned {
        eprintln!("[instance] could not start the pipe thread: {err}");
    }
}

/// Secondary instance: delivers `message` to the primary one.
///
/// Also lets the primary instance take the foreground: this process just got
/// user input (it was launched), the primary one didn't.
pub fn send(message: &str) -> Result<()> {
    unsafe {
        let pipe = CreateFileW(
            &pipe_name(),
            GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )?;
        let mut server_pid = 0;
        if GetNamedPipeServerProcessId(pipe, &mut server_pid).is_ok() {
            let _ = AllowSetForegroundWindow(server_pid);
        }
        let result = WriteFile(pipe, Some(message.as_bytes()), None, None);
        let _ = CloseHandle(pipe);
        result
    }
}

fn create_pipe(name: &HSTRING, first: bool) -> Result<HANDLE> {
    let mut open_mode = PIPE_ACCESS_INBOUND;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // The default security descriptor lets only this user (plus SYSTEM and
    // administrators) open the pipe for writing.
    let pipe = unsafe {
        CreateNamedPipeW(
            name,
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            MAX_MESSAGE as u32,
            0,
            None,
        )
    };
    if pipe.is_invalid() {
        return Err(windows::core::Error::from_win32());
    }
    Ok(pipe)
}

/// Waits for one client and reads its whole message.
fn receive(pipe: HANDLE) -> Option<String> {
    unsafe {
        // A client that connected between CreateNamedPipe and here reports PIPE_CONNECTED.
        if let Err(err) = ConnectNamedPipe(pipe, None)
            && err.code() != ERROR_PIPE_CONNECTED.to_hresult()
        {
            return None;
        }
        let mut message = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let mut read = 0;
            let ok = ReadFile(pipe, Some(&mut buf), Some(&mut read), None).is_ok();
            message.extend_from_slice(&buf[..read as usize]);
            if !ok || read == 0 || message.len() >= MAX_MESSAGE {
                break;
            }
        }
        message.truncate(MAX_MESSAGE);
        String::from_utf8(message).ok()
    }
}

/// `spotlight.<user>.<session>`, shared by the mutex and the pipe.
fn base_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let mut session = 0;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
    }
    // Pipe and mutex names can't contain backslashes; user names can't either,
    // but keep the name safe regardless.
    let user: String = user
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { '_' })
        .collect();
    format!("spotlight.{user}.{session}")
}

fn pipe_name() -> HSTRING {
    HSTRING::from(format!("\\\\.\\pipe\\{}", base_name()))
}
