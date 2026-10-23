//! The web search engine behind root search's "Search … for" row.

/// A built-in search engine.
#[derive(Debug, PartialEq, Eq)]
pub struct Preset {
    /// Stored in the settings table.
    pub id: &'static str,
    pub name: &'static str,
    /// `{query}` stands for the percent-encoded query.
    pub template: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "google",
        name: "Google",
        template: "https://www.google.com/search?q={query}",
    },
    Preset {
        id: "bing",
        name: "Bing",
        template: "https://www.bing.com/search?q={query}",
    },
    Preset {
        id: "duckduckgo",
        name: "DuckDuckGo",
        template: "https://duckduckgo.com/?q={query}",
    },
    Preset {
        id: "brave",
        name: "Brave Search",
        template: "https://search.brave.com/search?q={query}",
    },
    Preset {
        id: "ecosia",
        name: "Ecosia",
        template: "https://www.ecosia.org/search?q={query}",
    },
    Preset {
        id: "perplexity",
        name: "Perplexity",
        template: "https://www.perplexity.ai/search?q={query}",
    },
];

const CUSTOM_PREFIX: &str = "custom:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEngine {
    Preset(&'static Preset),
    /// A URL template the user typed, already checked by [`check_template`].
    Custom(String),
}

impl Default for SearchEngine {
    fn default() -> Self {
        SearchEngine::Preset(&PRESETS[0])
    }
}

impl SearchEngine {
    /// "Google", or a custom engine's host ("kagi.com").
    pub fn name(&self) -> String {
        match self {
            SearchEngine::Preset(preset) => preset.name.to_owned(),
            SearchEngine::Custom(template) => host(template).to_owned(),
        }
    }

    pub fn template(&self) -> &str {
        match self {
            SearchEngine::Preset(preset) => preset.template,
            SearchEngine::Custom(template) => template,
        }
    }

    /// The results page for `query`.
    pub fn url(&self, query: &str) -> String {
        self.template().replace("{query}", &encode_query(query))
    }

    /// The form kept in the settings table: a preset id or `custom:<template>`.
    pub fn to_setting(&self) -> String {
        match self {
            SearchEngine::Preset(preset) => preset.id.to_owned(),
            SearchEngine::Custom(template) => format!("{CUSTOM_PREFIX}{template}"),
        }
    }

    /// Reads [`Self::to_setting`]'s form back; `None` for anything else.
    pub fn from_setting(text: &str) -> Option<Self> {
        if let Some(template) = text.strip_prefix(CUSTOM_PREFIX) {
            return check_template(template)
                .ok()
                .map(|()| SearchEngine::Custom(template.to_owned()));
        }
        PRESETS
            .iter()
            .find(|preset| preset.id == text)
            .map(SearchEngine::Preset)
    }
}

/// Why a custom template can't be used, in words for the settings window.
pub fn check_template(template: &str) -> Result<(), &'static str> {
    let lower = template.trim().to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("Start the URL with https://");
    }
    if !template.contains("{query}") {
        return Err("Put {query} where the search text goes");
    }
    if host(template).is_empty() {
        return Err("The URL needs a site name");
    }
    Ok(())
}

/// `https://www.kagi.com/search?q=…` → `kagi.com`.
fn host(template: &str) -> &str {
    let rest = template
        .split_once("://")
        .map_or(template, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    host.strip_prefix("www.").unwrap_or(host)
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
    fn builds_result_urls() {
        let google = SearchEngine::default();
        assert_eq!(
            google.url("rust gpui"),
            "https://www.google.com/search?q=rust+gpui"
        );
        let kagi = SearchEngine::Custom("https://kagi.com/search?q={query}&r=eu".into());
        assert_eq!(kagi.url("a&b"), "https://kagi.com/search?q=a%26b&r=eu");
        assert_eq!(kagi.name(), "kagi.com");
    }

    #[test]
    fn settings_round_trip() {
        for engine in PRESETS
            .iter()
            .map(SearchEngine::Preset)
            .chain([SearchEngine::Custom(
                "https://www.startpage.com/do/search?q={query}".into(),
            )])
        {
            assert_eq!(
                SearchEngine::from_setting(&engine.to_setting()),
                Some(engine)
            );
        }
        assert_eq!(SearchEngine::from_setting("altavista"), None);
        assert_eq!(SearchEngine::from_setting("custom:ftp://x/{query}"), None);
    }

    #[test]
    fn checks_custom_templates() {
        assert!(check_template("https://kagi.com/search?q={query}").is_ok());
        assert!(check_template("kagi.com/search?q={query}").is_err());
        assert!(check_template("https://kagi.com/search?q=").is_err());
        assert!(check_template("javascript:alert({query})").is_err());
        assert!(check_template("https:///{query}").is_err());
    }

    #[test]
    fn encodes_queries() {
        assert_eq!(encode_query("rust gpui"), "rust+gpui");
        assert_eq!(encode_query("a&b=c?"), "a%26b%3Dc%3F");
        assert_eq!(
            encode_query("привет"),
            "%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82"
        );
    }

    #[test]
    fn preset_ids_are_unique() {
        let mut ids: Vec<_> = PRESETS.iter().map(|preset| preset.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PRESETS.len());
    }
}
