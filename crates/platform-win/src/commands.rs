//! Runs the [`Command`]s that search items carry. `Copy` is the exception: the
//! UI framework owns the clipboard, so the UI handles it.

use std::thread;

use spotlight_core::item::Command;
use windows::{
    Win32::{
        System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx},
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::system;

/// Starts `command` and returns at once; anything slow runs on its own thread.
pub fn execute(command: &Command) {
    match command {
        Command::LaunchApp(launch_id) => shell_open(format!("shell:AppsFolder\\{launch_id}")),
        Command::OpenUrl(url) => {
            // Only web URLs: `ShellExecute` would just as happily run a program.
            let lower = url.to_ascii_lowercase();
            if lower.starts_with("https://") || lower.starts_with("http://") {
                shell_open(url.clone());
            } else {
                eprintln!("[commands] refusing to open {url:?}: not an http(s) URL");
            }
        }
        Command::Copy(_) => eprintln!("[commands] Copy must be handled by the UI"),
        Command::System(command) => system::run(*command),
    }
}

/// `ShellExecute` "open" on a short-lived thread, so a slow shell never blocks the UI.
fn shell_open(target: String) {
    thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let result = ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target.as_str()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        // ShellExecute reports success as a value greater than 32.
        if (result.0 as isize) <= 32 {
            eprintln!(
                "[commands] failed to open {target:?} (code {})",
                result.0 as isize
            );
        }
    });
}
