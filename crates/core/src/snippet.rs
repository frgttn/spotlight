//! Snippets: text the user saves under a name, pasted from the launcher or
//! typed out by its keyword in any app.
//!
//! Placeholders, as in Raycast (see [`crate::placeholder`]): `{clipboard}`,
//! `{cursor}`, `{selection}`, `{date}`, `{time}`, `{datetime}`, `{day}` (with
//! `format` and `offset`), `{uuid}`, `{snippet name="…"}` and `{argument …}`.
//! Anything else in braces is pasted as it is.
//!
//! They move to and from Raycast as JSON: an array of
//! `{"name", "text", "keyword"}` objects, the keyword left out when there's none.

use serde_json::{Map, Value, json};
use unicode_segmentation::UnicodeSegmentation;

pub use crate::placeholder::{Argument, Moment, arguments, field_id, with_field_ids};
use crate::{
    item::Command,
    placeholder::{self, Placeholder},
    view::{Field, FieldKind, FormView, View},
};

/// A saved snippet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// Row id in the store; 0 for one not saved yet.
    pub id: i64,
    pub name: String,
    pub text: String,
    /// Typing it in any app replaces it with the snippet. Kept as typed:
    /// matching is case-sensitive, as in Raycast.
    pub keyword: Option<String>,
}

/// `settings` key: whether keywords typed in other apps expand (`"false"`
/// turns it off; on by default).
pub const EXPANSION_SETTING: &str = "snippet_expansion";
/// `settings` key: apps where keywords don't expand, in the format of
/// [`crate::clip::IgnoredApp::format_list`].
pub const EXCLUDED_APPS_SETTING: &str = "snippet_excluded_apps";

/// The longest keyword, in characters; the expander only remembers this
/// much of what was typed.
pub const MAX_KEYWORD: usize = 32;

/// `keyword` as stored: trimmed; `None` when empty. `Err` says why it can't
/// be used.
pub fn normalize_keyword(keyword: &str) -> Result<Option<String>, &'static str> {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return Ok(None);
    }
    if keyword.contains(char::is_whitespace) {
        return Err("A keyword is one word, without spaces");
    }
    if keyword.chars().count() > MAX_KEYWORD {
        return Err("A keyword is at most 32 characters");
    }
    Ok(Some(keyword.to_owned()))
}

/// Why a snippet can't be saved, in words for the form.
pub fn check(name: &str, text: &str) -> Result<(), &'static str> {
    if name.trim().is_empty() {
        return Err("Name is required");
    }
    if text.trim().is_empty() {
        return Err("Snippet is required");
    }
    Ok(())
}

/// `text` as stored: line breaks as `\n`.
pub fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The name a duplicate of `name` starts with.
pub fn copy_name(name: &str) -> String {
    format!("{name} Copy")
}

/// How many characters of the text name a snippet that comes without a name.
const NAME_FROM_TEXT: usize = 40;

/// What [`import`] makes of a JSON file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    /// To save: new, and with keywords nothing else has.
    pub added: Vec<Snippet>,
    /// Already there: same name, text and keyword.
    pub duplicates: usize,
    /// Left out because a snippet already has their keyword.
    pub taken: usize,
    /// Without text, or with a keyword that can't be used.
    pub invalid: usize,
}

/// The snippets in `json` (as Raycast exports them) that `existing` doesn't
/// have yet. `Err` when it isn't such a file at all.
pub fn import(json: &str, existing: &[Snippet]) -> Result<Imported, String> {
    let json = json.trim_start_matches('\u{feff}');
    let value: Value =
        serde_json::from_str(json).map_err(|err| format!("Not a JSON file: {err}"))?;
    let Value::Array(entries) = value else {
        return Err("Not a list of snippets".into());
    };
    let mut imported = Imported::default();
    for entry in entries {
        let Some(snippet) = parse_entry(&entry) else {
            imported.invalid += 1;
            continue;
        };
        let known = existing.iter().chain(&imported.added);
        let same = |other: &&Snippet| {
            other.name == snippet.name
                && other.text == snippet.text
                && other.keyword == snippet.keyword
        };
        if known.clone().any(|other| same(&other)) {
            imported.duplicates += 1;
        } else if snippet.keyword.is_some()
            && known
                .into_iter()
                .any(|other| other.keyword == snippet.keyword)
        {
            imported.taken += 1;
        } else {
            imported.added.push(snippet);
        }
    }
    Ok(imported)
}

/// One object of an import; `None` when it can't be a snippet.
fn parse_entry(entry: &Value) -> Option<Snippet> {
    let text = normalize_text(entry.get("text")?.as_str()?);
    if text.trim().is_empty() {
        return None;
    }
    let keyword = match entry.get("keyword") {
        None | Some(Value::Null) => None,
        Some(keyword) => normalize_keyword(keyword.as_str()?).ok()?,
    };
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(
            || {
                let first = text.trim().lines().next().unwrap_or_default();
                first.chars().take(NAME_FROM_TEXT).collect()
            },
            str::to_owned,
        );
    Some(Snippet {
        id: 0,
        name,
        text,
        keyword,
    })
}

/// `snippets` as JSON Raycast can import.
pub fn export(snippets: &[Snippet]) -> String {
    let entries: Vec<Value> = snippets
        .iter()
        .map(|snippet| {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(snippet.name));
            entry.insert("text".into(), json!(snippet.text));
            if let Some(keyword) = &snippet.keyword {
                entry.insert("keyword".into(), json!(keyword));
            }
            Value::Object(entry)
        })
        .collect();
    serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into())
}

/// How deep `{snippet}` placeholders are followed into each other.
const MAX_NESTING: usize = 5;

/// `text` with each `{snippet name="…"}` replaced by the text of the snippet
/// of that name (its own `{snippet}`s too). Names match as typed, else
/// ignoring case. A snippet that isn't there, or that would include itself,
/// stays as typed.
pub fn inline(text: &str, snippets: &[Snippet]) -> String {
    inline_from(text, snippets, &mut Vec::new())
}

fn inline_from<'a>(text: &str, snippets: &'a [Snippet], within: &mut Vec<&'a str>) -> String {
    placeholder::replace(text, |placeholder| {
        let Placeholder::Snippet(name) = placeholder else {
            return None;
        };
        let snippet = snippets
            .iter()
            .find(|snippet| snippet.name == *name)
            .or_else(|| {
                snippets
                    .iter()
                    .find(|snippet| snippet.name.to_lowercase() == name.to_lowercase())
            })?;
        if within.len() >= MAX_NESTING || within.contains(&snippet.name.as_str()) {
            return None;
        }
        within.push(&snippet.name);
        let text = inline_from(&snippet.text, snippets, within);
        within.pop();
        Some(text)
    })
}

/// Whether `text` contains `{selection}`, which the launcher reads from the
/// app it came from before pasting.
pub fn has_selection(text: &str) -> bool {
    placeholder::contains(text, |placeholder| *placeholder == Placeholder::Selection)
}

/// `text` with `{selection}` replaced by `selection`.
pub fn fill_selection(text: &str, selection: &str) -> String {
    placeholder::replace(text, |placeholder| {
        (*placeholder == Placeholder::Selection).then(|| selection.to_owned())
    })
}

/// What the placeholders of a snippet are filled with.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    pub moment: &'a Moment,
    /// The text on the clipboard before the snippet is pasted; `{clipboard}`
    /// is left empty without it.
    pub clipboard: Option<&'a str>,
    /// Argument values, in [`arguments`] order; missing or empty ones take
    /// their default, or their first option.
    pub values: &'a [String],
}

/// A snippet ready to paste.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    /// Byte offset into `text` of the first `{cursor}`, where the caret goes.
    pub cursor: Option<usize>,
}

impl Expansion {
    /// How many times to press Left after pasting to put the caret at
    /// [`Expansion::cursor`]: one press per grapheme after it (a `\r\n`
    /// line break is one).
    pub fn moves_back(&self) -> usize {
        self.cursor
            .map_or(0, |cursor| self.text[cursor..].graphemes(true).count())
    }
}

/// `text` with its placeholders filled in. Only the first `{cursor}` counts;
/// the others are dropped.
pub fn expand(text: &str, inputs: &Inputs) -> Expansion {
    let arguments = arguments(text);
    let mut out = String::with_capacity(text.len());
    let mut cursor = None;
    let mut last = 0;
    for (start, end, placeholder) in placeholder::placeholders(text) {
        let value = match &placeholder {
            Placeholder::Argument(argument) => {
                placeholder::argument_value(&arguments, inputs.values, argument).map(str::to_owned)
            }
            Placeholder::Clipboard => Some(inputs.clipboard.unwrap_or_default().to_owned()),
            Placeholder::Cursor => Some(String::new()),
            // The launcher fills it in before (see `fill_selection`); a
            // keyword typed in an app has nothing selected.
            Placeholder::Selection => Some(String::new()),
            moment => moment.moment(inputs.moment),
        };
        let Some(value) = value else {
            continue;
        };
        out.push_str(&text[last..start]);
        if placeholder == Placeholder::Cursor && cursor.is_none() {
            cursor = Some(out.len());
        }
        out.push_str(&value);
        last = end;
    }
    out.push_str(&text[last..]);
    Expansion { text: out, cursor }
}

/// A form with a field per argument of `text` (the snippet called `name`);
/// submitting pastes it (or copies it) with them.
pub fn arguments_form(name: &str, text: &str, paste: bool) -> View {
    let fields = arguments(text)
        .iter()
        .enumerate()
        .map(|(ix, argument)| {
            let kind = if argument.is_free_text() {
                FieldKind::Text {
                    placeholder: argument.name.clone(),
                    default: argument.default.clone(),
                    required: argument.fallback().is_empty(),
                }
            } else {
                FieldKind::Dropdown {
                    default: argument
                        .options
                        .iter()
                        .position(|option| *option == argument.default)
                        .unwrap_or(0),
                    options: argument
                        .options
                        .iter()
                        .map(|option| (option.clone(), option.clone()))
                        .collect(),
                }
            };
            Field {
                id: field_id(ix),
                label: argument.name.clone(),
                kind,
            }
        })
        .collect();
    View::Form(FormView {
        title: name.to_owned(),
        fields,
        submit_title: if paste { "Paste" } else { "Copy" }.into(),
        submit: Command::PasteSnippet {
            text: with_field_ids(text),
            paste,
        },
    })
}

/// Whether `text` contains `{clipboard}`, so the clipboard has to be read
/// before the snippet goes on it.
pub fn needs_clipboard(text: &str) -> bool {
    placeholder::contains(text, |placeholder| *placeholder == Placeholder::Clipboard)
}

/// The last characters typed in the app in front, to find a keyword at
/// their end. The expander forgets them when the caret may have moved (a
/// click, an arrow key, another window), so they always end where the caret is.
#[derive(Clone, Debug, Default)]
pub struct Typed {
    /// At most one more than the longest keyword: the character before it
    /// tells whether it starts a word.
    chars: Vec<char>,
}

impl Typed {
    pub fn push(&mut self, ch: char) {
        if self.chars.len() > MAX_KEYWORD {
            self.chars.remove(0);
        }
        self.chars.push(ch);
    }

    pub fn backspace(&mut self) {
        self.chars.pop();
    }

    pub fn clear(&mut self) {
        self.chars.clear();
    }

    /// The longest of `keywords` the typed text ends with, where it starts a
    /// word: what comes before it (if anything was typed) isn't a letter or a
    /// digit, so `;addr` fires after a space but not inside `foo;addr`.
    pub fn keyword_at_end<'a>(
        &self,
        keywords: impl IntoIterator<Item = &'a str>,
    ) -> Option<&'a str> {
        keywords
            .into_iter()
            .filter(|keyword| {
                let keyword: Vec<char> = keyword.chars().collect();
                let Some(start) = self.chars.len().checked_sub(keyword.len()) else {
                    return false;
                };
                !keyword.is_empty()
                    && self.chars[start..] == keyword[..]
                    && (start == 0 || !self.chars[start - 1].is_alphanumeric())
            })
            .max_by_key(|keyword| keyword.chars().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moment() -> Moment {
        Moment {
            now: crate::datetime::LocalTime {
                year: 2026,
                month: 10,
                day: 23,
                hour: 14,
                minute: 5,
                second: 0,
            },
            uuid: "u".into(),
        }
    }

    fn expand_with(text: &str, clipboard: Option<&str>, values: &[String]) -> Expansion {
        let moment = moment();
        expand(
            text,
            &Inputs {
                moment: &moment,
                clipboard,
                values,
            },
        )
    }

    #[test]
    fn fills_every_placeholder() {
        let expansion = expand_with(
            "{day}, {datetime} ({date} {time}) #{uuid}: {clipboard} {selection} {unknown}",
            Some("copied"),
            &[],
        );
        assert_eq!(
            expansion.text,
            "Friday, 2026-10-23 14:05 (2026-10-23 14:05) #u: copied  {unknown}"
        );
        assert_eq!(expansion.cursor, None);
        assert_eq!(expansion.moves_back(), 0);
        assert_eq!(expand_with("[{clipboard}]", None, &[]).text, "[]");
    }

    #[test]
    fn arguments_take_values_then_defaults() {
        let text = r#"Hi {argument name="Who" default="team"}, re: {argument name="Topic" options="docs,bugs"} — {argument name="Who"}"#;
        let names: Vec<_> = arguments(text).into_iter().map(|arg| arg.name).collect();
        assert_eq!(names, ["Who", "Topic"]);
        assert_eq!(
            expand_with(text, None, &[]).text,
            "Hi team, re: docs — team"
        );
        assert_eq!(
            expand_with(text, None, &["Ann".into(), "bugs".into()]).text,
            "Hi Ann, re: bugs — Ann"
        );
    }

    #[test]
    fn the_first_cursor_places_the_caret() {
        let expansion = expand_with("<b>{cursor}</b>{cursor}", None, &[]);
        assert_eq!(expansion.text, "<b></b>");
        assert_eq!(expansion.cursor, Some(3));
        assert_eq!(expansion.moves_back(), 4);

        // Graphemes, not bytes: a line break and an emoji with a modifier
        // are one press each.
        let expansion = expand_with("Привет, {cursor}!\r\n👍🏽", None, &[]);
        assert_eq!(expansion.moves_back(), 3);
    }

    #[test]
    fn includes_other_snippets() {
        let snippets = [
            snippet("Sign", "— Ann{snippet name=\"Title\"}", None),
            snippet("Title", ", Docs team", None),
            snippet("Loop", "x{snippet name=\"Loop\"}", None),
        ];
        assert_eq!(
            inline(
                "Thanks!\n{snippet name=\"sign\"} {snippet name=\"None\"}",
                &snippets
            ),
            "Thanks!\n— Ann, Docs team {snippet name=\"None\"}"
        );
        assert_eq!(
            inline("{snippet name=\"Loop\"}", &snippets),
            "x{snippet name=\"Loop\"}"
        );
    }

    #[test]
    fn fills_the_selection() {
        let text = "<b>{selection}</b>{cursor}";
        assert!(has_selection(text));
        assert!(!has_selection("{clipboard}"));
        assert_eq!(fill_selection(text, "bold"), "<b>bold</b>{cursor}");
    }

    #[test]
    fn knows_when_it_needs_the_clipboard() {
        assert!(needs_clipboard("Copied: {clipboard}"));
        assert!(!needs_clipboard("{date} {cursor}"));
    }

    #[test]
    fn keywords() {
        assert_eq!(normalize_keyword(" ;Addr "), Ok(Some(";Addr".into())));
        assert_eq!(normalize_keyword("  "), Ok(None));
        assert!(normalize_keyword("my addr").is_err());
        assert!(normalize_keyword(&"x".repeat(MAX_KEYWORD)).is_ok());
        assert!(normalize_keyword(&"x".repeat(MAX_KEYWORD + 1)).is_err());
    }

    fn snippet(name: &str, text: &str, keyword: Option<&str>) -> Snippet {
        Snippet {
            id: 0,
            name: name.into(),
            text: text.into(),
            keyword: keyword.map(str::to_owned),
        }
    }

    #[test]
    fn exports_what_it_imports() {
        let snippets = [
            snippet("Address", "Main St 1\nSpringfield", Some(";addr")),
            snippet("Thanks", "Thank you, {cursor}!", None),
        ];
        let json = export(&snippets);
        assert!(!json.contains(r#""keyword": null"#));
        let imported = import(&json, &[]).unwrap();
        assert_eq!(imported.added, snippets);
        assert_eq!(
            (imported.duplicates, imported.taken, imported.invalid),
            (0, 0, 0)
        );
    }

    #[test]
    fn imports_raycast_files_leniently() {
        let existing = [
            snippet("Address", "Main St 1", Some(";addr")),
            snippet("Sign", "— Ann", None),
        ];
        let json = "\u{feff}[
            {\"name\": \"Address\", \"text\": \"Main St 1\", \"keyword\": \";addr\"},
            {\"name\": \"Work address\", \"text\": \"Office 2\", \"keyword\": \";addr\"},
            {\"name\": \"Sign\", \"text\": \"— Ann\"},
            {\"text\": \"  Untitled first line\\r\\nsecond\", \"keyword\": null},
            {\"name\": \"Mail\", \"text\": \"a@b.c\", \"keyword\": \";mail\"},
            {\"name\": \"Mail again\", \"text\": \"c@d.e\", \"keyword\": \";mail\"},
            {\"name\": \"Empty\", \"text\": \" \"},
            {\"name\": \"Spaced\", \"text\": \"x\", \"keyword\": \"two words\"},
            {\"name\": \"Number\", \"text\": 5},
            \"not an object\"
        ]";
        let imported = import(json, &existing).unwrap();
        assert_eq!(
            imported.added,
            [
                snippet("Untitled first line", "  Untitled first line\nsecond", None),
                snippet("Mail", "a@b.c", Some(";mail")),
            ]
        );
        assert_eq!(imported.duplicates, 2);
        assert_eq!(imported.taken, 2);
        assert_eq!(imported.invalid, 4);

        assert!(import("{}", &[]).is_err());
        assert!(import("nonsense", &[]).is_err());
        assert_eq!(import("[]", &[]), Ok(Imported::default()));
    }

    #[test]
    fn normalizes_line_breaks() {
        assert_eq!(normalize_text("a\r\nb\rc\n"), "a\nb\nc\n");
        assert_eq!(copy_name("Address"), "Address Copy");
    }

    fn typed(text: &str) -> Typed {
        let mut typed = Typed::default();
        text.chars().for_each(|ch| typed.push(ch));
        typed
    }

    #[test]
    fn finds_a_keyword_that_starts_a_word() {
        let keywords = [";addr", ";a", "brb"];
        assert_eq!(typed(";addr").keyword_at_end(keywords), Some(";addr"));
        // The longest wins; `;` ends a word, so `addr` could fire too.
        assert_eq!(
            typed(";addr").keyword_at_end(["addr", ";addr"]),
            Some(";addr")
        );
        assert_eq!(typed("hi ;addr").keyword_at_end(keywords), Some(";addr"));
        assert_eq!(typed("(;a").keyword_at_end(keywords), Some(";a"));
        // Inside a word: not a keyword.
        assert_eq!(typed("foo;addr").keyword_at_end(keywords), None);
        assert_eq!(typed("abrb").keyword_at_end(keywords), None);
        assert_eq!(typed("словоbrb").keyword_at_end(keywords), None);
        // Case matters, and so does the end.
        assert_eq!(typed("BRB").keyword_at_end(keywords), None);
        assert_eq!(typed("brb ").keyword_at_end(keywords), None);
        assert_eq!(typed("").keyword_at_end(keywords), None);
    }

    #[test]
    fn remembers_only_what_a_keyword_needs() {
        let keyword = "k".repeat(MAX_KEYWORD);
        let mut typed = typed(&"x".repeat(100));
        typed.push(' ');
        keyword.chars().for_each(|ch| typed.push(ch));
        assert_eq!(typed.chars.len(), MAX_KEYWORD + 1);
        assert_eq!(
            typed.keyword_at_end([keyword.as_str()]),
            Some(keyword.as_str())
        );

        let mut typed = self::typed(";adx");
        typed.backspace();
        typed.push('d');
        typed.push('r');
        assert_eq!(typed.keyword_at_end([";addr"]), Some(";addr"));
        typed.clear();
        typed.push('r');
        assert_eq!(typed.keyword_at_end([";addr"]), None);
    }

    #[test]
    fn checks_the_form() {
        assert_eq!(check("Address", "Main St 1"), Ok(()));
        assert!(check(" ", "text").is_err());
        assert!(check("Name", "\n").is_err());
    }
}
