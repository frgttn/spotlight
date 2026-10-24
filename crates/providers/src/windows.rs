//! Window Management: halves, thirds, quarters, maximize, other displays and
//! always on top, for the window that was active before the launcher (or
//! has the keyboard when an item hotkey is pressed).

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
    window::{LAYOUTS, WindowCommand},
};

use crate::glyph;

pub const ID: &str = "windows";

pub struct WindowManagement;

impl Provider for WindowManagement {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Window Management"
    }

    fn description(&self) -> &'static str {
        "Halves, thirds, quarters, maximize, other displays and always on top for the active window."
    }

    fn catalog(&self) -> Vec<SearchItem> {
        LAYOUTS
            .iter()
            .map(|&(instance, title, keywords, command)| {
                let mut item = SearchItem::new(item_id(ID, instance), title)
                    .subtitle("Window Management")
                    .icon(Icon::Glyph(icon(command)))
                    .action(
                        title,
                        Command::Window {
                            target: None,
                            command,
                        },
                    );
                item.keywords = keywords
                    .iter()
                    .chain(&["window"])
                    .map(|&word| word.to_owned())
                    .collect();
                item
            })
            .collect()
    }
}

fn icon(command: WindowCommand) -> char {
    match command {
        WindowCommand::Area(area) if area.x == 0. && area.width < 1. => glyph::DOCK_LEFT,
        WindowCommand::Area(area) if area.x > 0. && area.x + area.width >= 0.999 => {
            glyph::DOCK_RIGHT
        }
        WindowCommand::Area(area) if area.y > 0. && area.width >= 0.999 => glyph::DOCK_BOTTOM,
        WindowCommand::Maximize
        | WindowCommand::AlmostMaximize
        | WindowCommand::MaximizeWidth
        | WindowCommand::MaximizeHeight
        | WindowCommand::MakeLarger => glyph::MAXIMIZE,
        WindowCommand::Restore | WindowCommand::MakeSmaller | WindowCommand::ReasonableSize => {
            glyph::RESTORE_WINDOW
        }
        WindowCommand::Minimize => glyph::MINIMIZE,
        WindowCommand::NextDisplay | WindowCommand::PreviousDisplay => glyph::DISPLAY,
        WindowCommand::ToggleAlwaysOnTop => glyph::PIN,
        WindowCommand::FocusNextAppWindow | WindowCommand::FocusPreviousAppWindow => glyph::SWITCH,
        _ => glyph::WINDOWS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_layout_for_the_active_window() {
        let items = WindowManagement.catalog();
        assert_eq!(items.len(), LAYOUTS.len());
        let left = items.iter().find(|item| item.title == "Left Half").unwrap();
        assert_eq!(left.id, "windows:left-half");
        assert!(matches!(
            left.primary_action().unwrap().command,
            Command::Window { target: None, .. }
        ));
        assert!(left.keywords.contains(&"window".to_owned()));
    }
}
