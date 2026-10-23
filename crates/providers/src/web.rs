//! Opening URLs typed into the search field, and web search as the fallback.

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};

use crate::glyph;

pub const ID: &str = "web";

const SEARCH_URL: &str = "https://www.google.com/search?q=";

/// Top-level domains recognised without a scheme. A list rather than "any
/// letters", so file names (`notes.md`, `main.rs`, `setup.exe`) don't look
/// like sites.
const TLDS: &[&str] = &[
    "com", "net", "org", "edu", "gov", "io", "dev", "app", "ai", "co", "me", "info", "biz", "xyz",
    "tech", "site", "online", "store", "shop", "blog", "cloud", "page", "wiki", "news", "link",
    "live", "tv", "fm", "gg", "so", "to", "ly", "im", "is", "eu", "us", "uk", "ca", "au", "nz",
    "de", "fr", "it", "es", "pt", "nl", "be", "ch", "at", "pl", "cz", "sk", "se", "no", "fi", "dk",
    "ee", "lv", "lt", "ua", "ru", "by", "kz", "ge", "am", "tr", "il", "jp", "cn", "kr", "tw", "hk",
    "sg", "in", "id", "br", "mx", "ar", "cl", "za",
];

pub struct Web;

impl Provider for Web {
    fn id(&self) -> &'static str {
        ID
    }

    fn answers(&self, query: &str) -> Vec<SearchItem> {
        let Some(url) = as_url(query) else {
            return Vec::new();
        };
        vec![
            SearchItem::new(item_id(ID, "url"), format!("Open {url}"))
                .subtitle("URL")
                .icon(Icon::Glyph(glyph::GLOBE))
                .action("Open in Browser", Command::OpenUrl(url.clone()))
                .action("Copy URL", Command::Copy(url)),
        ]
    }

    fn fallbacks(&self, query: &str) -> Vec<SearchItem> {
        vec![
            SearchItem::new(
                item_id(ID, "search"),
                format!("Search Google for “{query}”"),
            )
            .subtitle("Web Search")
            .icon(Icon::Glyph(glyph::SEARCH))
            .action(
                "Search in Browser",
                Command::OpenUrl(format!("{SEARCH_URL}{}", encode_query(query))),
            ),
        ]
    }
}

/// `query` as an `http(s)` URL, if it reads as one: an explicit `http(s)://`
/// URL, a host name with a known top-level domain, `localhost` or an IPv4
/// address, each optionally with a port and a path.
pub fn as_url(query: &str) -> Option<String> {
    let query = query.trim();
    if query.is_empty() || query.contains(char::is_whitespace) {
        return None;
    }
    let lower = query.to_ascii_lowercase();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = lower.strip_prefix(scheme) {
            return (!rest.is_empty()).then(|| query.to_owned());
        }
    }
    if lower.contains("://") {
        return None;
    }

    let host_and_port = lower.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match host_and_port.split_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            host
        }
        None => host_and_port,
    };
    if host == "localhost" || is_ipv4(host) {
        return Some(format!("http://{query}"));
    }

    let labels: Vec<&str> = host.split('.').collect();
    let valid_labels = labels.iter().all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    });
    let known_tld = labels.last().is_some_and(|tld| TLDS.contains(tld));
    (labels.len() >= 2 && valid_labels && known_tld).then(|| format!("https://{query}"))
}

fn is_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.len() <= 3 && part.parse::<u8>().is_ok())
}

/// Percent-encodes `query` for a URL query string (spaces become `+`).
pub fn encode_query(query: &str) -> String {
    let mut encoded = String::with_capacity(query.len());
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_urls() {
        let cases = [
            ("github.com", "https://github.com"),
            (
                "GitHub.com/zed-industries/zed",
                "https://GitHub.com/zed-industries/zed",
            ),
            (
                "crates.io/crates/gpui?x=1#top",
                "https://crates.io/crates/gpui?x=1#top",
            ),
            ("http://example.org", "http://example.org"),
            ("localhost:3000", "http://localhost:3000"),
            ("192.168.1.1/admin", "http://192.168.1.1/admin"),
            ("sub.example.co.uk", "https://sub.example.co.uk"),
        ];
        for (query, url) in cases {
            assert_eq!(as_url(query).as_deref(), Some(url), "{query:?}");
        }
    }

    #[test]
    fn leaves_other_text_alone() {
        for query in [
            "chrome",
            "notes.md",
            "setup.exe",
            "main.rs",
            "1.5",
            "example .com",
            "https://",
            "ftp://example.com",
            "file:///C:/Windows",
            "example.com:abc",
            "-bad.com",
            "999.1.1.1",
        ] {
            assert_eq!(as_url(query), None, "{query:?}");
        }
    }

    #[test]
    fn encodes_search_queries() {
        assert_eq!(encode_query("rust gpui"), "rust+gpui");
        assert_eq!(encode_query("a&b=c?"), "a%26b%3Dc%3F");
        assert_eq!(
            encode_query("привет"),
            "%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82"
        );
    }

    #[test]
    fn falls_back_to_web_search() {
        let items = Web.fallbacks("rust gpui");
        assert_eq!(
            items[0].actions[0].command,
            Command::OpenUrl("https://www.google.com/search?q=rust+gpui".into())
        );
    }
}
