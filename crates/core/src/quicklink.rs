//! Quicklinks: links the user saves under a name (a URL, a folder, an app's
//! deeplink), with placeholders filled in when they're opened.
//!
//! Placeholders, as in Raycast:
//! - `{query}` or `{argument}`: text the user types when opening the link.
//! - `{argument name="Repo" default="zed" options="a,b"}`: a named argument,
//!   optionally with a default value or a fixed set of choices. Arguments with
//!   the same name share one value.
//! - `{date}`, `{time}`, `{uuid}`: filled in when the link opens.
//!
//! Anything else in braces is left as it is.

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

/// One value the user supplies when opening a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argument {
    /// `"Query"` for `{query}` and `{argument}`.
    pub name: String,
    pub default: String,
    /// Fixed choices; empty for free text.
    pub options: Vec<String>,
}

/// What `{date}`, `{time}` and `{uuid}` stand for at the moment a link opens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moment {
    /// `2026-10-23`
    pub date: String,
    /// `14:05`
    pub time: String,
    pub uuid: String,
}

/// The name `{query}` and a nameless `{argument}` go by.
const QUERY: &str = "Query";

#[derive(Debug, PartialEq, Eq)]
enum Placeholder {
    Argument(Argument),
    Date,
    Time,
    Uuid,
}

/// Splits `link` into literal text and placeholders: `(start, end, placeholder)`
/// byte ranges, in order.
fn placeholders(link: &str) -> Vec<(usize, usize, Placeholder)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(open) = link[from..].find('{').map(|ix| from + ix) {
        let Some(close) = link[open..].find('}').map(|ix| open + ix) else {
            break;
        };
        // A `{` inside is the real start: `{{query}`.
        let inner = &link[open + 1..close];
        if let Some(nested) = inner.rfind('{') {
            from = open + 1 + nested;
            continue;
        }
        if let Some(placeholder) = parse(inner) {
            found.push((open, close + 1, placeholder));
        }
        from = close + 1;
    }
    found
}

/// The inside of one `{…}`, if it's a placeholder.
fn parse(inner: &str) -> Option<Placeholder> {
    let inner = inner.trim();
    let (word, mut rest) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
    let simple = match word {
        "date" => Some(Placeholder::Date),
        "time" => Some(Placeholder::Time),
        "uuid" => Some(Placeholder::Uuid),
        _ => None,
    };
    if let Some(simple) = simple {
        return rest.trim().is_empty().then_some(simple);
    }
    if word != "query" && word != "argument" {
        return None;
    }
    let mut argument = Argument {
        name: QUERY.to_owned(),
        default: String::new(),
        options: Vec::new(),
    };
    // `key="value"` pairs.
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let (key, after) = rest.split_once('=')?;
        let after = after.trim_start().strip_prefix('"')?;
        let (value, after) = after.split_once('"')?;
        match key.trim() {
            "name" if !value.trim().is_empty() => argument.name = value.trim().to_owned(),
            "default" => argument.default = value.to_owned(),
            "options" => {
                argument.options = value
                    .split(',')
                    .map(str::trim)
                    .filter(|option| !option.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
            _ => return None,
        }
        rest = after;
    }
    Some(Placeholder::Argument(argument))
}

/// The arguments `link` asks for, each name once, in order of appearance.
/// The first mention of a name sets its default and options.
pub fn arguments(link: &str) -> Vec<Argument> {
    let mut arguments: Vec<Argument> = Vec::new();
    for (_, _, placeholder) in placeholders(link) {
        if let Placeholder::Argument(argument) = placeholder
            && !arguments.iter().any(|known| known.name == argument.name)
        {
            arguments.push(argument);
        }
    }
    arguments
}

/// `link` with argument `i` (in [`arguments`] order) replaced by
/// `{field_id(i)}`, so a form can fill it. Other placeholders stay.
pub fn with_field_ids(link: &str) -> String {
    let names: Vec<String> = arguments(link).into_iter().map(|arg| arg.name).collect();
    replace_placeholders(link, |placeholder| match placeholder {
        Placeholder::Argument(argument) => {
            let ix = names.iter().position(|name| *name == argument.name)?;
            Some(format!("{{{}}}", field_id(ix)))
        }
        _ => None,
    })
}

/// The form field id of argument `ix`.
pub fn field_id(ix: usize) -> String {
    format!("arg{ix}")
}

/// `link` with its arguments set to `values` (in [`arguments`] order; missing
/// or empty ones take their default, or their first option), encoded for a
/// URL when `link` is one.
pub fn fill(link: &str, values: &[String]) -> String {
    let arguments = arguments(link);
    let url = is_url(link);
    replace_placeholders(link, |placeholder| match placeholder {
        Placeholder::Argument(argument) => {
            let ix = arguments
                .iter()
                .position(|known| known.name == argument.name)?;
            let value = values
                .get(ix)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| arguments[ix].fallback());
            Some(encode_value(value, url))
        }
        _ => None,
    })
}

impl Argument {
    /// The value used when none is typed.
    pub fn fallback(&self) -> &str {
        match self.options.first() {
            Some(first) if self.default.is_empty() => first,
            _ => &self.default,
        }
    }

    pub fn is_free_text(&self) -> bool {
        self.options.is_empty()
    }
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

/// `link` with `{date}`, `{time}` and `{uuid}` filled in.
pub fn expand_moment(link: &str, moment: &Moment) -> String {
    replace_placeholders(link, |placeholder| match placeholder {
        Placeholder::Date => Some(moment.date.clone()),
        Placeholder::Time => Some(moment.time.clone()),
        Placeholder::Uuid => Some(moment.uuid.clone()),
        Placeholder::Argument(_) => None,
    })
}

/// Whether `link` contains `{date}`, `{time}` or `{uuid}`.
pub fn has_moment(link: &str) -> bool {
    placeholders(link)
        .iter()
        .any(|(_, _, placeholder)| !matches!(placeholder, Placeholder::Argument(_)))
}

/// Replaces each placeholder `replacement` returns text for.
fn replace_placeholders(
    link: &str,
    mut replacement: impl FnMut(&Placeholder) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(link.len());
    let mut last = 0;
    for (start, end, placeholder) in placeholders(link) {
        if let Some(text) = replacement(&placeholder) {
            out.push_str(&link[last..start]);
            out.push_str(&text);
            last = end;
        }
    }
    out.push_str(&link[last..]);
    out
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
    fn finds_arguments() {
        assert_eq!(arguments("https://example.com"), []);
        let args = arguments("https://x.com/?q={query}&again={argument}");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, "Query");

        let args = arguments(
            r#"https://x.com/{argument name="Repo" default="zed"}/{argument name="Tab" options="code, issues,"}?r={argument name="Repo"}"#,
        );
        let names: Vec<_> = args.iter().map(|arg| arg.name.as_str()).collect();
        assert_eq!(names, ["Repo", "Tab"]);
        assert_eq!(args[0].default, "zed");
        assert_eq!(args[1].options, ["code", "issues"]);
    }

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
            uuid: "u".into(),
        };
        let link = r"C:\Notes\{date} {time} {uuid} {query}.md";
        assert!(has_moment(link));
        assert!(!has_moment("https://x.com/{query}"));
        assert_eq!(
            expand_moment(link, &moment),
            r"C:\Notes\2026-10-23 14:05 u {query}.md"
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
