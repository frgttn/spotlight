//! Clipboard history: how long entries are kept, and how an entry reads in
//! the list (its title, what kind of thing it is).

/// How much clipboard history to keep. Pinned entries are kept regardless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    /// Entries older than this many days go; `None` keeps them however old.
    pub days: Option<u32>,
    /// At most this many entries; the oldest go first.
    pub max: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            days: Some(30),
            max: 1000,
        }
    }
}

/// The choices Settings offers for [`Retention::days`], with their labels.
pub const KEEP_DAYS: &[(Option<u32>, &str)] = &[
    (Some(1), "1 day"),
    (Some(7), "7 days"),
    (Some(30), "30 days"),
    (Some(90), "3 months"),
    (None, "Forever"),
];

/// `settings` key: apps whose copies the history doesn't keep, one per line
/// as `path<TAB>name`.
pub const IGNORED_APPS_SETTING: &str = "clipboard_ignored_apps";

/// An app whose copies the history doesn't keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IgnoredApp {
    /// The program's path, which copies are matched by.
    pub path: String,
    /// How it's shown ("Google Chrome").
    pub name: String,
}

impl IgnoredApp {
    /// Parses [`IGNORED_APPS_SETTING`]'s value.
    pub fn parse_list(text: &str) -> Vec<Self> {
        text.lines()
            .filter_map(|line| {
                let (path, name) = line.split_once('\t').unwrap_or((line, ""));
                let path = path.trim();
                (!path.is_empty()).then(|| Self {
                    path: path.to_owned(),
                    name: match name.trim() {
                        "" => path.to_owned(),
                        name => name.to_owned(),
                    },
                })
            })
            .collect()
    }

    pub fn format_list(apps: &[Self]) -> String {
        apps.iter()
            .map(|app| format!("{}\t{}", app.path, app.name))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether `apps` has the program at `path` (paths compare without case,
    /// as Windows does).
    pub fn contains(apps: &[Self], path: &str) -> bool {
        apps.iter().any(|app| app.path.eq_ignore_ascii_case(path))
    }
}

/// The choices Settings offers for [`Retention::max`].
pub const KEEP_MAX: &[(u32, &str)] =
    &[(100, "100"), (500, "500"), (1000, "1,000"), (5000, "5,000")];

impl Retention {
    /// The settings form: `"30d,1000"`, or `"forever,1000"`.
    pub fn to_setting(self) -> String {
        match self.days {
            Some(days) => format!("{days}d,{}", self.max),
            None => format!("forever,{}", self.max),
        }
    }

    pub fn from_setting(text: &str) -> Option<Self> {
        let (days, max) = text.trim().split_once(',')?;
        let days = match days.trim() {
            "forever" => None,
            days => Some(days.strip_suffix('d')?.parse().ok().filter(|&d| d > 0)?),
        };
        let max = max.trim().parse().ok().filter(|&max| max > 0)?;
        Some(Self { days, max })
    }

    /// Unpinned entries copied before this (unix seconds) have expired.
    pub fn cutoff(self, now: i64) -> Option<i64> {
        self.days.map(|days| now - i64::from(days) * 24 * 3600)
    }
}

/// What a clipboard entry holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipKind {
    Text,
    /// A PNG file in the data folder.
    Image,
    /// File and folder paths, one per line.
    Files,
}

impl ClipKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ClipKind::Text => "text",
            ClipKind::Image => "image",
            ClipKind::Files => "files",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "text" => Some(ClipKind::Text),
            "image" => Some(ClipKind::Image),
            "files" => Some(ClipKind::Files),
            _ => None,
        }
    }
}

/// How text reads in a one-line list row: the first non-blank line, trimmed,
/// cut at `max` characters with "…", and whether more text follows.
pub fn first_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut title: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        title.push('…');
    }
    title
}

/// Where the preview-sized copy of the image at `path` goes:
/// `….png` → `….thumb.png`.
pub fn thumbnail_path(path: &str) -> String {
    let stem = path.strip_suffix(".png").unwrap_or(path);
    format!("{stem}.thumb.png")
}

/// How long ago `then` was, from `now` (unix seconds): "Just now",
/// "5 min ago", "3 h ago", "Yesterday", "4 days ago".
pub fn ago(then: i64, now: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const TWO_DAYS: i64 = 2 * DAY;
    match now - then {
        ..MINUTE => "Just now".into(),
        seconds @ MINUTE..HOUR => format!("{} min ago", seconds / MINUTE),
        seconds @ HOUR..DAY => format!("{} h ago", seconds / HOUR),
        DAY..TWO_DAYS => "Yesterday".into(),
        seconds => format!("{} days ago", seconds / DAY),
    }
}

/// Text that is one `#rgb` / `#rrggbb` / `#rrggbbaa` colour, for the
/// "color" tag.
pub fn is_color(text: &str) -> bool {
    let Some(hex) = text.trim().strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|ch| ch.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_round_trips_through_settings() {
        for retention in [
            Retention::default(),
            Retention {
                days: None,
                max: 100,
            },
        ] {
            assert_eq!(
                Retention::from_setting(&retention.to_setting()),
                Some(retention)
            );
        }
        assert_eq!(Retention::from_setting("0d,10"), None);
        assert_eq!(Retention::from_setting("7,10"), None);
        assert_eq!(Retention::from_setting("7d,0"), None);
    }

    #[test]
    fn first_line_skips_blank_lines_and_cuts_long_ones() {
        assert_eq!(first_line("\n  \n  hello world \nnext", 80), "hello world");
        assert_eq!(first_line("abcdef", 3), "abc…");
        assert_eq!(first_line("   ", 3), "");
    }

    #[test]
    fn says_how_long_ago() {
        assert_eq!(ago(100, 110), "Just now");
        assert_eq!(ago(0, 5 * 60), "5 min ago");
        assert_eq!(ago(0, 3 * 3600 + 5), "3 h ago");
        assert_eq!(ago(0, 30 * 3600), "Yesterday");
        assert_eq!(ago(0, 4 * 86_400), "4 days ago");
        assert_eq!(thumbnail_path(r"C:\d\ab.png"), r"C:\d\ab.thumb.png");
    }

    #[test]
    fn ignored_apps_round_trip() {
        let apps = vec![
            IgnoredApp {
                path: r"C:\Apps\KeePass.exe".into(),
                name: "KeePass".into(),
            },
            IgnoredApp {
                path: r"C:\Apps\x.exe".into(),
                name: r"C:\Apps\x.exe".into(),
            },
        ];
        assert_eq!(
            IgnoredApp::parse_list(&IgnoredApp::format_list(&apps)),
            apps
        );
        assert_eq!(IgnoredApp::parse_list("  \n"), []);
        assert!(IgnoredApp::contains(&apps, r"c:\apps\keepass.EXE"));
        assert!(!IgnoredApp::contains(&apps, r"C:\Apps\other.exe"));
    }

    #[test]
    fn recognises_hex_colours() {
        assert!(is_color("#fff"));
        assert!(is_color(" #1e90FF "));
        assert!(!is_color("#ggg"));
        assert!(!is_color("1e90ff"));
    }
}
