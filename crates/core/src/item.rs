//! What providers hand to the UI: search items and the actions they run.
//!
//! Everything here is plain data, so the same types can later come from script
//! commands and WASM plugins.

use crate::{
    clip::IgnoredApp,
    quicklink::{self, Quicklink},
    snippet::{self, Snippet},
    view::{Preview, View},
    web::{SearchEngine, encode_query},
    window::WindowCommand,
};

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
    /// Shown beside the list while the row is selected, in lists that have
    /// previews (clipboard history).
    pub preview: Option<Box<Preview>>,
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
            preview: None,
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
        self.actions.push(Action::new(title, command));
        self
    }

    /// Gives the last action added a key combination of its own (see
    /// [`shortcut`]), in place of its command's default.
    pub fn shortcut(mut self, keys: &str) -> Self {
        if let Some(action) = self.actions.last_mut() {
            action.shortcut = Some(keys.to_owned());
        }
        self
    }

    pub fn preview(mut self, preview: Preview) -> Self {
        self.preview = Some(Box::new(preview));
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
    /// The icon of the site at `origin` (`https://github.com`, see
    /// [`crate::favicon`]); `glyph` is drawn until it arrives, or without one.
    Site { origin: String, glyph: char },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub title: String,
    pub command: Command,
    /// Runs it from the list without opening the action panel, in GPUI's
    /// keystroke syntax (`ctrl-shift-c`). `None` takes the command's default,
    /// [`Command::default_shortcut`].
    pub shortcut: Option<String>,
}

impl Action {
    pub fn new(title: impl Into<String>, command: Command) -> Self {
        Self {
            title: title.into(),
            command,
            shortcut: None,
        }
    }

    /// Its own key combination, or its command's default.
    pub fn shortcut(&self) -> Option<&str> {
        self.shortcut
            .as_deref()
            .or_else(|| self.command.default_shortcut())
    }
}

/// Key combinations for actions, named and chosen as Raycast's for Windows
/// are, so the same action has the same keys in both. The first two actions
/// are always Enter and Ctrl+Enter, whatever they ask for.
pub mod shortcut {
    /// Copies the main thing: a file, a URL.
    pub const COPY: &str = "ctrl-shift-c";
    /// Copies something about it: a name.
    pub const COPY_SECONDARY: &str = "ctrl-alt-c";
    /// Copies something else about it: a path.
    pub const COPY_TERTIARY: &str = "alt-shift-c";
    pub const COPY_DEEPLINK: &str = "ctrl-shift-c";
    /// Opens it somewhere else: Open With.
    pub const OPEN: &str = "ctrl-o";
    /// Shows where it is: Show in Explorer.
    pub const OPEN_SECONDARY: &str = "ctrl-shift-o";
    /// A third way to run it: Run as Administrator.
    pub const TERTIARY: &str = "ctrl-shift-enter";
    /// A fourth: Open in Terminal.
    pub const QUATERNARY: &str = "ctrl-alt-enter";
    /// Deletes it.
    pub const CLEAR: &str = "ctrl-d";
    /// Deletes all of them.
    pub const CLEAR_ALL: &str = "ctrl-alt-d";
    pub const EDIT: &str = "ctrl-e";
    /// Creates something from it: a quicklink.
    pub const SAVE: &str = "ctrl-s";
    pub const PIN: &str = "ctrl-.";
    /// Shows more about it: Show Details.
    pub const INFO: &str = "ctrl-i";
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
    /// Asks which app to open this file with (Windows' Open With dialog).
    OpenWith(String),
    /// Puts this file on the clipboard, to paste in Explorer or a chat app.
    CopyFile(String),
    /// Moves this file or folder to the Recycle Bin.
    MoveToTrash(String),
    /// Opens a terminal in this folder: Windows Terminal, else PowerShell.
    OpenTerminal(String),
    /// An action of provider `provider`'s own on its row `item` (an item
    /// id), which the UI hands to `Provider::act`: Exclude from Index.
    ProviderAction {
        provider: String,
        action: String,
        item: String,
    },
    /// Opens an `http`/`https` URL in the default browser, an `ms-settings:`
    /// page in the Settings app, or a game through its launcher (`steam://`…).
    /// The app refuses other schemes.
    OpenUrl(String),
    /// Puts text on the clipboard.
    Copy(String),
    /// Shows the calculations the user used, in a list of their own.
    OpenCalculatorHistory,
    /// Opens a quicklink the user saved: a URL of any scheme or a path, with
    /// its arguments filled in (`{date}`, `{time}` and `{uuid}` are filled when
    /// it opens), with `app` or the default handler. Only quicklinks produce
    /// it, since it opens whatever the user wrote.
    OpenQuicklink {
        link: String,
        app: Option<String>,
    },
    /// Saves a new (id 0) or edited quicklink. The UI checks it first.
    SaveQuicklink(Quicklink),
    DeleteQuicklink(i64),
    /// Makes `engine` (named `name`) the web search engine, as in Settings.
    SetSearchEngine {
        engine: SearchEngine,
        name: String,
    },
    /// Saves a new (id 0) or edited snippet. The UI checks it first.
    SaveSnippet(Snippet),
    DeleteSnippet(i64),
    /// Puts a snippet's text on the clipboard with its placeholders filled
    /// in at that moment (`{clipboard}` from what was there) and, with
    /// `paste`, pastes it into the window that was active before the
    /// launcher, the caret at `{cursor}`.
    PasteSnippet {
        text: String,
        paste: bool,
    },
    /// Asks for a JSON file of snippets, as Raycast exports them, and adds them.
    ImportSnippets,
    /// Asks where to save every snippet as JSON, as Raycast imports them.
    ExportSnippets,
    /// Opens a view on top of the current one; Esc comes back.
    PushView(Box<View>),
    /// Opens a view the provider `provider` builds when asked (see
    /// `Provider::view`), for lists too large to carry in every command.
    OpenProviderView {
        provider: String,
        view: String,
    },
    /// Puts clipboard history entry `id` back on the clipboard and, with
    /// `paste`, pastes it into the window that was active before the launcher.
    RestoreClip {
        id: i64,
        paste: bool,
    },
    /// Pins or unpins a clipboard history entry: pinned ones stay at the top
    /// and are never trimmed.
    PinClip {
        id: i64,
        pinned: bool,
    },
    DeleteClip(i64),
    /// Deletes every clipboard history entry that isn't pinned.
    ClearClipboardHistory,
    /// Stops keeping copies made in this app.
    IgnoreClipApp(IgnoredApp),
    /// Opens provider view `view` (as [`Command::OpenProviderView`]) with
    /// `query` already typed in its search field: Search Files from root
    /// search's file prefix.
    SearchIn {
        provider: String,
        view: String,
        query: String,
    },
    /// Goes back to root search with this text in the search field, the
    /// cursor at its end: `!g` completed to `!gh `.
    Complete(String),
    System(SystemCommand),
    /// Does `command` to window `target` (a raw HWND), or with `None` to the
    /// window that was active before the launcher (the app fills it in).
    Window {
        target: Option<isize>,
        command: WindowCommand,
    },
    /// Asks every window of process `pid` to close, as their × buttons would.
    QuitApp(u32),
    /// Ends process `pid` at once; what it hadn't saved is lost.
    ForceQuit(u32),
}

/// Where a command takes the text selected in the app the launcher came
/// from: in an `OpenUrl` URL (percent-encoded) or a quicklink.
pub const SELECTION: &str = "{selection}";

impl Command {
    /// Whether it uses the selected text ([`SELECTION`]), which the app reads
    /// before running it.
    pub fn needs_selection(&self) -> bool {
        match self {
            Command::OpenUrl(url) => url.contains(SELECTION),
            Command::OpenQuicklink { link, .. } => quicklink::has_selection(link),
            Command::PasteSnippet { text, .. } => snippet::has_selection(text),
            _ => false,
        }
    }

    /// The command with the selected `text` in place of [`SELECTION`].
    pub fn with_selection(&self, text: &str) -> Command {
        match self {
            Command::OpenUrl(url) => {
                Command::OpenUrl(url.replace(SELECTION, &encode_query(text.trim())))
            }
            Command::OpenQuicklink { link, app } => Command::OpenQuicklink {
                link: quicklink::fill_selection(link, text),
                app: app.clone(),
            },
            Command::PasteSnippet {
                text: snippet,
                paste,
            } => Command::PasteSnippet {
                text: snippet::fill_selection(snippet, text),
                paste: *paste,
            },
            command => command.clone(),
        }
    }

    /// Whether it's a quicklink with `{clipboard}`, which the app fills in
    /// with the clipboard's text before running it.
    pub fn needs_clipboard(&self) -> bool {
        matches!(self, Command::OpenQuicklink { link, .. } if quicklink::has_clipboard(link))
    }

    /// The command with `text` in place of `{clipboard}`.
    pub fn with_clipboard(&self, text: &str) -> Command {
        match self {
            Command::OpenQuicklink { link, app } => Command::OpenQuicklink {
                link: quicklink::fill_clipboard(link, text),
                app: app.clone(),
            },
            command => command.clone(),
        }
    }

    /// The keys an action with this command takes when it names none.
    pub fn default_shortcut(&self) -> Option<&'static str> {
        Some(match self {
            Command::OpenWith(_) => shortcut::OPEN,
            Command::ShowInFolder(_) => shortcut::OPEN_SECONDARY,
            Command::RunAsAdmin(_) => shortcut::TERTIARY,
            Command::OpenTerminal(_) => shortcut::QUATERNARY,
            Command::CopyFile(_) => shortcut::COPY,
            Command::MoveToTrash(_)
            | Command::DeleteQuicklink(_)
            | Command::DeleteSnippet(_)
            | Command::DeleteClip(_) => shortcut::CLEAR,
            Command::ClearClipboardHistory => shortcut::CLEAR_ALL,
            Command::PinClip { .. } => shortcut::PIN,
            _ => return None,
        })
    }

    /// Commands that are hard to undo ask for a second Enter.
    pub fn needs_confirmation(&self) -> bool {
        matches!(
            self,
            Command::System(
                SystemCommand::Restart | SystemCommand::ShutDown | SystemCommand::SignOut
            ) | Command::DeleteQuicklink(_)
                | Command::DeleteSnippet(_)
                | Command::ClearClipboardHistory
                | Command::ForceQuit(_)
                | Command::MoveToTrash(_)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_in_the_selection() {
        let search = Command::OpenUrl("https://www.google.com/search?q={selection}".into());
        assert!(search.needs_selection());
        assert_eq!(
            search.with_selection(" rust & gpui \n"),
            Command::OpenUrl("https://www.google.com/search?q=rust+%26+gpui".into())
        );
        let link = Command::OpenQuicklink {
            link: "https://translate.google.com/?text={selection}&tl=ru".into(),
            app: None,
        };
        assert!(link.needs_selection());
        assert_eq!(
            link.with_selection("a b"),
            Command::OpenQuicklink {
                link: "https://translate.google.com/?text=a%20b&tl=ru".into(),
                app: None,
            }
        );
        assert!(!Command::OpenUrl("https://x.com/?q={query}".into()).needs_selection());
        let path = Command::OpenQuicklink {
            link: r"C:\Notes\{selection}.md".into(),
            app: None,
        };
        assert_eq!(
            path.with_selection("todo list"),
            Command::OpenQuicklink {
                link: r"C:\Notes\todo list.md".into(),
                app: None,
            }
        );
    }
}
