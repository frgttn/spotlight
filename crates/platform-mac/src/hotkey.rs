//! macOS counterpart of `loupe_platform_win::hotkey`, with its signatures.
//! Hotkeys are named, parsed and shown as on Windows (`vk` stays a Windows
//! virtual-key code, so settings carry over); registering them is a stub
//! for now.

use std::{fmt, time::Instant};

use crate::UNSUPPORTED;

/// A key combination. `vk` is a Windows virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u16,
}

impl Hotkey {
    /// Tried in order when no hotkey is configured or the configured one is taken.
    pub const DEFAULTS: [Hotkey; 2] = [
        Hotkey {
            ctrl: false,
            alt: true,
            shift: false,
            win: false,
            vk: 0x20,
        },
        Hotkey {
            ctrl: true,
            alt: false,
            shift: true,
            win: false,
            vk: 0x20,
        },
    ];
    /// Builds a hotkey from modifier flags and a key name (`"Space"`, `"K"`,
    /// `"F5"`, `"PageUp"`, `"/"`…, case-insensitive). `None` for keys that
    /// can't be a hotkey and for combinations without a modifier, which
    /// would swallow that key everywhere.
    pub fn new(ctrl: bool, alt: bool, shift: bool, win: bool, key: &str) -> Option<Self> {
        let vk = vk_from_name(key)?;
        (ctrl || alt || shift || win).then_some(Self {
            ctrl,
            alt,
            shift,
            win,
            vk,
        })
    }
    /// Parses the [`Display`](fmt::Display) form, e.g. `"Ctrl+Shift+Space"`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        // "Ctrl++" names the plus key: splitting leaves two empty parts at the end.
        if text.trim_end().ends_with("++") {
            parts.truncate(parts.len() - 2);
            parts.push("+");
        }
        let key = parts.pop()?;
        let (mut ctrl, mut alt, mut shift, mut win) = (false, false, false, false);
        for part in parts {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                "win" | "windows" | "super" => win = true,
                _ => return None,
            }
        }
        Self::new(ctrl, alt, shift, win, key)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.win, "Win"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key_name(self.vk))
    }
}

/// Key names and their Windows virtual-key codes, as `loupe_platform_win`
/// has them.
const NAMED_KEYS: &[(&str, u16)] = &[
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("Backspace", 0x08),
    ("Delete", 0x2E),
    ("Insert", 0x2D),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    (";", 0xBA),
    ("=", 0xBB),
    (",", 0xBC),
    ("-", 0xBD),
    (".", 0xBE),
    ("/", 0xBF),
    ("`", 0xC0),
    ("[", 0xDB),
    ("\\", 0xDC),
    ("]", 0xDD),
    ("'", 0xDE),
    ("+", 0xBB),
];

fn vk_from_name(name: &str) -> Option<u16> {
    let name = name.trim();
    if let [ch] = name.as_bytes()
        && ch.is_ascii_alphanumeric()
    {
        return Some(ch.to_ascii_uppercase() as u16);
    }
    if let Some(n) = name
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&n)
    {
        return Some(0x70 + n - 1);
    }
    NAMED_KEYS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
        .map(|&(_, vk)| vk)
}

fn key_name(vk: u16) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        _ => NAMED_KEYS
            .iter()
            .find(|&&(_, known)| known == vk)
            .map_or_else(|| format!("0x{vk:02X}"), |(name, _)| (*name).to_owned()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetError {
    /// Another app (or Windows itself) already owns this combination.
    InUse,
    Other(String),
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetError::InUse => f.write_str("already used by another app or by macOS"),
            SetError::Other(message) => f.write_str(message),
        }
    }
}

/// What the recorder saw, see [`Service::record`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    Hotkey(Hotkey),
    /// Esc with no modifiers.
    Cancel,
    /// A key pressed without Ctrl, Alt, Shift or Win.
    NeedsModifier,
}

/// Handle to the hotkey thread.
#[derive(Clone)]
pub struct Service {
    _private: (),
}

impl Service {
    /// Switches to `hotkey` and ends recording. On failure the previous hotkey
    /// stays registered. Blocks until the hotkey thread answers, which takes
    /// well under a millisecond.
    pub fn set(&self, _hotkey: Hotkey) -> Result<(), SetError> {
        Err(SetError::Other(UNSUPPORTED.into()))
    }
    /// Gives the item `item_id` the hotkey `hotkey`, or takes its hotkey away
    /// (`None`), and ends recording. On failure the item keeps its previous
    /// hotkey. The launcher's own hotkey counts as taken.
    pub fn set_item(&self, _item_id: &str, _hotkey: Option<Hotkey>) -> Result<(), SetError> {
        Err(SetError::Other(UNSUPPORTED.into()))
    }
    /// Starts recording a shortcut: the hotkey is unregistered and a
    /// low-level keyboard hook reports every key combination pressed anywhere
    /// on `recorded`, swallowing it. The hook sees keys before Windows matches
    /// them against hotkeys, so a combination owned by another app arrives too
    /// (and [`Self::set`] then reports it as [`SetError::InUse`]).
    ///
    /// Recording ends with [`Self::set`] or [`Self::resume`]; until then the
    /// user can't type anywhere else, so callers end it on Esc and when their
    /// window loses focus.
    pub fn record(&self, _recorded: async_channel::Sender<Recorded>) {}
    /// Ends recording and re-registers the current hotkey.
    pub fn resume(&self) {}
    /// Makes Win+V run the item `item_id` (its id arrives on `item_presses`,
    /// as for an item hotkey) instead of opening Windows' clipboard history,
    /// or (`None`) gives Win+V back to Windows. Fails while Win+V is the
    /// hotkey of the launcher or of an item.
    pub fn intercept_win_v(&self, _item_id: Option<&str>) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}

/// Starts the hotkey thread. Each press of the launcher hotkey sends the
/// instant `WM_HOTKEY` was dequeued (the start point for the hotkey-to-visible
/// measurement) on `presses`; each press of an item hotkey sends that item's
/// id on `item_presses`.
///
/// Tries `preferred` first, then [`Hotkey::DEFAULTS`]. Returns the service and
/// the hotkey that got registered, or an error naming what was tried (usually
/// because other apps own all of them; the service still works for `set`).
pub fn spawn(
    _preferred: Option<Hotkey>,
    _presses: async_channel::Sender<Instant>,
    _item_presses: async_channel::Sender<String>,
) -> Result<(Service, Result<Hotkey, String>), String> {
    // The service "runs", so the app starts; no hotkey gets registered.
    Ok((Service { _private: () }, Err(UNSUPPORTED.into())))
}
