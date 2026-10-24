//! What the user typed, split into words and filters.
//!
//! - Plain words match the file name or, failing that, a folder above it;
//!   at least one must match the name itself (`rea plan` finds
//!   `rea-test\notes\plan.md`).
//! - `"two words"` keeps a space inside one word.
//! - `^foo` starts the name, `foo$` ends it, `^foo$` is the whole name,
//!   `'foo` is a literal substring, `!foo` excludes names containing it.
//! - `ext:pdf,docx`, `*.pdf` and a leading `.pdf` filter by extension (a
//!   `.word` that is no known extension, like `.gitignore`, stays a word).
//! - `kind:image` (or `type:`) filters by [`Kind`], `in:name` requires a
//!   folder above the file whose name contains `name`.

use neo_frizbee::Matching;

use crate::kind::Kind;

/// Words beyond this are ignored: each takes a bit in a per-folder mask.
pub(crate) const MAX_WORDS: usize = 16;
/// `in:` filters beyond this are ignored, for the same reason.
pub(crate) const MAX_FOLDERS: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Word {
    pub text: String,
    pub matching: Matching,
    /// Written as `.word`: an extension filter if the index knows it.
    pub maybe_ext: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub(crate) words: Vec<Word>,
    pub(crate) excluded: Vec<String>,
    pub(crate) exts: Vec<String>,
    pub(crate) kind: Option<Kind>,
    pub(crate) folders: Vec<String>,
}

impl Query {
    pub fn parse(text: &str) -> Self {
        let mut query = Query::default();
        for (token, quoted) in tokens(text) {
            if quoted {
                query.push_word(token, Matching::Substring, false);
                continue;
            }
            if let Some((key, value)) = token.split_once(':')
                && !value.is_empty()
            {
                match key.to_lowercase().as_str() {
                    "ext" => {
                        query.exts.extend(
                            value
                                .split(',')
                                .map(|ext| ext.trim_start_matches('.').to_lowercase())
                                .filter(|ext| !ext.is_empty()),
                        );
                        continue;
                    }
                    "kind" | "type" => {
                        if let Some(kind) = Kind::parse(value) {
                            query.kind = Some(kind);
                            continue;
                        }
                    }
                    "in" => {
                        if query.folders.len() < MAX_FOLDERS {
                            query.folders.push(value.to_owned());
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            if let Some(ext) = token.strip_prefix("*.")
                && !ext.is_empty()
                && !ext.contains(['*', '.'])
            {
                query.exts.push(ext.to_lowercase());
                continue;
            }
            if let Some(excluded) = token.strip_prefix('!') {
                if !excluded.is_empty() {
                    query.excluded.push(excluded.to_owned());
                }
                continue;
            }
            let token = token.trim_matches('*');
            if let Some(literal) = token.strip_prefix('\'') {
                query.push_word(literal, Matching::Substring, false);
            } else if let Some(rest) = token.strip_prefix('^') {
                match rest.strip_suffix('$') {
                    Some(exact) => query.push_word(exact, Matching::Exact, false),
                    None => query.push_word(rest, Matching::Prefix, false),
                }
            } else if let Some(start) = token.strip_suffix('$') {
                query.push_word(start, Matching::Suffix, false);
            } else {
                let maybe_ext =
                    token.len() > 1 && token.starts_with('.') && !token[1..].contains('.');
                query.push_word(token, Matching::Fuzzy, maybe_ext);
            }
        }
        query
    }

    /// Nothing to search for: the caller shows recent files instead.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
            && self.exts.is_empty()
            && self.kind.is_none()
            && self.folders.is_empty()
    }

    fn push_word(&mut self, text: &str, matching: Matching, maybe_ext: bool) {
        if !text.is_empty() && self.words.len() < MAX_WORDS {
            self.words.push(Word {
                text: text.to_owned(),
                matching,
                maybe_ext,
            });
        }
    }
}

/// Whitespace-separated tokens; double quotes group words, with `true` for
/// a quoted token. An unclosed quote runs to the end.
fn tokens(text: &str) -> Vec<(&str, bool)> {
    let mut tokens = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return tokens;
        }
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"').unwrap_or(quoted.len());
            if !quoted[..end].trim().is_empty() {
                tokens.push((&quoted[..end], true));
            }
            rest = quoted.get(end + 1..).unwrap_or("");
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            tokens.push((&rest[..end], false));
            rest = &rest[end..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(query: &Query) -> Vec<(&str, Matching)> {
        query
            .words
            .iter()
            .map(|word| (word.text.as_str(), word.matching))
            .collect()
    }

    #[test]
    fn plain_words_and_atoms() {
        let query = Query::parse("  rea  ^plan  md$ 'x-y ^README$ ");
        assert_eq!(
            words(&query),
            [
                ("rea", Matching::Fuzzy),
                ("plan", Matching::Prefix),
                ("md", Matching::Suffix),
                ("x-y", Matching::Substring),
                ("README", Matching::Exact),
            ]
        );
    }

    #[test]
    fn filters() {
        let query = Query::parse("report ext:PDF,.docx *.xlsx kind:картинки in:Desktop !draft");
        assert_eq!(words(&query), [("report", Matching::Fuzzy)]);
        assert_eq!(query.exts, ["pdf", "docx", "xlsx"]);
        assert_eq!(query.kind, Some(Kind::Image));
        assert_eq!(query.folders, ["Desktop"]);
        assert_eq!(query.excluded, ["draft"]);
    }

    #[test]
    fn dot_words_may_be_extensions() {
        let query = Query::parse(".pdf .gitignore a.b");
        assert!(query.words[0].maybe_ext);
        assert!(query.words[1].maybe_ext);
        assert!(!query.words[2].maybe_ext);
    }

    #[test]
    fn quotes_group_words() {
        let query = Query::parse(r#""annual report" 2024 "unclosed"#);
        assert_eq!(
            words(&query),
            [
                ("annual report", Matching::Substring),
                ("2024", Matching::Fuzzy),
                ("unclosed", Matching::Substring),
            ]
        );
    }

    #[test]
    fn unknown_keys_and_colons_stay_words() {
        let query = Query::parse("C: kind:nonsense 10:30");
        assert_eq!(
            words(&query),
            [
                ("C:", Matching::Fuzzy),
                ("kind:nonsense", Matching::Fuzzy),
                ("10:30", Matching::Fuzzy),
            ]
        );
        assert!(Query::parse("   ").is_empty());
    }
}
