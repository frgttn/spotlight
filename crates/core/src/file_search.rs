//! File Search's settings and progress, shared by its provider, the app that
//! saves them and the Settings window that edits them.

use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

/// Extra folders to index besides the user profile, one path per line.
pub const FOLDERS_SETTING: &str = "files_roots";
/// What indexing leaves out, one rule per line; never saved means
/// [`DEFAULT_EXCLUSIONS`].
pub const EXCLUSIONS_SETTING: &str = "files_exclusions";
/// `true` indexes and shows hidden files too.
pub const HIDDEN_SETTING: &str = "files_hidden";
/// Busy folders left out automatically, one path per line.
pub const HOT_SETTING: &str = "files_hot";
/// Busy folders the user asked to keep anyway, one path per line.
pub const KEPT_SETTING: &str = "files_kept";
/// What a root search starts with to search files; empty turns it off.
pub const PREFIX_SETTING: &str = "files_prefix";
/// The prefix until the user picks another: Shift+8 in both the English and
/// Russian layouts, and never the start of a calculation.
pub const DEFAULT_PREFIX: &str = "*";

/// Left out unless the user says otherwise: app data, the recycle bin, and
/// folders that tools fill with thousands of files nobody opens by hand. A
/// rule with a path separator or a drive colon is a path; any other is a
/// folder name, left out wherever it appears.
pub const DEFAULT_EXCLUSIONS: &[&str] = &[
    "AppData",
    "node_modules",
    ".git",
    "target",
    "__pycache__",
    "$Recycle.Bin",
    "System Volume Information",
];

/// What is indexed: the scope, and what to leave out of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Folders added in Settings, besides the user profile.
    pub folders: Vec<PathBuf>,
    pub exclusions: Vec<String>,
    pub include_hidden: bool,
    /// Folders left out for changing all the time (caches, build output),
    /// found while following changes.
    pub hot: Vec<PathBuf>,
    /// Busy folders the user took back: never left out automatically again.
    pub kept: Vec<PathBuf>,
    /// What a root search starts with to search files; empty: off.
    pub prefix: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            exclusions: DEFAULT_EXCLUSIONS
                .iter()
                .map(|&rule| rule.to_owned())
                .collect(),
            include_hidden: false,
            hot: Vec::new(),
            kept: Vec::new(),
            prefix: DEFAULT_PREFIX.to_owned(),
        }
    }
}

impl Config {
    /// From the settings `setting` reads, by key; `None` for one never saved.
    pub fn from_settings(setting: impl Fn(&str) -> Option<String>) -> Self {
        let paths = |key: &str| -> Vec<PathBuf> {
            setting(key)
                .as_deref()
                .map(lines)
                .unwrap_or_default()
                .into_iter()
                .map(PathBuf::from)
                .collect()
        };
        Self {
            folders: paths(FOLDERS_SETTING),
            exclusions: setting(EXCLUSIONS_SETTING)
                .as_deref()
                .map(lines)
                .unwrap_or_else(|| Self::default().exclusions),
            include_hidden: setting(HIDDEN_SETTING).is_some_and(|value| value == "true"),
            hot: paths(HOT_SETTING),
            kept: paths(KEPT_SETTING),
            prefix: setting(PREFIX_SETTING)
                .map(|prefix| prefix.trim().to_owned())
                .unwrap_or_else(|| DEFAULT_PREFIX.to_owned()),
        }
    }

    /// Every setting, as (key, value), for saving.
    pub fn to_settings(&self) -> Vec<(&'static str, String)> {
        let paths = |paths: &[PathBuf]| {
            paths
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n")
        };
        vec![
            (FOLDERS_SETTING, paths(&self.folders)),
            (EXCLUSIONS_SETTING, self.exclusions.join("\n")),
            (HIDDEN_SETTING, self.include_hidden.to_string()),
            (HOT_SETTING, paths(&self.hot)),
            (KEPT_SETTING, paths(&self.kept)),
            (PREFIX_SETTING, self.prefix.clone()),
        ]
    }

    /// The text after the prefix when `query` starts with it.
    pub fn after_prefix<'a>(&self, query: &'a str) -> Option<&'a str> {
        if self.prefix.is_empty() {
            return None;
        }
        query.trim_start().strip_prefix(self.prefix.as_str())
    }

    /// Path rules for the busy folders still left out.
    pub fn hot_rules(&self) -> impl Iterator<Item = String> + '_ {
        self.hot
            .iter()
            .filter(|hot| {
                !self
                    .kept
                    .iter()
                    .any(|kept| fold_path(kept) == fold_path(hot))
            })
            .map(|hot| hot.to_string_lossy().into_owned())
    }

    /// The user profile, then the added folders, each once. An added folder
    /// inside another root stays a root of its own: it is indexed even where
    /// the outer one leaves it out (a folder in AppData), and the walk of the
    /// outer one skips it.
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = profile().into_iter().collect();
        for folder in &self.folders {
            if !roots
                .iter()
                .any(|root| fold_path(root) == fold_path(folder))
            {
                roots.push(folder.clone());
            }
        }
        roots
    }
}

/// How far indexing has got.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Turned off, or not started yet.
    Idle,
    /// Walking the scope: entries found so far.
    Indexing(usize),
    Ready {
        entries: usize,
        memory_bytes: usize,
        took: Duration,
        /// Roots the index service filled in; the others were walked.
        from_service: usize,
    },
}

impl Status {
    /// One line for Settings.
    pub fn describe(&self) -> String {
        match self {
            Status::Idle => "Not indexing".into(),
            Status::Indexing(found) => format!("Indexing… {} files so far", thousands(*found)),
            Status::Ready {
                entries,
                memory_bytes,
                took,
                from_service,
            } => {
                let mut line = format!(
                    "{} files and folders, indexed in {:.1} s, {:.1} MB",
                    thousands(*entries),
                    took.as_secs_f64(),
                    *memory_bytes as f64 / 1_048_576.
                );
                match from_service {
                    0 => {}
                    1 => line.push_str(", one folder through the indexing service"),
                    n => line.push_str(&format!(", {n} folders through the indexing service")),
                }
                line
            }
        }
    }
}

/// The user profile folder, `C:\Users\<name>`.
pub fn profile() -> Option<PathBuf> {
    env::var_os("USERPROFILE").map(PathBuf::from)
}

/// `12,345`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (ix, ch) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn fold_path(path: &Path) -> String {
    path.to_string_lossy().trim_end_matches('\\').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let saved = |key: &str| match key {
            FOLDERS_SETTING => Some("D:\\Work\n\n E:\\Photos ".to_owned()),
            HIDDEN_SETTING => Some("true".to_owned()),
            HOT_SETTING => Some(r"C:\Users\me\cache".to_owned()),
            _ => None,
        };
        let config = Config::from_settings(saved);
        assert_eq!(
            config.folders,
            [PathBuf::from(r"D:\Work"), PathBuf::from(r"E:\Photos")]
        );
        assert_eq!(config.exclusions, Config::default().exclusions);
        assert!(config.include_hidden);
        assert_eq!(config.prefix, DEFAULT_PREFIX);
        let settings = config.to_settings();
        let again = Config::from_settings(|key| {
            settings
                .iter()
                .find(|(saved, _)| *saved == key)
                .map(|(_, value)| value.clone())
        });
        assert_eq!(again, config);
        let cleared = Config::from_settings(|key| (key == EXCLUSIONS_SETTING).then(String::new));
        assert!(cleared.exclusions.is_empty());
    }

    #[test]
    fn prefix_and_busy_folders() {
        let mut config = Config::default();
        assert_eq!(config.after_prefix("*report"), Some("report"));
        assert_eq!(config.after_prefix("  *.pdf"), Some(".pdf"));
        assert_eq!(config.after_prefix("report"), None);
        config.prefix.clear();
        assert_eq!(config.after_prefix("*report"), None);

        config.hot = vec![PathBuf::from(r"C:\a\cache"), PathBuf::from(r"C:\b\logs")];
        config.kept = vec![PathBuf::from(r"c:\B\LOGS")];
        assert_eq!(config.hot_rules().collect::<Vec<_>>(), [r"C:\a\cache"]);
    }

    #[test]
    fn roots_keep_nested_folders_once() {
        let profile = profile().unwrap();
        let config = Config {
            folders: vec![
                profile.join("AppData"),
                PathBuf::from(r"Z:\Projects"),
                PathBuf::from(r"z:\projects\"),
                profile.clone(),
            ],
            ..Config::default()
        };
        assert_eq!(
            config.roots(),
            [
                profile.clone(),
                profile.join("AppData"),
                PathBuf::from(r"Z:\Projects")
            ]
        );
    }

    #[test]
    fn counts_and_status() {
        assert_eq!(thousands(12_345), "12,345");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000_000), "1,000,000");
        let ready = Status::Ready {
            entries: 152_340,
            memory_bytes: 7 * 1_048_576,
            took: Duration::from_millis(1_234),
            from_service: 1,
        };
        assert_eq!(
            ready.describe(),
            "152,340 files and folders, indexed in 1.2 s, 7.0 MB, one folder through the indexing service"
        );
    }
}
