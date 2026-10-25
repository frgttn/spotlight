//! Quicklinks: links the user saves under a name (a URL, a folder, an app's
//! deeplink), with placeholders filled in when they're opened.
//!
//! Placeholders, as in Raycast (see [`crate::placeholder`]): `{query}`,
//! `{argument …}`, `{date}`, `{time}`, `{datetime}`, `{day}`, `{uuid}`,
//! `{selection}` and `{clipboard}`. Anything else in braces is left as it is.

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
/// start from (and to delete). Name, keyword, link. Mirrors
/// `quicklinks/quicklinks-curated.csv`, one section per category.
pub const DEFAULTS: &[(&str, &str, &str)] = &[
    // Search
    ("Google", "g", "https://www.google.com/search?q={query}"),
    (
        "Google Images",
        "gi",
        "https://www.google.com/search?tbm=isch&q={query}",
    ),
    ("DuckDuckGo", "ddg", "https://duckduckgo.com/?q={query}"),
    ("Bing", "b", "https://bing.com/search?q={query}"),
    ("Kagi", "kagi", "https://kagi.com/search?q={query}"),
    (
        "Brave Search",
        "brave",
        "https://search.brave.com/search?q={query}",
    ),
    (
        "Internet Archive",
        "ia",
        "https://archive.org/search?query={query}",
    ),
    (
        "Wayback Machine",
        "wb",
        "https://web.archive.org/web/*/{query}",
    ),
    (
        "Wolfram Alpha",
        "wa",
        "https://wolframalpha.com/input?i={query}",
    ),
    // AI
    ("ChatGPT", "chatgpt", "https://chatgpt.com/?prompt={query}"),
    ("Claude", "claude", "https://claude.ai/new?q={query}"),
    ("Perplexity", "pplx", "https://perplexity.ai/?q={query}"),
    (
        "Hugging Face",
        "hf",
        "https://huggingface.co/models?search={query}",
    ),
    (
        "Kaggle",
        "kaggle",
        "https://www.kaggle.com/search?q={query}",
    ),
    // Reference
    (
        "Wikipedia",
        "w",
        "https://en.wikipedia.org/w/index.php?search={query}",
    ),
    (
        "Wiktionary",
        "wt",
        "https://en.wiktionary.org/w/index.php?search={query}&title=Special:Search&go=Go",
    ),
    (
        "Wikidata",
        "wd",
        "https://www.wikidata.org/w/index.php?search={query}",
    ),
    (
        "Merriam-Webster",
        "mw",
        "https://www.merriam-webster.com/dictionary/{query}",
    ),
    (
        "Cambridge Dictionary",
        "cam",
        "https://dictionary.cambridge.org/search/english/direct/?q={query}",
    ),
    (
        "Etymonline",
        "etym",
        "https://www.etymonline.com/search?q={query}",
    ),
    (
        "Urban Dictionary",
        "ud",
        "https://www.urbandictionary.com/define.php?term={query}",
    ),
    (
        "Thesaurus.com",
        "thesaurus",
        "https://thesaurus.com/browse/{query}",
    ),
    (
        "Google Translate",
        "tr",
        "https://translate.google.com/?sl=auto&text={argument name=\"Text\"}&tl={argument name=\"To\" options=\"en,ru,uk,de,fr,es\"}",
    ),
    (
        "DeepL",
        "deepl",
        "https://www.deepl.com/translator#auto/en/{query}",
    ),
    // Maps & Travel
    (
        "Google Maps",
        "maps",
        "https://www.google.com/maps/search/{query}",
    ),
    (
        "OpenStreetMap",
        "osm",
        "https://www.openstreetmap.org/search?query={query}",
    ),
    (
        "Booking.com",
        "booking",
        "https://www.booking.com/searchresults.html?ss={query}&si=ai",
    ),
    ("Airbnb", "airbnb", "https://www.airbnb.com/s/{query}"),
    (
        "Tripadvisor",
        "ta",
        "https://www.tripadvisor.com/Search?q={query}",
    ),
    (
        "Kayak",
        "kayak",
        "https://www.kayak.com/sherlock/opensearch/search?q={query}",
    ),
    (
        "Flightradar24",
        "fr24",
        "https://www.flightradar24.com/data/flights/{query}",
    ),
    ("Weather (wttr.in)", "wttr", "https://wttr.in/{query}"),
    // Dev Q&A
    ("GitHub", "gh", "https://github.com/search?q={query}"),
    (
        "Stack Overflow",
        "so",
        "https://stackoverflow.com/search?q={query}",
    ),
    ("Super User", "su", "https://superuser.com/search?q={query}"),
    (
        "Server Fault",
        "sf",
        "https://serverfault.com/search?q={query}",
    ),
    ("Hacker News", "hn", "https://hn.algolia.com/?q={query}"),
    ("Reddit", "r", "https://www.reddit.com/search?q={query}"),
    ("DEV Community", "devto", "https://dev.to/search?q={query}"),
    ("Medium", "medium", "https://medium.com/search?q={query}"),
    ("GitLab", "gl", "https://gitlab.com/search?search={query}"),
    // Dev Docs
    (
        "MDN Web Docs",
        "mdn",
        "https://developer.mozilla.org/en-US/search?q={query}",
    ),
    ("DevDocs", "dd", "https://devdocs.io/#q={query}"),
    (
        "Microsoft Learn",
        "msdn",
        "https://learn.microsoft.com/en-us/search?terms={query}",
    ),
    (
        "docs.rs",
        "docs",
        "https://docs.rs/releases/search?query={query}",
    ),
    (
        "Rust std docs",
        "rust",
        "https://doc.rust-lang.org/std/?search={query}",
    ),
    (
        "Python Docs",
        "py",
        "https://docs.python.org/3/search.html?q={query}",
    ),
    ("Kotlin Docs", "kotlin", "https://kotlinlang.org/?q={query}"),
    (
        "Android Developers",
        "android",
        "https://developer.android.com/s/results/?q={query}",
    ),
    ("Go packages", "go", "https://pkg.go.dev/search?q={query}"),
    ("React Docs", "react", "https://react.dev/?q={query}"),
    (
        "Can I use",
        "caniuse",
        "https://caniuse.com/?search={query}",
    ),
    (
        "tldr pages",
        "tldr",
        "https://tldr.inbrowser.app/search?query={query}",
    ),
    (
        "Man pages (Arch)",
        "man",
        "https://man.archlinux.org/search?q={query}&go=Go",
    ),
    (
        "Arch Wiki",
        "aw",
        "https://wiki.archlinux.org/index.php?title=Special:Search&search={query}",
    ),
    // Packages
    ("crates.io", "crates", "https://crates.io/search?q={query}"),
    ("lib.rs", "librs", "https://lib.rs/search?q={query}"),
    ("npm", "npm", "https://www.npmjs.com/search?q={query}"),
    ("PyPI", "pypi", "https://pypi.org/search/?q={query}"),
    (
        "NuGet",
        "nuget",
        "https://nuget.org/packages?q={query}&sortOrder=package-download-count",
    ),
    (
        "Maven Repository",
        "mvn",
        "https://mvnrepository.com/search?q={query}",
    ),
    (
        "Packagist",
        "packagist",
        "https://packagist.org/search/?q={query}",
    ),
    (
        "RubyGems",
        "gem",
        "https://rubygems.org/search?query={query}",
    ),
    (
        "Docker Hub",
        "docker",
        "https://hub.docker.com/search?q={query}",
    ),
    (
        "Homebrew",
        "brew",
        "https://formulae.brew.sh/search?q={query}",
    ),
    (
        "Chocolatey",
        "choco",
        "https://chocolatey.org/packages?q={query}",
    ),
    (
        "winget.run",
        "winget",
        "https://winget.run/search?query={query}",
    ),
    (
        "Flathub",
        "flathub",
        "https://flathub.org/apps/search?q={query}",
    ),
    (
        "Arch User Repository",
        "aur",
        "https://aur.archlinux.org/packages/?K={query}",
    ),
    (
        "Debian Packages",
        "deb",
        "https://packages.debian.org/search?keywords={query}",
    ),
    (
        "Fedora Packages",
        "fedora",
        "https://packages.fedoraproject.org/search?query={query}",
    ),
    (
        "Arch Packages",
        "archpkg",
        "https://archlinux.org/packages/?q={query}",
    ),
    // Science
    (
        "Google Scholar",
        "scholar",
        "https://scholar.google.com/scholar?&q={query}",
    ),
    (
        "arXiv",
        "arxiv",
        "https://arxiv.org/search?query={query}&searchtype=all",
    ),
    (
        "PubMed",
        "pubmed",
        "https://pubmed.ncbi.nlm.nih.gov/?term={query}",
    ),
    ("DOI", "doi", "https://doi.org/{query}"),
    // Media
    (
        "YouTube",
        "yt",
        "https://www.youtube.com/results?search_query={query}",
    ),
    (
        "YouTube Music",
        "ytm",
        "https://music.youtube.com/search?q={query}",
    ),
    (
        "Spotify",
        "spotify",
        "https://open.spotify.com/search/{query}",
    ),
    (
        "Genius Lyrics",
        "genius",
        "https://genius.com/search?q={query}",
    ),
    (
        "Discogs",
        "discogs",
        "https://www.discogs.com/search?q={query}&type=all",
    ),
    ("IMDb", "imdb", "https://www.imdb.com/find?s=all&q={query}"),
    (
        "TMDB",
        "tmdb",
        "https://www.themoviedb.org/search?query={query}",
    ),
    (
        "Rotten Tomatoes",
        "rt",
        "https://www.rottentomatoes.com/search/?search={query}",
    ),
    (
        "MyAnimeList",
        "mal",
        "https://myanimelist.net/anime.php?q={query}",
    ),
    (
        "Goodreads",
        "gr",
        "https://www.goodreads.com/search?q={query}",
    ),
    ("Cobalt", "co", "https://cobalt.tools/#{query}"),
    // Design
    (
        "Unsplash",
        "unsplash",
        "https://unsplash.com/search?utf8=%E2%9C%93&keyword={query}",
    ),
    ("Pexels", "pexels", "https://www.pexels.com/search/{query}/"),
    ("Giphy", "giphy", "https://giphy.com/search/{query}"),
    (
        "Flaticon",
        "flaticon",
        "https://www.flaticon.com/search?word={query}",
    ),
    (
        "SVG Repo",
        "svgrepo",
        "https://www.svgrepo.com/vectors/{query}/",
    ),
    (
        "The Noun Project",
        "noun",
        "https://thenounproject.com/search/?q={query}",
    ),
    (
        "Dribbble",
        "dribbble",
        "https://dribbble.com/search?q={query}",
    ),
    (
        "Google Fonts",
        "gf",
        "https://fonts.google.com/?query={query}",
    ),
    // Shopping
    ("Amazon", "a", "https://www.amazon.com/s?k={query}"),
    (
        "eBay",
        "ebay",
        "https://www.ebay.com/sch/items/?_nkw={query}",
    ),
    (
        "AliExpress",
        "ali",
        "https://www.aliexpress.com/wholesale?SearchText={query}",
    ),
    ("Etsy", "etsy", "https://www.etsy.com/search?q={query}"),
    (
        "Newegg",
        "newegg",
        "https://www.newegg.com/Product/ProductList.aspx?Submit=ENE&DEPA=0&Description={query}",
    ),
    // Games
    (
        "Steam",
        "steam",
        "https://store.steampowered.com/search/?term={query}",
    ),
    (
        "Steam DB",
        "steamdb",
        "https://steamdb.info/search/?a=app&q={query}",
    ),
    ("GOG", "gog", "https://www.gog.com/games?query={query}"),
    ("itch.io", "itch", "https://itch.io/search?q={query}"),
    (
        "HowLongToBeat",
        "hltb",
        "https://howlongtobeat.com/?q={query}",
    ),
    // Finance
    (
        "Yahoo Finance",
        "yf",
        "https://finance.yahoo.com/quote/{query}",
    ),
    (
        "CoinMarketCap",
        "cmc",
        "https://coinmarketcap.com/search/?q={query}",
    ),
    (
        "CoinGecko",
        "coingecko",
        "https://www.coingecko.com/en/search/bang?query={query}",
    ),
    (
        "Investing.com",
        "investing",
        "https://www.investing.com/search/?q={query}",
    ),
    // Social
    ("X", "x", "https://x.com/search?q={query}"),
    (
        "LinkedIn",
        "li",
        "https://www.linkedin.com/search/results/all/?keywords={query}",
    ),
    ("Quora", "quora", "https://www.quora.com/search?q={query}"),
    (
        "Product Hunt",
        "ph",
        "https://www.producthunt.com/search?q={query}",
    ),
    (
        "AlternativeTo",
        "alternativeto",
        "https://alternativeto.net/browse/search/?q={query}",
    ),
    // Google Workspace
    (
        "Gmail",
        "gmail",
        "https://mail.google.com/mail/#search/{query}",
    ),
    (
        "Google Docs",
        "gd",
        "https://docs.google.com/document/u/0/?q={query}",
    ),
    (
        "Google Calendar",
        "gcal",
        "https://calendar.google.com/calendar/b/0/r/search?q={query}",
    ),
    (
        "Google Drive",
        "drive",
        "https://drive.google.com/drive/search?q={query}",
    ),
    // Security
    (
        "VirusTotal",
        "vt",
        "https://www.virustotal.com/gui/search?query={query}",
    ),
    (
        "Shodan",
        "shodan",
        "https://www.shodan.io/search?query={query}",
    ),
    ("crt.sh", "crt", "https://crt.sh/?q={query}"),
    (
        "urlscan.io",
        "urlscan",
        "https://urlscan.io/search/#{query}",
    ),
    (
        "NVD",
        "nvd",
        "https://nvd.nist.gov/vuln/search/results?form_type=Basic&results_type=overview&query={query}&search_type=all",
    ),
    (
        "CVE",
        "cve",
        "https://www.cve.org/CVERecord/SearchResults?query={query}",
    ),
    (
        "Exploit-DB",
        "edb",
        "https://www.exploit-db.com/search/?action=search&description={query}",
    ),
    ("WHOIS", "whois", "https://whois.domaintools.com/{query}"),
    (
        "MXToolbox DNS",
        "dns",
        "https://mxtoolbox.com/SuperTool.aspx?action=a:{query}&run=toolpage",
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
    fill_with(link, &Placeholder::Selection, text)
}

/// `link` as a web search template (see [`crate::web`]): `{query}` where its
/// first free-text argument goes, the other arguments at their defaults.
/// `None` unless it is an `http(s)` URL with such an argument.
pub fn search_template(link: &str) -> Option<String> {
    // Alphanumeric, so `fill` leaves it as it is.
    const MARK: &str = "SPOTLIGHTQUERYMARK";
    let arguments = arguments(link);
    if !arguments.iter().any(Argument::is_free_text) {
        return None;
    }
    let template = fill(link, &bang_values(&arguments, MARK)).replace(MARK, "{query}");
    crate::web::check_template(&template)
        .ok()
        .map(|()| template)
}

/// Whether `link` contains `{clipboard}`.
pub fn has_clipboard(link: &str) -> bool {
    placeholder::contains(link, |placeholder| *placeholder == Placeholder::Clipboard)
}

/// `link` with `{clipboard}` replaced by `text`, encoded as typed values are.
pub fn fill_clipboard(link: &str, text: &str) -> String {
    fill_with(link, &Placeholder::Clipboard, text)
}

fn fill_with(link: &str, which: &Placeholder, text: &str) -> String {
    let value = encode_value(text.trim(), is_url(link));
    placeholder::replace(link, |placeholder| {
        (placeholder == which).then(|| value.clone())
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
            now: crate::datetime::LocalTime {
                year: 2026,
                month: 10,
                day: 23,
                hour: 14,
                minute: 5,
                second: 0,
            },
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
    fn makes_search_templates() {
        assert_eq!(
            search_template("https://github.com/search?q={query}&type=repositories").as_deref(),
            Some("https://github.com/search?q={query}&type=repositories")
        );
        assert_eq!(
            search_template(
                "https://x.com/{argument name=\"Lang\" options=\"en, de\"}/{argument name=\"Text\"}"
            )
            .as_deref(),
            Some("https://x.com/en/{query}")
        );
        assert_eq!(search_template("https://x.com/home"), None);
        assert_eq!(search_template(r"C:\Notes\{query}.md"), None);
    }

    #[test]
    fn fills_the_clipboard() {
        let link = "https://translate.google.com/?text={clipboard}&q={query}";
        assert!(has_clipboard(link));
        assert!(!has_clipboard("https://x.com/{selection}"));
        assert_eq!(
            fill_clipboard(link, " a&b c\n"),
            "https://translate.google.com/?text=a%26b%20c&q={query}"
        );
        assert_eq!(
            fill_clipboard(r"C:\Notes\{clipboard}.md", "todo"),
            r"C:\Notes\todo.md"
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
