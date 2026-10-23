//! What quicklinks need from Windows: the current moment for `{date}`,
//! `{time}` and `{uuid}`, environment variables in paths, and the installed
//! browsers to open a link with.

use std::{env, path::Path};

use spotlight_core::quicklink::Moment;
use windows::{Win32::System::SystemInformation::GetLocalTime, core::GUID};

use crate::registry;

/// Where browsers register themselves for Default Apps, per user and per machine.
const BROWSER_KEYS: &[&str] = &[
    r"HKCU\SOFTWARE\Clients\StartMenuInternet",
    r"HKLM\SOFTWARE\Clients\StartMenuInternet",
];

/// The local date and time, and a fresh random UUID.
pub fn moment() -> Moment {
    let now = unsafe { GetLocalTime() };
    Moment {
        date: format!("{:04}-{:02}-{:02}", now.wYear, now.wMonth, now.wDay),
        time: format!("{:02}:{:02}", now.wHour, now.wMinute),
        uuid: GUID::new()
            .map(|guid| format!("{guid:?}").to_lowercase())
            .unwrap_or_default(),
    }
}

/// `%USERPROFILE%\Desktop` → `C:\Users\me\Desktop`. Unknown variables stay as
/// they are.
pub fn expand_env(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => match env::var(&after[..end]) {
                Ok(value) => {
                    out.push_str(&value);
                    rest = &after[end + 1..];
                }
                Err(_) => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// An installed web browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Browser {
    pub name: String,
    pub exe: String,
}

/// The browsers registered on this machine, by name, each once. Blocking
/// (a few registry reads).
pub fn browsers() -> Vec<Browser> {
    let mut found: Vec<Browser> = Vec::new();
    for root in BROWSER_KEYS {
        for key in registry::subkeys(root) {
            let path = format!(r"{root}\{key}");
            let Some(exe) = registry::string(&format!(r"{path}\shell\open\command"), "")
                .and_then(|command| program(&command))
            else {
                continue;
            };
            if !Path::new(&exe).exists()
                || found
                    .iter()
                    .any(|known| known.exe.eq_ignore_ascii_case(&exe))
            {
                continue;
            }
            let name = registry::string(&path, "")
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(key);
            found.push(Browser { name, exe });
        }
    }
    found.sort_by_cached_key(|browser| browser.name.to_lowercase());
    found
}

/// The program of a shell command line: `"C:\x\chrome.exe" --flag` → `C:\x\chrome.exe`.
fn program(command: &str) -> Option<String> {
    let command = command.trim();
    let exe = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => {
            // Unquoted: up to `.exe`, spaces included (`C:\Program Files\…`).
            let end = command.to_ascii_lowercase().find(".exe")? + ".exe".len();
            &command[..end]
        }
    };
    (!exe.is_empty()).then(|| exe.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_moment() {
        let moment = moment();
        assert_eq!(moment.date.len(), "2026-10-23".len());
        assert_eq!(moment.time.len(), "14:05".len());
        assert_eq!(moment.uuid.len(), 36);
        assert_ne!(moment.uuid, super::moment().uuid);
    }

    #[test]
    fn expands_environment_variables() {
        let home = env::var("USERPROFILE").unwrap();
        assert_eq!(
            expand_env(r"%USERPROFILE%\Desktop"),
            format!(r"{home}\Desktop")
        );
        assert_eq!(
            expand_env("100% sure %NO_SUCH_VAR_X%"),
            "100% sure %NO_SUCH_VAR_X%"
        );
        assert_eq!(expand_env("%%"), "%%");
    }

    #[test]
    fn finds_programs_in_command_lines() {
        assert_eq!(
            program(r#""C:\Program Files\Mozilla Firefox\firefox.exe" -osint -url "%1""#)
                .as_deref(),
            Some(r"C:\Program Files\Mozilla Firefox\firefox.exe")
        );
        assert_eq!(
            program(r"C:\Program Files\Browser\browser.exe --x").as_deref(),
            Some(r"C:\Program Files\Browser\browser.exe")
        );
        assert_eq!(program(""), None);
    }

    #[test]
    fn lists_browsers() {
        // Every Windows install registers Edge.
        let browsers = browsers();
        eprintln!("{browsers:?}");
        assert!(!browsers.is_empty());
    }
}
