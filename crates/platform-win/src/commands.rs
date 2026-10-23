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

use crate::system::{self, Outcome};

/// What `OpenUrl` may open. `ShellExecute` would just as happily run a
/// program, so anything else is refused.
const URL_SCHEMES: &[&str] = &["https://", "http://", "ms-settings:"];

/// Runs `command` on a thread of its own (a slow shell, a UAC prompt or a
/// system command never blocks the UI). Its outcome, a message for the HUD
/// or why it failed, arrives on the returned channel.
pub fn spawn(command: Command) -> async_channel::Receiver<Outcome> {
    if let Command::System(command) = command {
        return system::spawn(command);
    }
    let (tx, rx) = async_channel::bounded(1);
    thread::spawn(move || {
        let outcome = run_blocking(&command);
        if let Err(err) = &outcome {
            eprintln!("[commands] {command:?} failed: {err}");
        }
        let _ = tx.send_blocking(outcome);
    });
    rx
}

fn run_blocking(command: &Command) -> Outcome {
    match command {
        Command::LaunchApp(launch_id) => {
            shell_execute("open", &format!("shell:AppsFolder\\{launch_id}"))
        }
        Command::RunAsAdmin(path) => shell_execute("runas", path),
        Command::ShowInFolder(path) => {
            // `/select,` takes the rest of the argument as the path, spaces included.
            process::Command::new("explorer.exe")
                .raw_arg(format!("/select,\"{path}\""))
                .creation_flags(CREATE_NO_WINDOW.0)
                .spawn()
                .map(|_| None)
                .map_err(|err| format!("Couldn't open Explorer: {err}"))
        }
        Command::OpenUrl(url) => {
            let lower = url.to_ascii_lowercase();
            if URL_SCHEMES.iter().any(|scheme| lower.starts_with(scheme)) {
                shell_execute("open", url)
            } else {
                Err(format!("Not a web address: {url}"))
            }
        }
        Command::Copy(_) | Command::OpenCalculatorHistory => {
            Err(format!("{command:?} must be handled by the UI"))
        }
        Command::System(command) => system::run_blocking(*command),
    }
}

/// `ShellExecute` with `verb`, blocking.
fn shell_execute(verb: &str, target: &str) -> Outcome {
    let code = unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        ShellExecuteW(
            None,
            &HSTRING::from(verb),
            &HSTRING::from(target),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
        .0 as isize
    };
    // Success is any value above 32; below are the old SE_ERR_* codes.
    match code {
        33.. => Ok(None),
        // Declining the UAC prompt: the user said no, nothing to report.
        5 if verb == "runas" => Ok(None),
        2 | 3 => Err("Couldn't open it: the file is gone".into()),
        5 => Err("Couldn't open it: access denied".into()),
        8 => Err("Couldn't open it: not enough memory".into()),
        26 | 32 => Err("Couldn't open it: the file is in use".into()),
        27 | 31 => Err("Couldn't open it: no app is set to open this".into()),
        code => Err(format!("Couldn't open it (error {code})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_missing_file() {
        let outcome = run_blocking(&Command::RunAsAdmin(
            r"C:\no such folder\no such program.exe".into(),
        ));
        assert_eq!(outcome, Err("Couldn't open it: the file is gone".into()));
    }

    #[test]
    fn refuses_other_schemes() {
        let outcome = run_blocking(&Command::OpenUrl("file:///C:/Windows/notepad.exe".into()));
        assert!(outcome.is_err());
    }
}
