//! Run, as Windows' Run dialog (Win+R) does it: finding commands by name
//! (`gpedit` is `gpedit.msc`) and starting a command line.

use std::{env, fs, path::Path};

use spotlight_core::run;

use crate::{links, registry, system::Outcome};

/// Where apps register a name that runs them (`winword`, `chrome`).
const APP_PATHS: &[&str] = &[
    r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths",
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths",
];
/// Snap-ins and Control Panel applets run by name too.
const EXTRA_EXTENSIONS: &[&str] = &[".msc", ".cpl"];

/// A command that runs by name: `gpedit` → `gpedit.msc` at its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownCommand {
    /// The file name, as it's shown: `gpedit.msc`.
    pub file: String,
    pub path: String,
}

impl KnownCommand {
    /// The name without its extension, lowercase: what's typed to find it.
    pub fn name(&self) -> String {
        let lower = self.file.to_lowercase();
        match lower.rsplit_once('.') {
            Some((stem, _)) => stem.to_owned(),
            None => lower,
        }
    }
}

/// Every command that runs by name: programs in the PATH folders with the
/// PATHEXT extensions (and `.msc`, `.cpl`), then the App Paths names. The
/// first found of a name wins, as on the command line. Blocking (reads a few
/// folders and registry keys).
pub fn known_commands() -> Vec<KnownCommand> {
    let extensions: Vec<String> = env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .map(|extension| extension.trim().to_lowercase())
        .filter(|extension| extension.starts_with('.') && extension != ".js" && extension != ".vbs")
        .chain(
            EXTRA_EXTENSIONS
                .iter()
                .map(|&extension| extension.to_owned()),
        )
        .collect();
    let mut found: Vec<KnownCommand> = Vec::new();
    let mut add = |command: KnownCommand| {
        if !found
            .iter()
            .any(|known| known.file.eq_ignore_ascii_case(&command.file))
        {
            found.push(command);
        }
    };
    let folders = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    for folder in folders {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            let lower = file.to_lowercase();
            if !extensions
                .iter()
                .any(|extension| lower.ends_with(extension.as_str()))
            {
                continue;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                add(KnownCommand {
                    file,
                    path: entry.path().to_string_lossy().into_owned(),
                });
            }
        }
    }
    for key in APP_PATHS {
        for file in registry::subkeys(key) {
            let Some(path) = registry::string(&format!(r"{key}\{file}"), "") else {
                continue;
            };
            let path = links::expand_env(path.trim().trim_matches('"'));
            if Path::new(&path).is_file() {
                add(KnownCommand { file, path });
            }
        }
    }
    found
}

/// Starts `line` as the Run dialog would: environment variables expanded,
/// the program (or file, folder, URL) split from its arguments; elevated
/// after the UAC prompt with `admin`. Starts in the user's profile folder.
pub fn start(line: &str, admin: bool) -> Outcome {
    let line = links::expand_env(line.trim());
    if line.is_empty() {
        return Err("Type a command to run".into());
    }
    let (program, args) = run::split(&line, |path| Path::new(path).exists());
    let verb = if admin { "runas" } else { "open" };
    let home = env::var("USERPROFILE").unwrap_or_default();
    crate::commands::shell_execute_in(verb, program, args, &home).map_err(|err| {
        if err.contains("file is gone") {
            format!("Windows can't find “{program}”")
        } else {
            err
        }
    })
}
