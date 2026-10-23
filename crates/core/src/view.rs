//! Views an action can open on top of root search. Plain data: providers
//! (and later scripts and plugins) describe a view, the UI renders it, and Esc
//! goes back to the view below.

use crate::{
    item::{Action, Command, SearchItem},
    quicklink,
    web::encode_query,
};

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    /// Searchable rows, like root search.
    List(ListView),
    /// Searchable tiles with large icons.
    Grid(ListView),
    /// Markdown to read, with facts beside it and actions.
    Detail(DetailView),
    /// Fields to fill in; submitting runs a command with the values.
    Form(FormView),
}

impl View {
    pub fn title(&self) -> &str {
        match self {
            View::List(list) | View::Grid(list) => &list.title,
            View::Detail(detail) => &detail.title,
            View::Form(form) => &form.title,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListView {
    pub title: String,
    pub placeholder: Option<String>,
    pub items: Vec<SearchItem>,
    /// Shown when there are no items at all (rather than none matching).
    pub empty: Option<String>,
    /// Choices that narrow the list (All, Images, Links…), which Tab steps
    /// through. Empty: no filter.
    pub filters: Vec<ListFilter>,
}

/// One choice of a list's filter: items with `keyword` among their keywords.
#[derive(Clone, Debug, PartialEq)]
pub struct ListFilter {
    pub label: String,
    /// `None` keeps every item.
    pub keyword: Option<String>,
}

impl ListFilter {
    pub fn new(label: impl Into<String>, keyword: Option<&str>) -> Self {
        Self {
            label: label.into(),
            keyword: keyword.map(str::to_owned),
        }
    }

    pub fn keeps(&self, item: &SearchItem) -> bool {
        self.keyword
            .as_ref()
            .is_none_or(|keyword| item.keywords.contains(keyword))
    }
}

/// What a list shows beside itself for the selected row.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub body: PreviewBody,
    /// Label and value pairs, under the body.
    pub metadata: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PreviewBody {
    /// Shown as it is, line breaks kept.
    Text(String),
    /// An image file (PNG), scaled down to fit.
    Image(String),
    /// File paths, one per row.
    Files(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DetailView {
    pub title: String,
    pub markdown: String,
    /// Label and value pairs, shown beside the text.
    pub metadata: Vec<(String, String)>,
    /// `[0]` runs on Enter; all of them are in the action panel.
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormView {
    pub title: String,
    pub fields: Vec<Field>,
    /// The submit button's label ("Search").
    pub submit_title: String,
    /// Run on submit, after [`FormView::fill`] put the values in.
    pub submit: Command,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// What `{id}` in the submit command is replaced with.
    pub id: String,
    pub label: String,
    pub kind: FieldKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text {
        placeholder: String,
        default: String,
        /// Submitting is refused while it's empty.
        required: bool,
    },
    /// Text over several lines: Enter starts a new line, Ctrl+Enter submits.
    TextArea {
        placeholder: String,
        default: String,
        /// Submitting is refused while it's empty.
        required: bool,
    },
    Checkbox {
        default: bool,
    },
    /// One of `options`, as (label, value) pairs.
    Dropdown {
        options: Vec<(String, String)>,
        default: usize,
    },
}

/// A field's value when the form is submitted.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Text(String),
    Checkbox(bool),
    /// The index into the options.
    Dropdown(usize),
}

impl FormView {
    /// Why the form can't be submitted with `values` yet, naming the field.
    pub fn check(&self, values: &[FieldValue]) -> Result<(), String> {
        for (field, value) in self.fields.iter().zip(values) {
            if let (
                FieldKind::Text { required: true, .. } | FieldKind::TextArea { required: true, .. },
                FieldValue::Text(text),
            ) = (&field.kind, value)
                && text.trim().is_empty()
            {
                return Err(format!("{} is required", field.label));
            }
        }
        Ok(())
    }

    /// The submit command with each `{id}` replaced by that field's value.
    ///
    /// Dropdown values go in first and as they are, so a dropdown can choose
    /// a whole URL template that itself has `{query}` in it. Text values go in
    /// after, percent-encoded when they end up in a URL. Checkboxes become
    /// `true` / `false`.
    pub fn fill(&self, values: &[FieldValue]) -> Command {
        let mut command = self.submit.clone();
        let fields = || self.fields.iter().zip(values);
        for (field, value) in fields() {
            if let (FieldKind::Dropdown { options, .. }, FieldValue::Dropdown(ix)) =
                (&field.kind, value)
                && let Some((_, chosen)) = options.get(*ix)
            {
                replace(&mut command, &field.id, |_| chosen.clone());
            }
        }
        for (field, value) in fields() {
            match value {
                FieldValue::Text(text) => replace(&mut command, &field.id, |in_url| {
                    if in_url {
                        encode_query(text.trim())
                    } else {
                        text.clone()
                    }
                }),
                FieldValue::Checkbox(on) => replace(&mut command, &field.id, |_| on.to_string()),
                FieldValue::Dropdown(_) => {}
            }
        }
        command
    }
}

/// Replaces `{id}` in the text a command carries. `value` is told whether the
/// text is a URL, to encode it.
fn replace(command: &mut Command, id: &str, value: impl Fn(bool) -> String) {
    let placeholder = format!("{{{id}}}");
    let (text, in_url) = match command {
        Command::OpenUrl(url) => (url, true),
        Command::Copy(text) | Command::PasteSnippet { text, .. } => (text, false),
        Command::OpenQuicklink { link, .. } => {
            // Quicklinks encode their own way (`%20`, and paths not at all).
            if link.contains(&placeholder) {
                let value = quicklink::encode_value(value(false).trim(), quicklink::is_url(link));
                *link = link.replace(&placeholder, &value);
            }
            return;
        }
        // Each field of the form fills one field of the quicklink, as typed.
        Command::SaveQuicklink(link) => {
            let fields = [&mut link.name, &mut link.link]
                .into_iter()
                .chain(link.keyword.as_mut())
                .chain(link.open_with.as_mut());
            for field in fields {
                if *field == placeholder {
                    *field = value(false);
                }
            }
            return;
        }
        Command::SaveSnippet(snippet) => {
            let fields = [&mut snippet.name, &mut snippet.text]
                .into_iter()
                .chain(snippet.keyword.as_mut());
            for field in fields {
                if *field == placeholder {
                    *field = value(false);
                }
            }
            return;
        }
        // The others take paths and ids the user doesn't type.
        _ => return,
    };
    if text.contains(&placeholder) {
        *text = text.replace(&placeholder, &value(in_url));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_form() -> FormView {
        FormView {
            title: "Search the Web".into(),
            fields: vec![
                Field {
                    id: "query".into(),
                    label: "Search for".into(),
                    kind: FieldKind::Text {
                        placeholder: String::new(),
                        default: String::new(),
                        required: true,
                    },
                },
                Field {
                    id: "engine".into(),
                    label: "Engine".into(),
                    kind: FieldKind::Dropdown {
                        options: vec![
                            (
                                "Google".into(),
                                "https://google.com/search?q={query}".into(),
                            ),
                            (
                                "DuckDuckGo".into(),
                                "https://duckduckgo.com/?q={query}".into(),
                            ),
                        ],
                        default: 0,
                    },
                },
            ],
            submit_title: "Search".into(),
            submit: Command::OpenUrl("{engine}".into()),
        }
    }

    #[test]
    fn fills_a_url_template_from_a_dropdown_and_text() {
        let form = search_form();
        let values = [
            FieldValue::Text(" rust & gpui ".into()),
            FieldValue::Dropdown(1),
        ];
        assert_eq!(
            form.fill(&values),
            Command::OpenUrl("https://duckduckgo.com/?q=rust+%26+gpui".into())
        );
    }

    #[test]
    fn copies_text_as_typed() {
        let form = FormView {
            submit: Command::Copy("Hello, {query}! {engine}".into()),
            ..search_form()
        };
        let values = [FieldValue::Text("Ann & Bo".into()), FieldValue::Dropdown(0)];
        assert_eq!(
            form.fill(&values),
            // The chosen template's own {query} is filled in too.
            Command::Copy("Hello, Ann & Bo! https://google.com/search?q=Ann & Bo".into())
        );
    }

    #[test]
    fn fills_quicklinks() {
        let text = |id: &str| Field {
            id: id.into(),
            label: id.into(),
            kind: FieldKind::Text {
                placeholder: String::new(),
                default: String::new(),
                required: false,
            },
        };
        let open = FormView {
            title: "Open".into(),
            fields: vec![text("arg0")],
            submit_title: "Open".into(),
            submit: Command::OpenQuicklink {
                link: "https://github.com/{arg0}/{date}".into(),
                app: None,
            },
        };
        assert_eq!(
            open.fill(&[FieldValue::Text(" a b ".into())]),
            Command::OpenQuicklink {
                link: "https://github.com/a%20b/{date}".into(),
                app: None
            }
        );

        let save = FormView {
            title: "Create Quicklink".into(),
            fields: vec![text("name"), text("link"), text("keyword")],
            submit_title: "Save".into(),
            submit: Command::SaveQuicklink(quicklink::Quicklink {
                id: 3,
                name: "{name}".into(),
                link: "{link}".into(),
                keyword: Some("{keyword}".into()),
                open_with: None,
            }),
        };
        let values = ["Docs {link}", "https://x.com/?q={query}", "gh"]
            .map(|value| FieldValue::Text(value.into()));
        assert_eq!(
            save.fill(&values),
            Command::SaveQuicklink(quicklink::Quicklink {
                id: 3,
                name: "Docs {link}".into(),
                link: "https://x.com/?q={query}".into(),
                keyword: Some("gh".into()),
                open_with: None,
            })
        );
    }

    #[test]
    fn required_fields_must_be_filled() {
        let form = search_form();
        let empty = [FieldValue::Text("  ".into()), FieldValue::Dropdown(0)];
        assert_eq!(form.check(&empty), Err("Search for is required".into()));
        let filled = [FieldValue::Text("x".into()), FieldValue::Dropdown(0)];
        assert_eq!(form.check(&filled), Ok(()));

        let area = FormView {
            fields: vec![Field {
                id: "text".into(),
                label: "Snippet".into(),
                kind: FieldKind::TextArea {
                    placeholder: String::new(),
                    default: String::new(),
                    required: true,
                },
            }],
            ..search_form()
        };
        assert_eq!(
            area.check(&[FieldValue::Text("\n \n".into())]),
            Err("Snippet is required".into())
        );
        assert_eq!(area.check(&[FieldValue::Text("a\nb".into())]), Ok(()));
    }
}
