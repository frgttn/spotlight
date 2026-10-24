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
        }
    }
}

impl Config {
    /// From the three settings, each `None` when never saved.
    pub fn from_settings(
        folders: Option<&str>,
        exclusions: Option<&str>,
        hidden: Option<&str>,
    ) -> Self {
        Self {
            folders: folders
                .map(lines)
                .unwrap_or_default()
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            exclusions: exclusions
                .map(lines)
                .unwrap_or_else(|| Self::default().exclusions),
            include_hidden: hidden.is_some_and(|value| value == "true"),
        }
    }

    pub fn folders_setting(&self) -> String {
        self.folders
            .iter()
            .map(|folder| folder.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn exclusions_setting(&self) -> String {
        self.exclusions.join("\n")
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
        let config = Config::from_settings(Some("D:\\Work\n\n E:\\Photos "), None, Some("true"));
        assert_eq!(
            config.folders,
            [PathBuf::from(r"D:\Work"), PathBuf::from(r"E:\Photos")]
        );
        assert_eq!(config.exclusions, Config::default().exclusions);
        assert!(config.include_hidden);
        let again = Config::from_settings(
            Some(&config.folders_setting()),
            Some(&config.exclusions_setting()),
            Some("true"),
        );
        assert_eq!(again, config);
        assert!(
            Config::from_settings(None, Some(""), None)
                .exclusions
                .is_empty()
        );
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
