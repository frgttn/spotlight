//! Placeholders in braces, shared by quicklinks and snippets. Each one fills
//! in only the placeholders it knows; the rest stay as typed.
//!
//! - `{query}` or `{argument}`: text the user types.
//! - `{argument name="Repo" default="zed" options="a,b"}`: a named argument,
//!   optionally with a default value or a fixed set of choices. Arguments with
//!   the same name share one value.
//! - `{date}`, `{time}`, `{datetime}`, `{day}`, `{uuid}`: the moment it's used.
//! - `{selection}`: the text selected in the app the launcher was opened from.
//! - `{clipboard}`: the text on the clipboard.
//! - `{cursor}`: where the caret goes after a snippet is pasted.
//!
//! Anything else in braces is not a placeholder.

/// One value the user supplies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argument {
    /// `"Query"` for `{query}` and `{argument}`.
    pub name: String,
    pub default: String,
    /// Fixed choices; empty for free text.
    pub options: Vec<String>,
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

/// What `{date}`, `{time}`, `{datetime}`, `{day}` and `{uuid}` stand for at
/// one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moment {
    /// `2026-10-23`
    pub date: String,
    /// `14:05`
    pub time: String,
    /// `Friday`
    pub day: String,
    pub uuid: String,
}

impl Moment {
    /// `2026-10-23 14:05`
    pub fn datetime(&self) -> String {
        format!("{} {}", self.date, self.time)
    }
}

/// The name `{query}` and a nameless `{argument}` go by.
const QUERY: &str = "Query";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Placeholder {
    Argument(Argument),
    Date,
    Time,
    DateTime,
    Day,
    Uuid,
    Selection,
    Clipboard,
    Cursor,
}

impl Placeholder {
    /// The text for a moment placeholder; `None` for the others.
    pub(crate) fn moment(&self, moment: &Moment) -> Option<String> {
        match self {
            Placeholder::Date => Some(moment.date.clone()),
            Placeholder::Time => Some(moment.time.clone()),
            Placeholder::DateTime => Some(moment.datetime()),
            Placeholder::Day => Some(moment.day.clone()),
            Placeholder::Uuid => Some(moment.uuid.clone()),
            _ => None,
        }
    }
}

/// Splits `text` into literal text and placeholders: `(start, end, placeholder)`
/// byte ranges, in order.
pub(crate) fn placeholders(text: &str) -> Vec<(usize, usize, Placeholder)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(open) = text[from..].find('{').map(|ix| from + ix) {
        let Some(close) = text[open..].find('}').map(|ix| open + ix) else {
            break;
        };
        // A `{` inside is the real start: `{{query}`.
        let inner = &text[open + 1..close];
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
        "datetime" => Some(Placeholder::DateTime),
        "day" => Some(Placeholder::Day),
        "uuid" => Some(Placeholder::Uuid),
        "selection" => Some(Placeholder::Selection),
        "clipboard" => Some(Placeholder::Clipboard),
        "cursor" => Some(Placeholder::Cursor),
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

/// The arguments `text` asks for, each name once, in order of appearance.
/// The first mention of a name sets its default and options.
pub fn arguments(text: &str) -> Vec<Argument> {
    let mut arguments: Vec<Argument> = Vec::new();
    for (_, _, placeholder) in placeholders(text) {
        if let Placeholder::Argument(argument) = placeholder
            && !arguments.iter().any(|known| known.name == argument.name)
        {
            arguments.push(argument);
        }
    }
    arguments
}

/// `text` with argument `i` (in [`arguments`] order) replaced by
/// `{field_id(i)}`, so a form can fill it. Other placeholders stay.
pub fn with_field_ids(text: &str) -> String {
    let names: Vec<String> = arguments(text).into_iter().map(|arg| arg.name).collect();
    replace(text, |placeholder| match placeholder {
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

/// The value argument `argument` takes from `values` (in [`arguments`]
/// order; missing or empty ones take their default, or their first option).
pub(crate) fn argument_value<'a>(
    arguments: &'a [Argument],
    values: &'a [String],
    argument: &Argument,
) -> Option<&'a str> {
    let ix = arguments
        .iter()
        .position(|known| known.name == argument.name)?;
    Some(
        values
            .get(ix)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| arguments[ix].fallback()),
    )
}

/// Whether `text` contains a placeholder `matches` accepts.
pub(crate) fn contains(text: &str, matches: impl Fn(&Placeholder) -> bool) -> bool {
    placeholders(text)
        .iter()
        .any(|(_, _, placeholder)| matches(placeholder))
}

/// Replaces each placeholder `replacement` returns text for.
pub(crate) fn replace(
    text: &str,
    mut replacement: impl FnMut(&Placeholder) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end, placeholder) in placeholders(text) {
        if let Some(value) = replacement(&placeholder) {
            out.push_str(&text[last..start]);
            out.push_str(&value);
            last = end;
        }
    }
    out.push_str(&text[last..]);
    out
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
    fn knows_every_simple_placeholder() {
        let text = "{date}{time}{datetime}{day}{uuid}{selection}{clipboard}{cursor}{ cursor }";
        let found: Vec<_> = placeholders(text).into_iter().map(|(_, _, p)| p).collect();
        assert_eq!(
            found,
            [
                Placeholder::Date,
                Placeholder::Time,
                Placeholder::DateTime,
                Placeholder::Day,
                Placeholder::Uuid,
                Placeholder::Selection,
                Placeholder::Clipboard,
                Placeholder::Cursor,
                Placeholder::Cursor,
            ]
        );
        assert!(placeholders("{cursor here} {clipboards} {}").is_empty());
    }
}
