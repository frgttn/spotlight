//! Runs the [`Command`]s that search items carry, except `Copy` (the UI
//! framework owns the clipboard) and `OpenCalculatorHistory` (a UI screen),
//! which the UI handles.

use std::{os::windows::process::CommandExt, process, thread};

use spotlight_core::item::Command;
use windows::{
    Win32::{
        System::{
            Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx},
            Threading::CREATE_NO_WINDOW,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::{HSTRING, PCWSTR},
};

use crate::system;

/// What `OpenUrl` may open. `ShellExecute` would just as happily run a
/// program, so anything else is refused.
const URL_SCHEMES: &[&str] = &["https://", "http://", "ms-settings:"];

/// Starts `command` and returns at once; anything slow runs on its own thread.
pub fn execute(command: &Command) {
    match command {
        Command::LaunchApp(launch_id) => {
            shell_execute("open", format!("shell:AppsFolder\\{launch_id}"))
        }
        Command::RunAsAdmin(path) => shell_execute("runas", path.clone()),
        Command::ShowInFolder(path) => {
            // `/select,` takes the rest of the argument as the path, spaces included.
            let spawned = process::Command::new("explorer.exe")
                .raw_arg(format!("/select,\"{path}\""))
                .creation_flags(CREATE_NO_WINDOW.0)
                .spawn();
            if let Err(err) = spawned {
                eprintln!("[commands] could not show {path:?} in Explorer: {err}");
            }
        }
        Command::OpenUrl(url) => {
            let lower = url.to_ascii_lowercase();
            if URL_SCHEMES.iter().any(|scheme| lower.starts_with(scheme)) {
                shell_execute("open", url.clone());
            } else {
                eprintln!("[commands] refusing to open {url:?}: not an allowed URL");
            }
        }
        Command::Copy(_) | Command::OpenCalculatorHistory => {
            eprintln!("[commands] {command:?} must be handled by the UI")
        }
        Command::System(command) => system::run(*command),
    }
}

/// `ShellExecute` with `verb` on a short-lived thread, so a slow shell (or a
/// UAC prompt) never blocks the UI.
fn shell_execute(verb: &'static str, target: String) {
    thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let result = ShellExecuteW(
            None,
            &HSTRING::from(verb),
            &HSTRING::from(target.as_str()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        // ShellExecute reports success as a value greater than 32. Declining
        // the UAC prompt also ends up here (code 5, access denied).
        if (result.0 as isize) <= 32 {
            eprintln!(
                "[commands] {verb} {target:?} failed (code {})",
                result.0 as isize
            );
        }
    });
}
