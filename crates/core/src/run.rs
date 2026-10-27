//! Run: commands typed as in Windows' Run dialog (Win+R), after a prefix in
//! root search (`>gpedit.msc`, `>cmd /k ipconfig`), and the ones run before.

/// `settings` key: what a root search starts with to run a command; empty
/// turns it off.
pub const PREFIX_SETTING: &str = "run_prefix";
/// The prefix until the user picks another.
pub const DEFAULT_PREFIX: &str = ">";

/// How many past commands the prefix alone lists.
pub const HISTORY_SHOWN: usize = 20;

/// A command line as the Run dialog splits it: the program (or file,
/// folder, URL) and its arguments. A quoted first word is the program; an
/// unquoted path with spaces is found by trying the longest beginning of the
/// line that `exists`, as the Run dialog does.
pub fn split(line: &str, exists: impl Fn(&str) -> bool) -> (&str, &str) {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('"') {
        return match rest.split_once('"') {
            Some((program, args)) => (program, args.trim()),
            None => (rest, ""),
        };
    }
    let mut ends: Vec<usize> = line
        .char_indices()
        .filter(|(_, ch)| ch.is_whitespace())
        .map(|(ix, _)| ix)
        .collect();
    ends.push(line.len());
    // The whole line first, then shorter and shorter.
    for &end in ends.iter().rev() {
        let program = &line[..end];
        if program.contains(['\\', '/', ':']) && exists(program) {
            return (program, line[end..].trim());
        }
    }
    let end = ends[0];
    (&line[..end], line[end..].trim())
}

/// The text after `prefix` when `query` starts with it.
pub fn after_prefix<'a>(prefix: &str, query: &'a str) -> Option<&'a str> {
    if prefix.is_empty() {
        return None;
    }
    query.trim_start().strip_prefix(prefix)
}

/// Whether `query` could be a command name to look up: one word, no path.
pub fn looks_like_name(query: &str) -> bool {
    let query = query.trim();
    !query.is_empty()
        && query.len() <= 64
        && !query.contains(char::is_whitespace)
        && !query.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_the_run_dialog() {
        let none = |_: &str| false;
        assert_eq!(split("gpedit.msc", none), ("gpedit.msc", ""));
        assert_eq!(split("  cmd /k ipconfig ", none), ("cmd", "/k ipconfig"));
        assert_eq!(
            split(r#""C:\Program Files\App\app.exe" -x "y z""#, none),
            (r"C:\Program Files\App\app.exe", r#"-x "y z""#)
        );
        let exists = |path: &str| path == r"C:\Program Files\App\app.exe";
        assert_eq!(
            split(r"C:\Program Files\App\app.exe --flag", exists),
            (r"C:\Program Files\App\app.exe", "--flag")
        );
        let folder = |path: &str| path == r"C:\My Folder";
        assert_eq!(split(r"C:\My Folder", folder), (r"C:\My Folder", ""));
        assert_eq!(
            split("https://example.com", none),
            ("https://example.com", "")
        );
    }

    #[test]
    fn knows_names_and_prefixes() {
        assert_eq!(after_prefix(">", " >cmd"), Some("cmd"));
        assert_eq!(after_prefix("", ">cmd"), None);
        assert_eq!(after_prefix(">", "cmd"), None);
        assert!(looks_like_name("gpedit"));
        assert!(looks_like_name("devmgmt.msc"));
        assert!(!looks_like_name("two words"));
        assert!(!looks_like_name(r"C:\x"));
        assert!(!looks_like_name(""));
    }
}
