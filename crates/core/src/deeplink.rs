//! `spotlight://` links, shaped like Raycast's `raycast://` ones:
//!
//! - `spotlight://` opens the launcher.
//! - `spotlight://extensions/spotlight/{provider}/{command}` runs a command.
//!   `{command}` is the item id's instance, percent-encoded (`system/lock`,
//!   `apps/Microsoft.WindowsCalculator_8wekyb3d8bbwe%21App`), or the item's
//!   title as a slug (`system/lock-screen`), as Raycast writes them.
//! - `?arguments={"Repo":"zed"}` fills a command's form fields by label (a
//!   quicklink's arguments); `?fallbackText=…` goes in the search field of a
//!   list the command opens.
//! - `?context=…` is Raycast's launch context: the JSON Import Snippets and
//!   Import Quicklinks add (`snippets/import?context=[…]`), or an object
//!   whose fields fill a form without submitting it (`quicklinks/create`
//!   with `{"name", "link"}`).

use serde_json::Value;

use crate::{
    item::Command,
    view::{FieldKind, FieldValue, View},
};

pub const SCHEME: &str = "spotlight";
/// The one "author" there is so far; Raycast puts the extension's owner here.
const OWNER: &str = "spotlight";
/// A provider whose command doesn't always ask for its arguments (a
/// quicklink whose arguments all have defaults) gives its form as view
/// `arguments:{instance}`, for links that bring arguments.
pub const ARGUMENTS_VIEW: &str = "arguments:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deeplink {
    /// `spotlight://`: just open the launcher.
    Open,
    Command(CommandLink),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandLink {
    pub provider: String,
    /// Decoded: an item id's instance, or a title slug.
    pub command: String,
    /// Form field label and value pairs, in the link's order.
    pub arguments: Vec<(String, String)>,
    pub fallback_text: Option<String>,
    /// Raycast's launch context, as the link had it (JSON).
    pub context: Option<String>,
}

impl CommandLink {
    /// The item id this link names if `command` is an instance.
    pub fn item_id(&self) -> String {
        crate::item_id(&self.provider, &self.command)
    }

    /// Whether the item `id` titled `title` is the one this link names: by
    /// its id, or by its title as a slug within the same provider.
    pub fn names(&self, id: &str, title: &str) -> bool {
        if id == self.item_id() {
            return true;
        }
        id.split_once(':')
            .is_some_and(|(provider, _)| provider == self.provider)
            && slug(title) == self.command
    }
}

/// Reads a `spotlight://` link; the error says what is wrong with it.
pub fn parse(link: &str) -> Result<Deeplink, String> {
    let link = link.trim();
    let rest = link
        .split_once(':')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case(SCHEME))
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("not a {SCHEME}:// link"))?;
    let rest = rest.trim_start_matches('/');
    let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let segments: Vec<String> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(decode)
        .collect();
    match segments.first().map(String::as_str) {
        None => Ok(Deeplink::Open),
        Some("extensions") => {
            let [_, owner, provider, command] = segments.as_slice() else {
                return Err("expected extensions/spotlight/{provider}/{command}".into());
            };
            if owner != OWNER {
                return Err(format!("unknown extension author {owner:?}"));
            }
            let mut link = CommandLink {
                provider: provider.clone(),
                command: command.clone(),
                arguments: Vec::new(),
                fallback_text: None,
                context: None,
            };
            for (key, value) in query_pairs(query) {
                match key.as_str() {
                    "arguments" => link.arguments.extend(arguments(&value)?),
                    "fallbackText" if !value.is_empty() => link.fallback_text = Some(value),
                    "context" | "launchContext" if !value.trim().is_empty() => {
                        link.context = Some(value)
                    }
                    _ => {}
                }
            }
            Ok(Deeplink::Command(link))
        }
        Some(other) => Err(format!("unknown link kind {other:?}")),
    }
}

/// The link that runs item `id`, with no arguments.
pub fn for_item(id: &str) -> String {
    let (provider, instance) = id.split_once(':').unwrap_or((id, ""));
    format!(
        "{SCHEME}://extensions/{OWNER}/{}/{}",
        encode(provider),
        encode(instance)
    )
}

/// What a command becomes with a link's `arguments`: a form takes them as its
/// fields' values (matched to a field's label or id, ignoring case) and, with
/// every required field then filled, is submitted right away. Other commands
/// take no arguments and stay as they are.
pub fn apply_arguments(command: Command, arguments: &[(String, String)]) -> Command {
    let form = match &command {
        Command::PushView(view) if !arguments.is_empty() => match &**view {
            View::Form(form) => form,
            _ => return command,
        },
        _ => return command,
    };
    let mut form = form.clone();
    prefill(&mut form, arguments);
    let values: Vec<FieldValue> = form
        .fields
        .iter()
        .map(|field| match &field.kind {
            FieldKind::Text { default, .. } | FieldKind::TextArea { default, .. } => {
                FieldValue::Text(default.clone())
            }
            FieldKind::Checkbox { default } => FieldValue::Checkbox(*default),
            FieldKind::Dropdown { default, .. } => FieldValue::Dropdown(*default),
        })
        .collect();
    if form.check(&values).is_ok() {
        form.fill(&values)
    } else {
        Command::PushView(Box::new(View::Form(form)))
    }
}

/// What a command becomes with a link's launch `context`: an import takes
/// the JSON to add (and so asks first), a form takes the fields of a JSON
/// object as its values, without being submitted. Others stay as they are.
pub fn apply_context(command: Command, context: Option<&str>) -> Command {
    let Some(context) = context else {
        return command;
    };
    match command {
        Command::ImportSnippets => Command::ImportSnippetsJson(context.to_owned()),
        Command::ImportQuicklinks => Command::ImportQuicklinksJson(context.to_owned()),
        Command::PushView(view) => match *view {
            View::Form(mut form) => {
                if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(context) {
                    let fields: Vec<(String, String)> = object
                        .into_iter()
                        .map(|(name, value)| match value {
                            Value::String(text) => (name, text),
                            other => (name, other.to_string()),
                        })
                        .collect();
                    prefill(&mut form, &fields);
                }
                Command::PushView(Box::new(View::Form(form)))
            }
            view => Command::PushView(Box::new(view)),
        },
        command => command,
    }
}

/// Puts `values` in `form`'s fields, matched to a field's label or id,
/// ignoring case.
fn prefill(form: &mut crate::view::FormView, values: &[(String, String)]) {
    let arguments = values;
    for field in &mut form.fields {
        let Some((_, value)) = arguments.iter().find(|(name, _)| {
            name.eq_ignore_ascii_case(&field.label) || name.eq_ignore_ascii_case(&field.id)
        }) else {
            continue;
        };
        match &mut field.kind {
            FieldKind::Text { default, .. } | FieldKind::TextArea { default, .. } => {
                *default = value.clone()
            }
            FieldKind::Checkbox { default } => *default = value.eq_ignore_ascii_case("true"),
            FieldKind::Dropdown { options, default } => {
                if let Some(ix) = options.iter().position(|(label, option)| {
                    label.eq_ignore_ascii_case(value) || option.eq_ignore_ascii_case(value)
                }) {
                    *default = ix;
                }
            }
        }
    }
}

/// Raycast's slug: lowercase ASCII letters and digits, words joined by
/// dashes. Latin letters keep their base letter without the accent (`Ç` is
/// `c`, `ß` is `ss`); other letters (Cyrillic, CJK) are dropped.
pub fn slug(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.trim().chars() {
        if let Some(plain) = unaccented(ch) {
            slug.extend(plain.chars().map(|ch| ch.to_ascii_lowercase()));
            continue;
        }
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if (ch.is_whitespace() || ch == '-') && !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// A Latin letter with an accent (or a ligature) as plain ASCII letters.
fn unaccented(ch: char) -> Option<&'static str> {
    const FROM: &str = "ÀÁÂÃÄÅàáâãäåĀāĂăĄąÇçĆćĈĉĊċČčĎďĐđÈÉÊËèéêëĒēĔĕĖėĘęĚěĜĝĞğĠġĢģĤĥĦħÌÍÎÏìíîïĨĩĪīĬĭĮįİıĴĵĶķĹĺĻļĽľĿŀŁłÑñŃńŅņŇňÒÓÔÕÖØòóôõöøŌōŎŏŐőŔŕŖŗŘřŚśŜŝŞşŠšŢţŤťŦŧÙÚÛÜùúûüŨũŪūŬŭŮůŰűŲųŴŵÝýÿŶŷŸŹźŻżŽž";
    const TO: &str = "AAAAAAaaaaaaAaAaAaCcCcCcCcCcDdDdEEEEeeeeEeEeEeEeEeGgGgGgGgHhHhIIIIiiiiIiIiIiIiIiJjKkLlLlLlLlLlNnNnNnNnOOOOOOooooooOoOoOoRrRrRrSsSsSsSsTtTtTtUUUUuuuuUuUuUuUuUuUuWwYyyYyYZzZzZz";
    if let Some(ix) = FROM.chars().position(|known| known == ch) {
        let start = TO.char_indices().nth(ix)?.0;
        return TO.get(start..start + 1);
    }
    Some(match ch {
        'ß' => "ss",
        'Æ' | 'æ' => "ae",
        'Œ' | 'œ' => "oe",
        'Þ' | 'þ' => "th",
        'Ð' | 'ð' => "d",
        _ => return None,
    })
}

/// `arguments` holds a JSON object of strings. Raycast's script commands
/// repeat the parameter with plain values instead, which come here as
/// `argument1`, `argument2`… by position.
fn arguments(value: &str) -> Result<Vec<(String, String)>, String> {
    if !value.trim_start().starts_with('{') {
        return Ok(vec![("argument1".into(), value.to_owned())]);
    }
    let object: serde_json::Map<String, Value> =
        serde_json::from_str(value).map_err(|err| format!("arguments aren't JSON: {err}"))?;
    Ok(object
        .into_iter()
        .map(|(name, value)| {
            let value = match value {
                Value::String(text) => text,
                other => other.to_string(),
            };
            (name, value)
        })
        .collect())
}

fn query_pairs(query: &str) -> impl Iterator<Item = (String, String)> + '_ {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                decode(&key.replace('+', " ")),
                decode(&value.replace('+', " ")),
            )
        })
}

/// Percent-decoding; invalid escapes stay as written, invalid UTF-8 is replaced.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|&b| (b as char).to_digit(16));
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (byte, ..) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encodes everything but unreserved characters, for a path segment.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(link: &str) -> CommandLink {
        match parse(link) {
            Ok(Deeplink::Command(link)) => link,
            other => panic!("{link}: {other:?}"),
        }
    }

    #[test]
    fn opens_the_launcher() {
        assert_eq!(parse("spotlight://"), Ok(Deeplink::Open));
        assert_eq!(parse("spotlight:"), Ok(Deeplink::Open));
        assert_eq!(parse("Spotlight:///"), Ok(Deeplink::Open));
        assert!(parse("raycast://").is_err());
        assert!(parse("https://example.com").is_err());
        assert!(parse("spotlight://confetti").is_err());
    }

    #[test]
    fn reads_command_links() {
        let link = command("spotlight://extensions/spotlight/system/lock");
        assert_eq!(link.item_id(), "system:lock");
        assert!(link.arguments.is_empty());
        assert_eq!(link.fallback_text, None);

        let link = command(
            "spotlight://extensions/spotlight/quicklinks/7?arguments=%7B%22Repo%22%3A%22zed%22%2C%22n%22%3A2%7D&fallbackText=hello+world",
        );
        assert_eq!(link.item_id(), "quicklinks:7");
        assert_eq!(
            link.arguments,
            vec![("Repo".into(), "zed".into()), ("n".into(), "2".into())]
        );
        assert_eq!(link.fallback_text.as_deref(), Some("hello world"));

        let link = command("spotlight://extensions/spotlight/x/y?arguments=a&arguments=b");
        assert_eq!(link.arguments.len(), 2);

        assert!(parse("spotlight://extensions/spotlight/system").is_err());
        assert!(parse("spotlight://extensions/someone/system/lock").is_err());
        assert!(parse("spotlight://extensions/spotlight/x/y?arguments=%7Bnope").is_err());

        let link = command(
            "spotlight://extensions/spotlight/snippets/import?context=%5B%7B%22text%22%3A%22hi%22%7D%5D",
        );
        assert_eq!(link.context.as_deref(), Some(r#"[{"text":"hi"}]"#));
    }

    #[test]
    fn takes_a_launch_context() {
        use crate::view::{Field, FormView};

        let json = r#"[{"name":"A","text":"a"}]"#;
        assert_eq!(
            apply_context(Command::ImportSnippets, Some(json)),
            Command::ImportSnippetsJson(json.into())
        );
        assert!(apply_context(Command::ImportQuicklinks, Some(json)).needs_confirmation());
        assert_eq!(
            apply_context(Command::ImportSnippets, None),
            Command::ImportSnippets
        );

        let field = |id: &str, label: &str| Field {
            id: id.into(),
            label: label.into(),
            kind: FieldKind::Text {
                placeholder: String::new(),
                default: String::new(),
                required: true,
            },
        };
        let create = Command::PushView(Box::new(View::Form(FormView {
            title: "Create Quicklink".into(),
            fields: vec![field("name", "Name"), field("link", "Link")],
            submit_title: "Save".into(),
            submit: Command::Copy("{name} {link}".into()),
        })));
        // Filled in, but not submitted: the user saves it.
        let Command::PushView(view) = apply_context(
            create,
            Some(r#"{"name": "Zed", "link": "https://zed.dev"}"#),
        ) else {
            panic!("expected the form");
        };
        let View::Form(form) = *view else {
            panic!("expected the form");
        };
        assert!(matches!(
            &form.fields[1].kind,
            FieldKind::Text { default, .. } if default == "https://zed.dev"
        ));
    }

    #[test]
    fn builds_links_that_read_back() {
        for id in [
            "system:lock",
            "apps:Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
            r"apps:C:\Program Files\App\app.exe",
            "control:::{26EE0668-A00A-44D7-9371-BEB064C98683}",
            "settings:ms-settings:display",
        ] {
            let link = for_item(id);
            assert!(!link[SCHEME.len() + 3..].contains(':'), "{link}");
            assert_eq!(command(&link).item_id(), id, "{link}");
        }
    }

    #[test]
    fn fills_forms_with_arguments() {
        use crate::view::{Field, FormView};

        let text = |id: &str, label: &str| Field {
            id: id.into(),
            label: label.into(),
            kind: FieldKind::Text {
                placeholder: String::new(),
                default: String::new(),
                required: true,
            },
        };
        let form = |fields| {
            Command::PushView(Box::new(View::Form(FormView {
                title: "Repo".into(),
                fields,
                submit_title: "Open".into(),
                submit: Command::OpenUrl("https://github.com/{arg0}/{arg1}".into()),
            })))
        };
        let both = form(vec![text("arg0", "Owner"), text("arg1", "Repo")]);
        let args = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect()
        };

        assert_eq!(
            apply_arguments(
                both.clone(),
                &args(&[("owner", "zed-industries"), ("Repo", "zed")])
            ),
            Command::OpenUrl("https://github.com/zed-industries/zed".into())
        );
        // One missing: the form opens with the other filled in.
        let Command::PushView(view) = apply_arguments(both.clone(), &args(&[("Repo", "zed")]))
        else {
            panic!("expected the form");
        };
        let View::Form(partial) = *view else {
            panic!("expected the form");
        };
        assert!(matches!(
            &partial.fields[1].kind,
            FieldKind::Text { default, .. } if default == "zed"
        ));
        assert_eq!(apply_arguments(both.clone(), &[]), both);
        let lock = Command::OpenUrl("ms-settings:".into());
        assert_eq!(apply_arguments(lock.clone(), &args(&[("a", "b")])), lock);
    }

    #[test]
    fn names_items_by_id_or_title() {
        let link = command("spotlight://extensions/spotlight/system/lock-screen");
        assert!(link.names("system:lock", "Lock Screen"));
        assert!(!link.names("apps:lock", "Lock Screen"));
        assert!(!link.names("system:sleep", "Sleep"));
        assert_eq!(slug("  Ça va — Search the Web… "), "ca-va-search-the-web");
        assert_eq!(
            slug("Crème brûlée für Straße, Łódź"),
            "creme-brulee-fur-strasse-lodz"
        );
        assert_eq!(slug("Ærø Привет"), "aero");
        assert_eq!(slug("Open  quick-link"), "open-quick-link");
    }
}
