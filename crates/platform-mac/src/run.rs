//! macOS counterpart of `loupe_platform_win::run`, with its signatures.
//! Commands are found in the PATH folders; starting them is a stub for now.

use std::{env, fs};

use crate::system::Outcome;

use crate::UNSUPPORTED;

/// A command that runs by name: `gpedit` → `gpedit.msc` at its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownCommand {
    /// The file name, as it's shown: `gpedit.msc`.
    pub file: String,
    pub path: String,
}

impl KnownCommand {
    /// The name, lowercase: what's typed to find it. Unlike Windows, the
    /// whole name: a dot there is part of it (`python3.12`), not an extension.
    pub fn name(&self) -> String {
        self.file.to_lowercase()
    }
}

/// Every command that runs by name: executable files in the PATH folders.
/// The first found of a name wins, as in the shell. Blocking (reads a few
/// folders).
pub fn known_commands() -> Vec<KnownCommand> {
    let mut found: Vec<KnownCommand> = Vec::new();
    let folders = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    for folder in folders {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            // Followed: Homebrew's commands are links into its cellar.
            let Ok(meta) = fs::metadata(entry.path()) else {
                continue;
            };
            if !meta.is_file() || !executable(&meta) {
                continue;
            }
            let file = entry.file_name().to_string_lossy().into_owned();
            if !found.iter().any(|known| known.file == file) {
                found.push(KnownCommand {
                    file,
                    path: entry.path().to_string_lossy().into_owned(),
                });
            }
        }
    }
    found
}

#[cfg(unix)]
fn executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &fs::Metadata) -> bool {
    false
}

/// Starts `line` as the Run dialog would: environment variables expanded,
/// the program (or file, folder, URL) split from its arguments; elevated
/// after the UAC prompt with `admin`. Starts in the user's profile folder.
pub fn start(_line: &str, _admin: bool) -> Outcome {
    Err(UNSUPPORTED.into())
}
