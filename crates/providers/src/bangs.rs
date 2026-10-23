//! Search bangs (`!gh tokio`), the same list the Helium browser uses:
//! [Kagi's bangs](https://github.com/kagisearch/bangs) (MIT) plus Helium's
//! own additions, about ten thousand sites.
//!
//! Helium serves the list as one JSON file. It is downloaded, kept in
//! `bangs.json` in the data folder and refreshed once it is older than
//! [`MAX_AGE`], like the exchange rates. Until the first download succeeds,
//! only the user's own quicklinks have bangs.

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use spotlight_core::web::encode_query;
use spotlight_platform_win::net;

const URL: &str = "https://services.helium.imput.net/bangs.json";
/// The service asks clients to cache it for a day.
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// After a failed fetch (offline), don't try again for this long.
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
/// Where the query goes in a bang's URL.
const PLACEHOLDER: &str = "{searchTerms}";
pub const ATTRIBUTION: &str = "Bangs from Kagi and Helium";

// Format flags (`f`). A bang without them has all of them.
/// With an empty query, open the site rather than its search page.
const OPEN_BASE_PATH: u32 = 1 << 0;
/// Percent-encode the query.
const URL_ENCODE_PLACEHOLDER: u32 = 1 << 2;
/// Encode spaces as `+` rather than `%20`.
const URL_ENCODE_SPACE_TO_PLUS: u32 = 1 << 3;
const DEFAULT_FLAGS: u32 = OPEN_BASE_PATH | URL_ENCODE_PLACEHOLDER | URL_ENCODE_SPACE_TO_PLUS;

/// One site's bang.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bang {
    /// The site: "GitHub".
    pub name: String,
    /// Lowercase, shortest first: `["gh", "git", "github"]`.
    pub triggers: Vec<String>,
    /// With `{searchTerms}` where the query goes.
    template: String,
    flags: u32,
}

impl Bang {
    /// The page to open for `query`.
    pub fn url(&self, query: &str) -> String {
        let query = query.trim();
        if query.is_empty() && self.flags & OPEN_BASE_PATH != 0 {
            return self.home();
        }
        let value = if self.flags & URL_ENCODE_PLACEHOLDER == 0 {
            query.to_owned()
        } else if self.flags & URL_ENCODE_SPACE_TO_PLUS != 0 {
            encode_query(query)
        } else {
            encode_query(query).replace('+', "%20")
        };
        self.template.replace(PLACEHOLDER, &value)
    }

    /// The site itself: `https://github.com/`.
    pub fn home(&self) -> String {
        let Some((scheme, rest)) = self.template.split_once("://") else {
            return self.template.replace(PLACEHOLDER, "");
        };
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        format!("{scheme}://{host}/")
    }

    /// The template as a quicklink link, with `{query}`.
    pub fn quicklink(&self) -> String {
        self.template.replace(PLACEHOLDER, "{query}")
    }

    /// `github.com`, to find a bang by its site.
    pub fn host(&self) -> &str {
        let rest = self
            .template
            .split_once("://")
            .map_or(self.template.as_str(), |(_, rest)| rest);
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        host.strip_prefix("www.").unwrap_or(host)
    }
}

#[derive(Default)]
struct List {
    bangs: Vec<Bang>,
    /// Trigger → index into `bangs`.
    by_trigger: HashMap<String, usize>,
    /// When `bangs` was fetched; `None` if never.
    fetched: Option<SystemTime>,
    last_attempt: Option<SystemTime>,
}

impl List {
    fn new(bangs: Vec<Bang>, fetched: Option<SystemTime>) -> Self {
        let by_trigger = bangs
            .iter()
            .enumerate()
            .flat_map(|(ix, bang)| {
                bang.triggers
                    .iter()
                    .map(move |trigger| (trigger.clone(), ix))
            })
            .collect();
        Self {
            bangs,
            by_trigger,
            fetched,
            last_attempt: None,
        }
    }
}

pub struct Bangs {
    /// `None`: no data folder; the list is fetched but not kept.
    cache: Option<PathBuf>,
    list: RwLock<List>,
    fetching: AtomicBool,
}

impl Bangs {
    /// Empty until [`Self::load`] runs.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self {
            cache: data_dir.map(|dir| dir.join("bangs.json")),
            list: RwLock::new(List::default()),
            fetching: AtomicBool::new(false),
        }
    }

    /// A fixed list in Helium's format, for tests.
    #[cfg(test)]
    pub fn from_json(json: &str) -> Self {
        Self::fixed(parse(json).unwrap())
    }

    #[cfg(test)]
    fn fixed(bangs: Vec<Bang>) -> Self {
        Self {
            cache: None,
            list: RwLock::new(List::new(bangs, Some(SystemTime::now()))),
            fetching: AtomicBool::new(false),
        }
    }

    /// Reads the cached list, then downloads a new one if it's old. Blocking:
    /// for the catalog thread.
    pub fn load(&self) {
        let empty = self.list.read().is_ok_and(|list| list.bangs.is_empty());
        if empty
            && let Some(path) = &self.cache
            && let Ok(body) = fs::read_to_string(path)
        {
            match parse(&body) {
                Ok(bangs) => {
                    let fetched = fs::metadata(path).and_then(|meta| meta.modified()).ok();
                    if let Ok(mut list) = self.list.write() {
                        *list = List::new(bangs, fetched);
                    }
                }
                Err(err) => eprintln!("[bangs] ignoring unreadable {}: {err}", path.display()),
            }
        }
        if self.is_stale() && !self.fetching.swap(true, Ordering::AcqRel) {
            self.fetch();
            self.fetching.store(false, Ordering::Release);
        }
    }

    fn is_stale(&self) -> bool {
        let Ok(list) = self.list.read() else {
            return false;
        };
        let older_than = |time: Option<SystemTime>, age: Duration| {
            time.is_none_or(|time| time.elapsed().unwrap_or_default() > age)
        };
        older_than(list.fetched, MAX_AGE) && older_than(list.last_attempt, RETRY_AFTER)
    }

    fn fetch(&self) {
        if let Ok(mut list) = self.list.write() {
            list.last_attempt = Some(SystemTime::now());
        }
        let fetched = net::get_text(URL).and_then(|body| {
            let bangs = parse(&body)?;
            Ok((body, bangs))
        });
        match fetched {
            Ok((body, bangs)) => {
                eprintln!("[bangs] {} sites", bangs.len());
                if let Some(path) = &self.cache
                    && let Err(err) = fs::write(path, body)
                {
                    eprintln!("[bangs] could not save {}: {err}", path.display());
                }
                if let Ok(mut list) = self.list.write() {
                    *list = List::new(bangs, Some(SystemTime::now()));
                }
            }
            Err(err) => eprintln!("[bangs] could not fetch: {err}"),
        }
    }

    /// The bang `trigger` (lowercase) opens.
    pub fn find(&self, trigger: &str) -> Option<Bang> {
        let list = self.list.read().ok()?;
        list.by_trigger
            .get(trigger)
            .map(|&ix| list.bangs[ix].clone())
    }

    /// Up to `limit` bangs with a trigger starting with `prefix`, each with the
    /// shortest such trigger: an exact match first, then shorter triggers.
    /// `skip` hides triggers the user's own quicklinks have taken.
    pub fn starting_with(
        &self,
        prefix: &str,
        limit: usize,
        skip: impl Fn(&str) -> bool,
    ) -> Vec<(String, Bang)> {
        let Ok(list) = self.list.read() else {
            return Vec::new();
        };
        let mut found: Vec<(&str, usize)> = list
            .bangs
            .iter()
            .enumerate()
            .filter_map(|(ix, bang)| {
                // Triggers are sorted shortest first.
                let trigger = bang
                    .triggers
                    .iter()
                    .find(|trigger| trigger.starts_with(prefix) && !skip(trigger))?;
                Some((trigger.as_str(), ix))
            })
            .collect();
        found.sort_unstable_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(b.0)));
        found
            .into_iter()
            .take(limit)
            .map(|(trigger, ix)| (trigger.to_owned(), list.bangs[ix].clone()))
            .collect()
    }

    /// Every bang, in the list's order (most common triggers first).
    pub fn all(&self) -> Vec<Bang> {
        self.list
            .read()
            .map(|list| list.bangs.clone())
            .unwrap_or_default()
    }
}

/// Helium's file: `//` comment lines with the licence, then a JSON array of
/// `{"s": name, "ts": [triggers], "u": template, "f": flags}`, where arrays
/// may end in a trailing comma.
fn parse(body: &str) -> Result<Vec<Bang>, String> {
    let json = body
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let json = strip_trailing_commas(&json);
    let value: serde_json::Value = serde_json::from_str(&json).map_err(|err| err.to_string())?;
    let entries = value.as_array().ok_or("not a list of bangs")?;
    let bangs: Vec<Bang> = entries
        .iter()
        .filter_map(|entry| {
            let template = entry["u"].as_str()?;
            if !template.contains(PLACEHOLDER) {
                return None;
            }
            let triggers: Vec<String> = entry["ts"]
                .as_array()?
                .iter()
                .filter_map(|trigger| Some(trigger.as_str()?.to_lowercase()))
                .collect();
            if triggers.is_empty() {
                return None;
            }
            Some(Bang {
                name: entry["s"].as_str()?.trim().to_owned(),
                triggers,
                template: template.to_owned(),
                flags: entry["f"]
                    .as_u64()
                    .map_or(DEFAULT_FLAGS, |flags| flags as u32),
            })
        })
        .collect();
    if bangs.is_empty() {
        return Err("no bangs in the list".into());
    }
    Ok(bangs)
}

/// Removes commas right before `]` or `}` outside strings, which JSON doesn't allow.
fn strip_trailing_commas(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escaped = false;
    for (ix, ch) in json.char_indices() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if ch == '"' {
            in_string = true;
        } else if ch == ',' && json[ix + 1..].trim_start().starts_with([']', '}']) {
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"// Generated at 2026-10-14T11:11:02.825Z
// MIT License
//
[
 {
  "s": "GitHub",
  "ts": [
   "gh",
   "git",
   "github",
  ],
  "u": "https://github.com/search?q={searchTerms}"
 },
 {
  "s": "Raw, \"quoted\"",
  "ts": [ "raw" ],
  "f": 1,
  "u": "https://raw.example/{searchTerms}/x,]"
 },
 {
  "s": "Twenty",
  "ts": [ "t20" ],
  "f": 4,
  "u": "https://www.twenty.example/s?q={searchTerms}"
 },
 {
  "s": "Broken",
  "ts": [ "broken" ],
  "u": "https://broken.example/"
 }
]"#;

    fn sample() -> Bangs {
        Bangs::fixed(parse(SAMPLE).unwrap())
    }

    #[test]
    fn parses_helium_s_file() {
        let bangs = parse(SAMPLE).unwrap();
        let names: Vec<_> = bangs.iter().map(|bang| bang.name.as_str()).collect();
        assert_eq!(names, ["GitHub", "Raw, \"quoted\"", "Twenty"]);
        assert_eq!(bangs[0].triggers, ["gh", "git", "github"]);
        assert_eq!(bangs[0].flags, DEFAULT_FLAGS);
        assert!(parse("[]").is_err());
        assert!(parse("<html>").is_err());
    }

    #[test]
    fn builds_urls_by_their_flags() {
        let bangs = sample();
        let github = bangs.find("git").unwrap();
        assert_eq!(
            github.url("rust gpui"),
            "https://github.com/search?q=rust+gpui"
        );
        assert_eq!(github.url("  "), "https://github.com/");
        assert_eq!(github.host(), "github.com");
        assert_eq!(github.quicklink(), "https://github.com/search?q={query}");

        let raw = bangs.find("raw").unwrap();
        assert_eq!(raw.url("a b"), "https://raw.example/a b/x,]");
        let twenty = bangs.find("t20").unwrap();
        assert_eq!(
            twenty.url("a b&c"),
            "https://www.twenty.example/s?q=a%20b%26c"
        );
        // No OPEN_BASE_PATH: an empty query searches for nothing.
        assert_eq!(twenty.url(""), "https://www.twenty.example/s?q=");
        assert_eq!(twenty.host(), "twenty.example");
        assert_eq!(bangs.find("broken"), None);
    }

    #[test]
    fn suggests_shortest_triggers_first() {
        let bangs = sample();
        let found: Vec<_> = bangs
            .starting_with("g", 5, |_| false)
            .into_iter()
            .map(|(trigger, _)| trigger)
            .collect();
        assert_eq!(found, ["gh"]);
        // A quicklink took "gh": GitHub is still offered under "git".
        let found: Vec<_> = bangs
            .starting_with("g", 5, |trigger| trigger == "gh")
            .into_iter()
            .map(|(trigger, _)| trigger)
            .collect();
        assert_eq!(found, ["git"]);
        assert!(bangs.starting_with("zz", 5, |_| false).is_empty());
    }

    #[test]
    fn strips_only_trailing_commas() {
        assert_eq!(
            strip_trailing_commas(r#"{"a": [1, 2,], "b": "x,]",}"#),
            r#"{"a": [1, 2], "b": "x,]"}"#
        );
    }

    /// Downloads the real list.
    #[test]
    #[ignore = "network"]
    fn fetches_helium_s_list() {
        let body = net::get_text(URL).unwrap();
        let bangs = parse(&body).unwrap();
        assert!(bangs.len() > 5000, "{}", bangs.len());
        let list = Bangs::fixed(bangs);
        assert_eq!(list.find("gh").unwrap().name, "GitHub");
        assert_eq!(
            list.find("w").unwrap().url("rust"),
            "https://wikipedia.org/w/index.php?search=rust"
        );
    }
}
