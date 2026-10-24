//! Where a site's icon is: the `<link rel="icon">` tags of its home page,
//! then `/favicon.ico`. Only the site itself is asked, no icon service.

/// `https://github.com/search?q={query}` → `https://github.com`. `None`
/// for anything but `http(s)`, and for hosts that are filled in later.
pub fn origin(link: &str) -> Option<String> {
    let link = link.trim();
    let (scheme, rest) = link.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    if host.is_empty() || host.contains(['{', '}', '%', '@', ' ']) {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// The icon URLs to try for the page at `page` with `html`, best first for
/// an icon `size` pixels square: the smallest declared icon at least that
/// big, else the biggest; `/favicon.ico` last. SVG icons are left out.
pub fn candidates(html: &str, page: &str, size: u32) -> Vec<String> {
    // Icons are declared in the head; the body can be large.
    let head = match find_ascii(html, "</head") {
        Some(end) => &html[..end],
        None => html,
    };
    let mut found: Vec<(u32, String)> = Vec::new();
    let mut rest = head;
    while let Some(start) = find_ascii(rest, "<link") {
        rest = &rest[start + 5..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        rest = &rest[end..];

        let attributes = attributes(tag);
        let get = |name: &str| {
            attributes
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        let rel = get("rel").unwrap_or_default().to_ascii_lowercase();
        let rels: Vec<&str> = rel.split_ascii_whitespace().collect();
        let touch = rels.iter().any(|rel| rel.starts_with("apple-touch-icon"));
        if !touch && !rels.contains(&"icon") {
            continue;
        }
        let Some(href) = get("href").map(|href| href.trim().replace("&amp;", "&")) else {
            continue;
        };
        let svg = get("type").is_some_and(|kind| kind.contains("svg"))
            || href
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .ends_with(".svg")
            || href.starts_with("data:");
        if svg || href.is_empty() {
            continue;
        }
        // Undeclared: Apple's touch icons are 180, the rest usually 32.
        let declared = get("sizes").and_then(|sizes| {
            sizes
                .split_ascii_whitespace()
                .filter_map(|size| size.split_once(['x', 'X'])?.0.parse::<u32>().ok())
                .max()
        });
        let side = declared.unwrap_or(if touch { 180 } else { 32 });
        if let Some(url) = resolve(page, &href) {
            found.push((side, url));
        }
    }
    // Big enough first, smallest of those; then the rest, biggest first.
    found.sort_by_key(|(side, _)| {
        if *side >= size {
            (0, *side as i64)
        } else {
            (1, -(*side as i64))
        }
    });
    let mut urls: Vec<String> = Vec::new();
    for (_, url) in found {
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    if let Some(origin) = origin(page) {
        let fallback = format!("{origin}/favicon.ico");
        if !urls.contains(&fallback) {
            urls.push(fallback);
        }
    }
    urls
}

/// `href` made absolute against `page`.
fn resolve(page: &str, href: &str) -> Option<String> {
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(href.to_owned());
    }
    let scheme = page.split_once("://")?.0;
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}"));
    }
    let origin = origin(page)?;
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    if href.contains(':') {
        // Another scheme (`javascript:`…).
        return None;
    }
    // Relative to the page's folder.
    let path = &page[origin.len()..];
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let folder = &path[..path.rfind('/').map_or(0, |slash| slash + 1)];
    let folder = if folder.is_empty() { "/" } else { folder };
    Some(format!("{origin}{folder}{href}"))
}

/// The `name=value` pairs of a tag's inside, values quoted or not.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut chars = tag.char_indices().peekable();
    while let Some(&(start, ch)) = chars.peek() {
        if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            chars.next();
            continue;
        }
        let mut end = start;
        while let Some(&(ix, ch)) = chars.peek() {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                end = ix + ch.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let name = tag[start..end].to_owned();
        while chars.peek().is_some_and(|(_, ch)| ch.is_whitespace()) {
            chars.next();
        }
        if chars.peek().map(|(_, ch)| *ch) != Some('=') {
            pairs.push((name, String::new()));
            continue;
        }
        chars.next();
        while chars.peek().is_some_and(|(_, ch)| ch.is_whitespace()) {
            chars.next();
        }
        let value = match chars.peek().map(|&(ix, ch)| (ix, ch)) {
            Some((ix, quote @ ('"' | '\''))) => {
                chars.next();
                let from = ix + 1;
                let mut to = tag.len();
                for (ix, ch) in chars.by_ref() {
                    if ch == quote {
                        to = ix;
                        break;
                    }
                }
                &tag[from..to]
            }
            Some((ix, _)) => {
                let mut to = tag.len();
                while let Some(&(at, ch)) = chars.peek() {
                    if ch.is_whitespace() {
                        to = at;
                        break;
                    }
                    chars.next();
                }
                // `href=x.png/>`: the slash closes the tag.
                let value = &tag[ix..to];
                if to == tag.len() {
                    value.strip_suffix('/').unwrap_or(value)
                } else {
                    value
                }
            }
            None => "",
        };
        pairs.push((name, value.to_owned()));
    }
    pairs
}

/// Byte offset of `needle` (ASCII) in `haystack`, ignoring ASCII case.
fn find_ascii(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_origin() {
        assert_eq!(
            origin("https://GitHub.com/search?q={query}").as_deref(),
            Some("https://github.com")
        );
        assert_eq!(
            origin("http://localhost:3000").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(origin("https://{query}.example.com/"), None);
        assert_eq!(origin("vscode://file/x"), None);
        assert_eq!(origin(r"C:\Notes"), None);
    }

    #[test]
    fn picks_the_best_icon() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/app.css">
            <link rel="icon" type="image/svg+xml" href="/icon.svg">
            <LINK REL="shortcut icon" href=/favicon-16.png sizes=16x16>
            <link rel='icon' href='https://cdn.example.com/i/64.png?v=2&amp;x=1' sizes='64x64'>
            <link rel="apple-touch-icon" href="touch.png">
            </head><body><link rel="icon" href="/late.png"></body></html>"#;
        assert_eq!(
            candidates(html, "https://example.com/docs/page", 44),
            [
                "https://cdn.example.com/i/64.png?v=2&x=1",
                "https://example.com/docs/touch.png",
                "https://example.com/favicon-16.png",
                "https://example.com/favicon.ico",
            ]
        );
    }

    #[test]
    fn falls_back_to_favicon_ico() {
        assert_eq!(
            candidates("<p>no head</p>", "https://example.com", 44),
            ["https://example.com/favicon.ico"]
        );
        assert_eq!(
            candidates(
                r#"<link rel="icon" href="//static.example.com/f.ico">"#,
                "https://example.com",
                44
            ),
            [
                "https://static.example.com/f.ico",
                "https://example.com/favicon.ico"
            ]
        );
    }
}
