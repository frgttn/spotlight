//! What providers hand to the UI: search items and the actions they run.
//!
//! Everything here is plain data, so the same types can later come from script
//! commands and WASM plugins.

use crate::view::View;

/// One row in a result list.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchItem {
    /// `"{provider}:{instance}"`, see [`crate::item_id`]. Frecency (and later
    /// aliases and hotkeys) are keyed by it.
    pub id: String,
    pub title: String,
    /// Shown dimmed on the right of the row.
    pub subtitle: Option<String>,
    /// Extra words the item is found by besides its title ("reboot" for Restart).
    pub keywords: Vec<String>,
    pub icon: Icon,
    /// `[0]` is the primary action (Enter). Empty for rows that do nothing.
    pub actions: Vec<Action>,
}

impl SearchItem {
    pub fn new(id: String, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            subtitle: None,
            keywords: Vec::new(),
            icon: Icon::None,
            actions: Vec::new(),
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn keywords(mut self, keywords: &[&str]) -> Self {
        self.keywords = keywords.iter().map(|&word| word.to_owned()).collect();
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }

    pub fn action(mut self, title: impl Into<String>, command: Command) -> Self {
        self.actions.push(Action {
            title: title.into(),
            command,
        });
        self
    }

    pub fn primary_action(&self) -> Option<&Action> {
        self.actions.first()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Icon {
    /// The UI draws a letter badge.
    None,
    /// The shell icon of an `AppsFolder` item, by its parsing name.
    App(String),
    /// The icon of a file or shell object (a program, a Control Panel item),
    /// by path or absolute parsing name.
    Shell(String),
    /// A symbol from the system icon font (Segoe Fluent Icons / Segoe MDL2 Assets).
    Glyph(char),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub title: String,
    pub command: Command,
}

/// What an action does. A closed set of safe operations: providers describe
/// the effect, and the app carries it out.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Starts an `AppsFolder` item by its parsing name (AUMID or path).
    LaunchApp(String),
    /// Opens a file or shell object (a program, a Control Panel item) by path
    /// or absolute parsing name, as double-clicking it would.
    Open(String),
    /// Starts a program or shortcut (a file path) elevated, after the UAC prompt.
    RunAsAdmin(String),
    /// Opens an Explorer window on the folder holding this file, with the file selected.
    ShowInFolder(String),
    /// Opens an `http`/`https` URL in the default browser, an `ms-settings:`
    /// page in the Settings app, or a game through its launcher (`steam://`…).
    /// The app refuses other schemes.
    OpenUrl(String),
    /// Puts text on the clipboard.
    Copy(String),
    /// Shows the calculations the user used, in a list of their own.
    OpenCalculatorHistory,
    /// Opens a view on top of the current one; Esc comes back.
    PushView(Box<View>),
    System(SystemCommand),
}

impl Command {
    /// Commands that are hard to undo ask for a second Enter.
    pub fn needs_confirmation(&self) -> bool {
        matches!(
            self,
            Command::System(
                SystemCommand::Restart | SystemCommand::ShutDown | SystemCommand::SignOut
            )
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemCommand {
    Lock,
    Sleep,
    Restart,
    ShutDown,
    SignOut,
    EmptyRecycleBin,
    ToggleDarkMode,
    ToggleMute,
    /// Up or down by 10% of the default playback device.
    VolumeUp,
    VolumeDown,
    /// Percent, 0–100.
    SetVolume(u8),
    ToggleBluetooth,
    /// HDR on every display that supports it.
    ToggleHdr,
    /// Safely removes the drive with this letter (`'E'`).
    Eject(char),
    /// Every removable drive.
    EjectAll,
}
