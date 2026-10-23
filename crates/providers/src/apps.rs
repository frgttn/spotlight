//! Installed apps, Win32 and MSIX alike, from the shell's `AppsFolder`.

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};
use spotlight_platform_win::apps;

pub const ID: &str = "apps";

pub struct Apps;

impl Provider for Apps {
    fn id(&self) -> &'static str {
        ID
    }

    /// Blocking and COM-heavy (~200 ms): the shell enumerates every app.
    fn catalog(&self) -> Vec<SearchItem> {
        let apps = match apps::discover() {
            Ok(apps) => apps,
            Err(err) => {
                eprintln!("[apps] discovery failed: {err}");
                return Vec::new();
            }
        };
        apps.into_iter().map(item).collect()
    }
}

/// Where Windows lists installed apps with their Uninstall buttons.
const INSTALLED_APPS: &str = "ms-settings:appsfeatures";

fn item(app: apps::AppEntry) -> SearchItem {
    // Same id as before providers existed, so recorded frecency still applies.
    let mut item = SearchItem::new(item_id(ID, &app.launch_id), app.name)
        .subtitle("Application")
        .icon(Icon::App(app.launch_id.clone()))
        .action("Open", Command::LaunchApp(app.launch_id));
    if let Some(path) = app.path {
        if can_elevate(&path) {
            item = item.action("Run as Administrator", Command::RunAsAdmin(path.clone()));
        }
        item = item
            .action("Show in Explorer", Command::ShowInFolder(path.clone()))
            .action("Copy Path", Command::Copy(path));
    }
    item.action("Uninstall…", Command::OpenUrl(INSTALLED_APPS.to_owned()))
}

/// Files the shell's "runas" verb can start elevated.
fn can_elevate(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".exe", ".lnk", ".bat", ".cmd", ".msc"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(item: &SearchItem) -> Vec<&str> {
        item.actions
            .iter()
            .map(|action| action.title.as_str())
            .collect()
    }

    #[test]
    fn win32_apps_get_file_actions() {
        let app = apps::AppEntry {
            name: "Tool".into(),
            launch_id: "C:\\Tools\\tool.exe".into(),
            path: Some("C:\\Tools\\tool.exe".into()),
        };
        assert_eq!(
            titles(&item(app)),
            [
                "Open",
                "Run as Administrator",
                "Show in Explorer",
                "Copy Path",
                "Uninstall…"
            ]
        );
    }

    #[test]
    fn packaged_apps_get_only_open_and_uninstall() {
        let app = apps::AppEntry {
            name: "Calculator".into(),
            launch_id: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(),
            path: None,
        };
        assert_eq!(titles(&item(app)), ["Open", "Uninstall…"]);
    }

    #[test]
    fn documents_are_not_run_as_admin() {
        assert!(can_elevate("C:\\x\\Setup.EXE"));
        assert!(!can_elevate("C:\\x\\readme.txt"));
    }
}
