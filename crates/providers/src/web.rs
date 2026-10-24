//! Opening URLs typed into the search field, and web search as the fallback.

use std::sync::{Arc, Mutex, RwLock};

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
    quicklink,
    view::{Field, FieldKind, FormView, View},
    web::{PRESETS, SearchEngine, encode_query},
};
use spotlight_store::Store;

use crate::glyph;

pub const ID: &str = "web";

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

pub struct Web {
    /// Changed from the settings window while the app runs.
    engine: RwLock<SearchEngine>,
    /// Where a quicklink engine's link is looked up.
    store: Option<Arc<Mutex<Store>>>,
    /// A quicklink engine's name and template, looked up again on every
    /// catalog load (the launcher reloads it when quicklinks change).
    quicklink: RwLock<Option<(String, String)>>,
}

impl Web {
    pub fn new(engine: SearchEngine, store: Option<Arc<Mutex<Store>>>) -> Self {
        Self {
            engine: RwLock::new(engine),
            store,
            quicklink: RwLock::new(None),
        }
    }

    pub fn set_engine(&self, engine: SearchEngine) {
        if let Ok(mut current) = self.engine.write() {
            *current = engine;
        }
        self.forget_quicklink();
    }

    pub fn engine(&self) -> SearchEngine {
        self.engine
            .read()
            .map(|engine| engine.clone())
            .unwrap_or_default()
    }

    /// The engine's name and template (`{query}` where the text goes). A
    /// quicklink that is gone or no longer searchable stands for the default.
    pub fn resolved(&self) -> (String, String) {
        let engine = self.engine();
        let SearchEngine::Quicklink(id) = engine else {
            return (engine.name(), engine.template().to_owned());
        };
        if let Some(found) = self.quicklink.read().ok().and_then(|cached| cached.clone()) {
            return found;
        }
        let found = self.look_up(id).unwrap_or_else(|| {
            let fallback = SearchEngine::default();
            (fallback.name(), fallback.template().to_owned())
        });
        if let Ok(mut cached) = self.quicklink.write() {
            *cached = Some(found.clone());
        }
        found
    }

    fn look_up(&self, id: i64) -> Option<(String, String)> {
        let links = self.store.as_ref()?.lock().ok()?.quicklinks().ok()?;
        let link = links.into_iter().find(|link| link.id == id)?;
        let template = quicklink::search_template(&link.link)?;
        Some((link.name, template))
    }

    fn forget_quicklink(&self) {
        if let Ok(mut cached) = self.quicklink.write() {
            *cached = None;
        }
    }
}

impl Provider for Web {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Web Search and URLs"
    }

    fn description(&self) -> &'static str {
        "Opens typed addresses, and offers a web search as the last result."
    }

    /// "Search the Web…": a form to search with any engine, not only the
    /// one chosen in Settings (listed first, as it was at startup).
    fn catalog(&self) -> Vec<SearchItem> {
        self.forget_quicklink();
        let (name, template) = self.resolved();
        let mut engines = vec![(name, template.clone())];
        engines.extend(
            PRESETS
                .iter()
                .filter(|preset| preset.template != template)
                .map(|preset| (preset.name.to_owned(), preset.template.to_owned())),
        );
        let form = View::Form(FormView {
            title: "Search the Web".into(),
            fields: vec![
                Field {
                    id: "query".into(),
                    label: "Search for".into(),
                    kind: FieldKind::Text {
                        placeholder: "What to look up".into(),
                        default: String::new(),
                        required: true,
                    },
                },
                Field {
                    id: "engine".into(),
                    label: "With".into(),
                    kind: FieldKind::Dropdown {
                        options: engines,
                        default: 0,
                    },
                },
            ],
            submit_title: "Search".into(),
            // The engine's template, then the query in its {query}.
            submit: Command::OpenUrl("{engine}".into()),
        });
        vec![
            SearchItem::new(item_id(ID, "search-form"), "Search the Web…")
                .subtitle("Web Search")
                .keywords(&["google", "search engine"])
                .icon(Icon::Glyph(glyph::FORM))
                .action("Open Form", Command::PushView(Box::new(form))),
        ]
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
        let (name, template) = self.resolved();
        let url = template.replace("{query}", &encode_query(query));
        vec![
            SearchItem::new(item_id(ID, "search"), format!("Search {name} for “{query}”"))
            .subtitle("Web Search")
            .icon(Icon::Glyph(glyph::SEARCH))
            .action("Search in Browser", Command::OpenUrl(url.clone()))
            .action("Copy Search URL", Command::Copy(url)),
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
    fn falls_back_to_web_search() {
        let web = Web::new(SearchEngine::default(), None);
        let items = web.fallbacks("rust gpui");
        assert_eq!(
            items[0].actions[0].command,
            Command::OpenUrl("https://www.google.com/search?q=rust+gpui".into())
        );
    }

    #[test]
    fn uses_the_chosen_engine() {
        let web = Web::new(SearchEngine::default(), None);
        web.set_engine(SearchEngine::from_setting("duckduckgo").unwrap());
        let items = web.fallbacks("rust");
        assert_eq!(items[0].title, "Search DuckDuckGo for “rust”");
        assert_eq!(
            items[0].actions[0].command,
            Command::OpenUrl("https://duckduckgo.com/?q=rust".into())
        );
    }

    #[test]
    fn searches_with_a_quicklink() {
        let store = Store::open_in_memory().unwrap();
        let link = store
            .quicklinks()
            .unwrap()
            .into_iter()
            .find(|link| link.name == "GitHub")
            .unwrap();
        let store = Arc::new(Mutex::new(store));
        let web = Web::new(SearchEngine::Quicklink(link.id), Some(store.clone()));
        let items = web.fallbacks("a b");
        assert_eq!(items[0].title, "Search GitHub for “a b”");
        assert_eq!(
            items[0].actions[0].command,
            Command::OpenUrl("https://github.com/search?q=a+b".into())
        );

        // Gone: back to the default, once the catalog is loaded again.
        store.lock().unwrap().delete_quicklink(link.id).unwrap();
        web.catalog();
        assert_eq!(web.fallbacks("x")[0].title, "Search Google for “x”");
    }

    #[test]
    fn search_form_offers_every_engine_once() {
        let web = Web::new(SearchEngine::from_setting("bing").unwrap(), None);
        let items = web.catalog();
        let Command::PushView(view) = &items[0].actions[0].command else {
            panic!("not a view");
        };
        let View::Form(form) = view.as_ref() else {
            panic!("not a form");
        };
        let FieldKind::Dropdown { options, .. } = &form.fields[1].kind else {
            panic!("not a dropdown");
        };
        assert_eq!(options[0].0, "Bing");
        assert_eq!(options.len(), PRESETS.len());
    }
}
