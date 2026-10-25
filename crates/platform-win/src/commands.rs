//! Runs the [`Command`]s that search items carry, except `Copy` and the
//! clipboard history's (the UI owns the clipboard and the database) and the
//! ones that open views, which the UI handles.

use std::{os::windows::process::CommandExt, process, thread};

use spotlight_core::{item::Command, quicklink};
use windows::{
    Win32::{
        System::{
            Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx},
            Threading::{CREATE_NEW_CONSOLE, CREATE_NO_WINDOW},
        },
        UI::{
            Shell::{
                FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
                SHFILEOPSTRUCTW, SHFileOperationW, ShellExecuteW,
            },
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{HSTRING, PCWSTR},
};

use crate::{
    links, run,
    system::{self, Outcome},
    wm,
};

/// What `OpenUrl` may open. `ShellExecute` would just as happily run a
/// program, so anything else is refused.
const URL_SCHEMES: &[&str] = &[
    "https://",
    "http://",
    "ms-settings:",
    // Game launchers, to start a game they installed.
    "steam://",
    "com.epicgames.launcher://",
    "uplay://",
];

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
        Command::Open(target) => {
            // ShellExecute takes `::{…}` parsing names with a `shell:` in front.
            let target = if target.starts_with("::") {
                format!("shell:{target}")
            } else {
                target.clone()
            };
            shell_execute("open", &target)
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
        Command::OpenWith(path) => process::Command::new("rundll32.exe")
            .raw_arg(format!("shell32.dll,OpenAs_RunDLL {path}"))
            .spawn()
            .map(|_| None)
            .map_err(|err| format!("Couldn't ask which app to use: {err}")),
        Command::MoveToTrash(path) => move_to_trash(path),
        Command::OpenTerminal(dir) => open_terminal(dir),
        Command::OpenUrl(url) => {
            let lower = url.to_ascii_lowercase();
            if URL_SCHEMES.iter().any(|scheme| lower.starts_with(scheme)) {
                shell_execute("open", url)
            } else {
                Err(format!("Not a web address: {url}"))
            }
        }
        Command::OpenQuicklink { link, app } => {
            let link = quicklink::expand_moment(link.trim(), &links::moment());
            // Paths can use `%USERPROFILE%`; URLs keep their `%20`s.
            let link = if quicklink::is_url(&link) {
                link
            } else {
                links::expand_env(&link)
            };
            match app {
                Some(app) => shell_execute_with("open", app, &format!("\"{link}\"")),
                None => shell_execute("open", &link),
            }
        }
        Command::Copy(_)
        | Command::CopyFile(_)
        | Command::OpenCalculatorHistory
        | Command::PushView(_)
        | Command::SaveQuicklink(_)
        | Command::DeleteQuicklink(_)
        | Command::SetSearchEngine { .. }
        | Command::SaveSnippet(_)
        | Command::DeleteSnippet(_)
        | Command::PasteSnippet { .. }
        | Command::ImportSnippets
        | Command::ExportSnippets
        | Command::ImportSnippetsJson(_)
        | Command::ImportQuicklinks
        | Command::ExportQuicklinks
        | Command::ImportQuicklinksJson(_)
        | Command::OpenProviderView { .. }
        | Command::SearchIn { .. }
        | Command::ProviderAction { .. }
        | Command::RestoreClip { .. }
        | Command::PinClip { .. }
        | Command::DeleteClip(_)
        | Command::ClearClipboardHistory
        | Command::IgnoreClipApp(_)
        | Command::Complete(_) => Err(format!("{command:?} must be handled by the UI")),
        Command::System(command) => system::run_blocking(*command),
        Command::Window {
            target: Some(target),
            command,
        } => wm::run(*target, *command),
        Command::Window { target: None, .. } => {
            Err("No window to arrange: the UI fills in the target".into())
        }
        Command::SetWindowFrame {
            target,
            x,
            y,
            width,
            height,
        } => wm::set_frame_from(*target, [x, y, width, height]),
        Command::Run { line, admin } => run::start(line, *admin),
        Command::QuitApp(pid) => wm::quit_app(*pid),
        Command::ForceQuit(pid) => wm::force_quit(*pid),
    }
}

/// `ShellExecute` with `verb`, blocking.
fn shell_execute(verb: &str, target: &str) -> Outcome {
    shell_execute_with(verb, target, "")
}

/// `ShellExecute` with `verb` and command-line `parameters` for a program, blocking.
fn shell_execute_with(verb: &str, target: &str, parameters: &str) -> Outcome {
    shell_execute_in(verb, target, parameters, "")
}

/// [`shell_execute_with`], started in folder `dir` (empty: ours).
pub(crate) fn shell_execute_in(verb: &str, target: &str, parameters: &str, dir: &str) -> Outcome {
    let parameters = HSTRING::from(parameters);
    let dir = HSTRING::from(dir);
    let optional = |text: &HSTRING| {
        if text.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(text.as_ptr())
        }
    };
    let code = unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        ShellExecuteW(
            None,
            &HSTRING::from(verb),
            &HSTRING::from(target),
            optional(&parameters),
            optional(&dir),
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

/// Opens Windows Terminal in `dir`, or PowerShell where it isn't installed.
fn open_terminal(dir: &str) -> Outcome {
    if !std::path::Path::new(dir).is_dir() {
        return Err("Couldn't open a terminal: the folder is gone".into());
    }
    let terminal = process::Command::new("wt.exe").arg("-d").arg(dir).spawn();
    if terminal.is_ok() {
        return Ok(None);
    }
    process::Command::new("powershell.exe")
        .arg("-NoExit")
        .current_dir(dir)
        .creation_flags(CREATE_NEW_CONSOLE.0)
        .spawn()
        .map(|_| None)
        .map_err(|err| format!("Couldn't open a terminal: {err}"))
}

/// Moves `path` to the Recycle Bin, without Explorer's own confirmation (the
/// launcher asked for a second Enter) and without its progress window.
fn move_to_trash(path: &str) -> Outcome {
    // A list of paths, each ending in a NUL, the list in a second one.
    let from: Vec<u16> = path.encode_utf16().chain([0, 0]).collect();
    let mut operation = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    let code = unsafe { SHFileOperationW(&mut operation) };
    if code != 0 {
        return Err(format!(
            "Couldn't move it to the Recycle Bin (error {code:#x})"
        ));
    }
    if operation.fAnyOperationsAborted.as_bool() {
        return Err("Moving it to the Recycle Bin was stopped".into());
    }
    Ok(Some("Moved to Recycle Bin".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_a_file_to_the_recycle_bin() {
        let path = std::env::temp_dir().join(format!("spotlight-trash-{}.txt", std::process::id()));
        std::fs::write(&path, "x").unwrap();
        let outcome = run_blocking(&Command::MoveToTrash(path.to_string_lossy().into_owned()));
        assert_eq!(outcome, Ok(Some("Moved to Recycle Bin".into())));
        assert!(!path.exists());
    }

    #[test]
    fn reports_a_missing_file() {
        let outcome = run_blocking(&Command::RunAsAdmin(
            r"C:\no such folder\no such program.exe".into(),
        ));
        assert_eq!(outcome, Err("Couldn't open it: the file is gone".into()));
    }

    /// Opens Control Panel's Device Manager; close it afterwards.
    #[test]
    #[ignore = "opens a window"]
    fn opens_a_control_panel_item() {
        let item =
            r"::{26EE0668-A00A-44D7-9371-BEB064C98683}\0\::{74246BFC-4C96-11D0-ABEF-0020AF6B0B7A}";
        assert_eq!(run_blocking(&Command::Open(item.into())), Ok(None));
    }

    #[test]
    fn refuses_other_schemes() {
        let outcome = run_blocking(&Command::OpenUrl("file:///C:/Windows/notepad.exe".into()));
        assert!(outcome.is_err());
    }
}
