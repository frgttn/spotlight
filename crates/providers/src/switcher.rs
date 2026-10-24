//! Switch Windows and Search Processes: the windows that are open, to go to,
//! close or quit; and this session's processes, to end.

use std::path::Path;

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
    view::{ListFilter, ListView, View},
    window::WindowCommand,
};
use spotlight_platform_win::wm::{self, ProcessInfo, WindowInfo};

use crate::glyph;

pub const ID: &str = "switcher";
const WINDOWS_VIEW: &str = "windows";
const PROCESSES_VIEW: &str = "processes";
/// Keyword of processes that have windows, for the list's filter.
const HAS_WINDOWS: &str = "has-windows";

pub struct Switcher;

impl Provider for Switcher {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Switch Windows"
    }

    fn description(&self) -> &'static str {
        "Go to any open window, close it or quit its app; end processes."
    }

    fn catalog(&self) -> Vec<SearchItem> {
        let open = |view: &str| Command::OpenProviderView {
            provider: ID.into(),
            view: view.into(),
        };
        vec![
            SearchItem::new(item_id(ID, WINDOWS_VIEW), "Switch Windows")
                .subtitle("Switch Windows")
                .keywords(&["window switcher", "alt tab", "task view"])
                .icon(Icon::Glyph(glyph::SWITCH))
                .action("Switch Windows", open(WINDOWS_VIEW)),
            SearchItem::new(item_id(ID, PROCESSES_VIEW), "Search Processes")
                .subtitle("Switch Windows")
                .keywords(&["kill process", "end task", "task manager", "force quit"])
                .icon(Icon::Glyph(glyph::PROCESSES))
                .action("Search Processes", open(PROCESSES_VIEW)),
        ]
    }

    fn view(&self, id: &str) -> Option<View> {
        match id {
            WINDOWS_VIEW => Some(View::List(windows_view(wm::windows()))),
            PROCESSES_VIEW => Some(View::List(processes_view(wm::processes()))),
            _ => None,
        }
    }
}

fn windows_view(windows: Vec<WindowInfo>) -> ListView {
    ListView {
        title: "Switch Windows".into(),
        placeholder: Some("Search windows by title or app…".into()),
        items: windows.into_iter().map(window_item).collect(),
        empty: Some("No windows are open".into()),
        filters: Vec::new(),
        live: false,
    }
}

fn window_item(window: WindowInfo) -> SearchItem {
    let on = |command| Command::Window {
        target: Some(window.hwnd),
        command,
    };
    let app = if window.app.is_empty() {
        "App".to_owned()
    } else {
        window.app.clone()
    };
    let mut subtitle = app.clone();
    if window.minimized {
        subtitle.push_str("  ·  Minimized");
    }
    let mut item = SearchItem::new(
        item_id(ID, &format!("window-{}", window.hwnd)),
        &window.title,
    )
    .subtitle(subtitle)
    .icon(
        window
            .exe
            .clone()
            .map_or(Icon::Glyph(glyph::WINDOWS), Icon::Shell),
    )
    .action("Switch to Window", on(WindowCommand::Focus))
    .action("Close Window", on(WindowCommand::Close))
    .action("Minimize Window", on(WindowCommand::Minimize))
    .action(format!("Quit {app}"), Command::QuitApp(window.pid))
    .action(format!("Force Quit {app}"), Command::ForceQuit(window.pid));
    item.keywords = std::iter::once(window.app)
        .chain(window.exe.as_deref().and_then(file_name))
        .filter(|word| !word.is_empty())
        .collect();
    item
}

fn processes_view(processes: Vec<ProcessInfo>) -> ListView {
    ListView {
        title: "Search Processes".into(),
        placeholder: Some("Search processes by name…".into()),
        items: processes.into_iter().map(process_item).collect(),
        empty: Some("No processes could be read".into()),
        filters: vec![
            ListFilter::new("All", None),
            ListFilter::new("With Windows", Some(HAS_WINDOWS)),
        ],
        live: false,
    }
}

fn process_item(process: ProcessInfo) -> SearchItem {
    let mut subtitle = format!("PID {}", process.pid);
    if process.memory > 0 {
        subtitle.push_str(&format!("  ·  {}", memory(process.memory)));
    }
    match process.windows {
        0 => {}
        1 => subtitle.push_str("  ·  1 window"),
        count => subtitle.push_str(&format!("  ·  {count} windows")),
    }
    let mut item = SearchItem::new(
        item_id(ID, &format!("process-{}", process.pid)),
        &process.name,
    )
    .subtitle(subtitle)
    .icon(
        process
            .exe
            .clone()
            .map_or(Icon::Glyph(glyph::PROCESSES), Icon::Shell),
    )
    .action("Force Quit Process", Command::ForceQuit(process.pid));
    if process.windows > 0 {
        item = item.action("Quit Process", Command::QuitApp(process.pid));
        item.keywords.push(HAS_WINDOWS.into());
    }
    if let Some(exe) = process.exe {
        item = item
            .action("Show in Folder", Command::ShowInFolder(exe.clone()))
            .action("Copy Path", Command::Copy(exe));
    }
    item
}

/// `chrome.exe` from a path.
fn file_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// `812 MB`, `1.4 GB`.
fn memory(bytes: u64) -> String {
    let mb = bytes as f64 / (1024. * 1024.);
    if mb >= 1024. {
        format!("{:.1} GB", mb / 1024.)
    } else {
        format!("{mb:.0} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_window_and_process_rows() {
        let window = window_item(WindowInfo {
            hwnd: 0x1234,
            title: "README.md - Visual Studio Code".into(),
            pid: 42,
            exe: Some(r"C:\Apps\Code\Code.exe".into()),
            app: "Visual Studio Code".into(),
            minimized: true,
        });
        assert_eq!(
            window.subtitle.as_deref(),
            Some("Visual Studio Code  ·  Minimized")
        );
        assert_eq!(window.keywords, ["Visual Studio Code", "Code.exe"]);
        assert_eq!(
            window.primary_action().unwrap().command,
            Command::Window {
                target: Some(0x1234),
                command: WindowCommand::Focus
            }
        );
        assert!(
            window
                .actions
                .iter()
                .any(|action| action.title == "Force Quit Visual Studio Code")
        );

        let process = process_item(ProcessInfo {
            pid: 7,
            name: "chrome.exe".into(),
            exe: None,
            memory: 1536 * 1024 * 1024,
            windows: 2,
        });
        assert_eq!(
            process.subtitle.as_deref(),
            Some("PID 7  ·  1.5 GB  ·  2 windows")
        );
        assert!(
            process
                .primary_action()
                .unwrap()
                .command
                .needs_confirmation()
        );
        assert!(process.keywords.contains(&HAS_WINDOWS.to_owned()));
    }
}
