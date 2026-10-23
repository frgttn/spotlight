//! Quicklinks: links the user saves under a name (a URL, a folder, an app's
//! deeplink), with placeholders filled in when they're opened.
//!
//! Placeholders, as in Raycast (see [`crate::placeholder`]): `{query}`,
//! `{argument …}`, `{date}`, `{time}`, `{datetime}`, `{day}`, `{uuid}` and
//! `{selection}`. Anything else in braces is left as it is.

use crate::placeholder::{self, Placeholder};
pub use crate::placeholder::{Argument, Moment, arguments, field_id, with_field_ids};
use crate::web::encode_query;

/// A saved quicklink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quicklink {
    /// Row id in the store; 0 for one not saved yet.
    pub id: i64,
    pub name: String,
    pub link: String,
    /// Typed after `!` in root search to open the link with the rest of the
    /// query (`!gh tokio`). Kept lowercase, without the `!`.
    pub keyword: Option<String>,
    /// The program to open the link with (a browser's exe); `None` opens it
    /// with whatever Windows associates with it.
    pub open_with: Option<String>,
}

/// Saved the first time the database is created, so there is something to
/// start from (and to delete). Name, keyword, link.
pub const DEFAULTS: &[(&str, &str, &str)] = &[
    ("Search GitHub", "gh", "https://github.com/search?q={query}"),
    (
        "Search YouTube",
        "yt",
        "https://www.youtube.com/results?search_query={query}",
    ),
    (
        "Search Wikipedia",
        "w",
        "https://en.wikipedia.org/w/index.php?search={query}",
    ),
    (
        "Search Google Maps",
        "maps",
        "https://www.google.com/maps/search/{query}",
    ),
    (
        "Translate",
        "tr",
        "https://translate.google.com/?sl=auto&text={argument name=\"Text\"}&tl={argument name=\"To\" options=\"en,ru,uk,de,fr,es\"}",
    ),
    (
        "Search Stack Overflow",
        "so",
        "https://stackoverflow.com/search?q={query}",
    ),
    (
        "Search crates.io",
        "crates",
        "https://crates.io/search?q={query}",
    ),
    (
        "Search docs.rs",
        "docs",
        "https://docs.rs/releases/search?query={query}",
    ),
    (
        "Search MDN",
        "mdn",
        "https://developer.mozilla.org/en-US/search?q={query}",
    ),
];

/// `link` with its arguments set to `values` (in [`arguments`] order; missing
/// or empty ones take their default, or their first option), encoded for a
/// URL when `link` is one.
pub fn fill(link: &str, values: &[String]) -> String {
    let arguments = arguments(link);
    let url = is_url(link);
    placeholder::replace(link, |placeholder| match placeholder {
        Placeholder::Argument(argument) => {
            placeholder::argument_value(&arguments, values, argument)
                .map(|value| encode_value(value, url))
        }
        _ => None,
    })
}

/// The values for a link opened with `!keyword text`: `text` goes to the
/// first free-text argument, the others are left to their defaults.
pub fn bang_values(arguments: &[Argument], text: &str) -> Vec<String> {
    let target = arguments.iter().position(Argument::is_free_text);
    (0..arguments.len())
        .map(|ix| {
            if Some(ix) == target {
                text.to_owned()
            } else {
                String::new()
            }
        })
        .collect()
}

/// A typed value as it goes into the link: percent-encoded in URLs, as it
/// is in paths.
pub fn encode_value(value: &str, url: bool) -> String {
    if url {
        // `%20` rather than `+`: a value can end up in a URL's path too.
        encode_query(value).replace('+', "%20")
    } else {
        value.to_owned()
    }
}

/// `link` with `{date}`, `{time}`, `{datetime}`, `{day}` and `{uuid}` filled in.
pub fn expand_moment(link: &str, moment: &Moment) -> String {
    placeholder::replace(link, |placeholder| placeholder.moment(moment))
}

/// Whether `link` contains `{date}`, `{time}`, `{datetime}`, `{day}` or `{uuid}`.
pub fn has_moment(link: &str) -> bool {
    placeholder::contains(link, |placeholder| {
        placeholder.moment(&Moment::default()).is_some()
    })
}

/// Whether `link` contains `{selection}`.
pub fn has_selection(link: &str) -> bool {
    placeholder::contains(link, |placeholder| *placeholder == Placeholder::Selection)
}

/// `link` with `{selection}` replaced by `text`, encoded as typed values are.
pub fn fill_selection(link: &str, text: &str) -> String {
    let value = encode_value(text.trim(), is_url(link));
    placeholder::replace(link, |placeholder| {
        (*placeholder == Placeholder::Selection).then(|| value.clone())
    })
}

/// A URL (`https://…`, `vscode://…`, `mailto:…`) rather than a file path.
pub fn is_url(link: &str) -> bool {
    let link = link.trim();
    let Some((scheme, _)) = link.split_once(':') else {
        return false;
    };
    // One letter is a drive: `C:\Users`.
    scheme.len() > 1
        && scheme.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

/// The site a URL goes to (`github.com`), for the subtitle.
pub fn host(link: &str) -> Option<&str> {
    let rest = link.trim().split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

/// Why `link` can't be saved, in words for the form.
pub fn check_link(link: &str) -> Result<(), &'static str> {
    let link = link.trim();
    if link.is_empty() {
        return Err("Link is required");
    }
    let lower = link.to_ascii_lowercase();
    for scheme in ["javascript:", "vbscript:", "data:"] {
        if lower.starts_with(scheme) {
            return Err("Script and data links can't be saved");
        }
    }
    if !is_url(link) && !looks_like_path(link) {
        return Err("Use a URL (https://…) or a full path (C:\\…)");
    }
    Ok(())
}

/// `C:\…`, `\\server\share` or `%USERPROFILE%\…`.
fn looks_like_path(link: &str) -> bool {
    let bytes = link.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    drive || link.starts_with(r"\\") || link.starts_with('%')
}

/// `keyword` as stored: trimmed, without a leading `!`, lowercase; `None`
/// when empty. `Err` says why it can't be used.
pub fn normalize_keyword(keyword: &str) -> Result<Option<String>, &'static str> {
    let keyword = keyword.trim();
    let keyword = keyword.strip_prefix('!').unwrap_or(keyword);
    if keyword.is_empty() {
        return Ok(None);
    }
    if keyword.contains(char::is_whitespace) || keyword.contains('!') {
        return Err("A keyword is one word, without spaces");
    }
    Ok(Some(keyword.to_lowercase()))
}

/// A root-search query with a bang: the keyword and the rest of the text.
/// `"!gh  tokio rt"` → `("gh", Some("tokio rt"))`; `"!gh"` → `("gh", None)`,
/// still being typed. At the end, as DuckDuckGo allows: `"tokio rt !gh"`.
pub fn parse_bang(query: &str) -> Option<(String, Option<&str>)> {
    let query = query.trim();
    let Some(rest) = query.strip_prefix('!') else {
        let (text, keyword) = query.rsplit_once(char::is_whitespace)?;
        let keyword = keyword
            .strip_prefix('!')
            .filter(|keyword| !keyword.is_empty())?;
        return Some((keyword.to_lowercase(), Some(text.trim())));
    };
    if rest.starts_with(char::is_whitespace) {
        return None;
    }
    match rest.split_once(char::is_whitespace) {
        Some((keyword, text)) => {
            let text = text.trim();
            Some((keyword.to_lowercase(), (!text.is_empty()).then_some(text)))
        }
        None => Some((rest.to_lowercase(), None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_other_braces_alone() {
        let link = "https://x.com/{unknown}/{query bad}/{{query}/{date extra}";
        assert_eq!(arguments(link).len(), 1);
        assert_eq!(
            fill(link, &["a b".into()]),
            "https://x.com/{unknown}/{query bad}/{a%20b/{date extra}"
        );
    }

    #[test]
    fn fills_and_encodes_urls() {
        assert_eq!(
            fill(
                "https://github.com/search?q={query}",
                &["rust & gpui".into()]
            ),
            "https://github.com/search?q=rust%20%26%20gpui"
        );
        // Empty values take the default.
        let link =
            r#"https://t.com/?tl={argument name="To" default="en"}&text={argument name="Text"}"#;
        assert_eq!(
            fill(link, &[" ".into(), "привет".into()]),
            "https://t.com/?tl=en&text=%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82"
        );
    }

    #[test]
    fn bang_text_goes_to_the_first_free_text_argument() {
        let (_, _, link) = DEFAULTS
            .iter()
            .find(|(_, keyword, _)| *keyword == "tr")
            .unwrap();
        let args = arguments(link);
        let values = bang_values(&args, "good morning");
        assert_eq!(
            fill(link, &values),
            "https://translate.google.com/?sl=auto&text=good%20morning&tl=en"
        );
    }

    #[test]
    fn paths_are_not_encoded() {
        assert_eq!(
            fill(r"C:\Projects\{query}", &["my app".into()]),
            r"C:\Projects\my app"
        );
    }

    #[test]
    fn numbers_fields_for_forms() {
        let link = r#"https://x.com/{argument name="A"}/{query}/{argument name="A"}/{date}"#;
        assert_eq!(
            with_field_ids(link),
            "https://x.com/{arg0}/{arg1}/{arg0}/{date}"
        );
    }

    #[test]
    fn expands_the_moment() {
        let moment = Moment {
            date: "2026-10-23".into(),
            time: "14:05".into(),
            day: "Friday".into(),
            uuid: "u".into(),
        };
        let link = r"C:\Notes\{date} {time} {uuid} {query} {day} {datetime}.md";
        assert!(has_moment(link));
        assert!(!has_moment("https://x.com/{query}"));
        assert_eq!(
            expand_moment(link, &moment),
            r"C:\Notes\2026-10-23 14:05 u {query} Friday 2026-10-23 14:05.md"
        );
    }

    #[test]
    fn tells_urls_from_paths() {
        for url in [
            "https://x.com",
            "vscode://file/C:/x",
            "mailto:a@b.c",
            "ms-settings:display",
        ] {
            assert!(is_url(url), "{url}");
            assert_eq!(check_link(url), Ok(()));
        }
        for path in [
            r"C:\Users",
            "D:/Games",
            r"\\nas\share",
            r"%USERPROFILE%\Desktop",
        ] {
            assert!(!is_url(path), "{path}");
            assert_eq!(check_link(path), Ok(()));
        }
        assert!(check_link("").is_err());
        assert!(check_link("github.com").is_err());
        assert!(check_link("JavaScript:alert(1)").is_err());
        assert_eq!(
            host("https://www.github.com/search?q=1"),
            Some("github.com")
        );
        assert_eq!(host(r"C:\Users"), None);
    }

    #[test]
    fn keywords() {
        assert_eq!(normalize_keyword(" !GH "), Ok(Some("gh".into())));
        assert_eq!(normalize_keyword(""), Ok(None));
        assert_eq!(normalize_keyword("!"), Ok(None));
        assert!(normalize_keyword("g h").is_err());
    }

    #[test]
    fn parses_bangs() {
        assert_eq!(
            parse_bang("!gh tokio  rt "),
            Some(("gh".into(), Some("tokio  rt")))
        );
        assert_eq!(parse_bang("!GH"), Some(("gh".into(), None)));
        assert_eq!(parse_bang("!gh   "), Some(("gh".into(), None)));
        assert_eq!(parse_bang("! gh"), None);
        assert_eq!(parse_bang("gh tokio"), None);
        assert_eq!(
            parse_bang("tokio  rt !GH "),
            Some(("gh".into(), Some("tokio  rt")))
        );
        assert_eq!(parse_bang("tokio !"), None);
        assert_eq!(
            parse_bang("100% !important"),
            Some(("important".into(), Some("100%")))
        );
    }

    #[test]
    fn defaults_are_valid() {
        let mut keywords: Vec<_> = DEFAULTS.iter().map(|(_, keyword, _)| *keyword).collect();
        for (name, keyword, link) in DEFAULTS {
            assert_eq!(check_link(link), Ok(()), "{name}");
            assert_eq!(normalize_keyword(keyword), Ok(Some(keyword.to_string())));
            assert!(!arguments(link).is_empty(), "{name}");
        }
        keywords.sort_unstable();
        keywords.dedup();
        assert_eq!(keywords.len(), DEFAULTS.len());
    }
}
