//! Installed apps, Win32 and MSIX alike, from the shell's `AppsFolder`.

use spotlight_core::{
    item::{Command, Icon, SearchItem, shortcut},
    item_id,
    provider::Provider,
    view::{DetailView, ListView, View},
};
use spotlight_platform_win::apps;

use crate::glyph;

pub const ID: &str = "apps";

pub struct Apps;

impl Provider for Apps {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Applications"
    }

    fn description(&self) -> &'static str {
        "Installed apps, desktop and Microsoft Store alike."
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
        let mut items: Vec<SearchItem> = apps.into_iter().map(item).collect();
        items.push(browse(&items));
        items
    }
}

/// "Browse Apps": every app as a tile with a large icon.
fn browse(apps: &[SearchItem]) -> SearchItem {
    let grid = View::Grid(ListView {
        title: "Apps".into(),
        placeholder: Some("Search apps…".into()),
        items: apps.to_vec(),
        empty: Some("No apps found".into()),
        filters: Vec::new(),
        live: false,
    });
    SearchItem::new(item_id(ID, "browse"), "Browse Apps")
        .subtitle("Applications")
        .keywords(&["all apps", "app grid"])
        .icon(Icon::Glyph(glyph::APPS))
        .action("Browse Apps", Command::PushView(Box::new(grid)))
}

/// Where Windows lists installed apps with their Uninstall buttons.
const INSTALLED_APPS: &str = "ms-settings:appsfeatures";

fn item(app: apps::AppEntry) -> SearchItem {
    // Same id as before providers existed, so recorded frecency still applies.
    let mut item = SearchItem::new(item_id(ID, &app.launch_id), app.name)
        .subtitle("Application")
        .icon(Icon::App(app.launch_id.clone()))
        .action("Open", Command::LaunchApp(app.launch_id.clone()));
    if let Some(path) = app.path {
        if can_elevate(&path) {
            item = item.action("Run as Administrator", Command::RunAsAdmin(path.clone()));
        }
        item = item
            .action("Show in Explorer", Command::ShowInFolder(path.clone()))
            .action("Copy Path", Command::Copy(path))
            .shortcut(shortcut::COPY_TERTIARY);
    }
    let item = item.action("Uninstall…", Command::OpenUrl(INSTALLED_APPS.to_owned()));
    let details = details(&item, &app.launch_id);
    item.action("Show Details", Command::PushView(Box::new(details)))
        .shortcut(shortcut::INFO)
}

/// What there is to know about an app, with the same actions as its row.
fn details(item: &SearchItem, launch_id: &str) -> View {
    let path = item
        .actions
        .iter()
        .find_map(|action| match &action.command {
            Command::ShowInFolder(path) => Some(path.clone()),
            _ => None,
        });
    let (kind, markdown) = match &path {
        Some(_) => (
            "Desktop app",
            "A classic Windows program. **Enter** opens it; **Ctrl+K** has the rest: \
             run it as administrator, show it in Explorer, copy its path.",
        ),
        None if launch_id.contains('!') => (
            "Microsoft Store app",
            "A packaged app from the Microsoft Store or Windows itself. **Enter** opens it; \
             uninstall it from *Installed apps* in Settings.",
        ),
        None => (
            "App",
            "Registered by name only, so there is no file to show. **Enter** opens it.",
        ),
    };
    let mut metadata = vec![("Kind".to_owned(), kind.to_owned())];
    if let Some(path) = path {
        metadata.push(("Location".to_owned(), path));
    }
    metadata.push(("App ID".to_owned(), launch_id.to_owned()));
    View::Detail(DetailView {
        title: item.title.clone(),
        markdown: markdown.to_owned(),
        metadata,
        actions: item.actions.clone(),
    })
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
                "Uninstall…",
                "Show Details"
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
        assert_eq!(titles(&item(app)), ["Open", "Uninstall…", "Show Details"]);
    }

    #[test]
    fn documents_are_not_run_as_admin() {
        assert!(can_elevate("C:\\x\\Setup.EXE"));
        assert!(!can_elevate("C:\\x\\readme.txt"));
    }
}
