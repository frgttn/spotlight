//! Snippets: text the user saved under a name (see `spotlight_core::snippet`).
//! Search Snippets lists them with a preview; Enter pastes one into the app
//! the launcher was opened from, after a form when it has arguments.
//!
//! Typing a snippet's keyword in any app is the expander's job, not this
//! provider's.

use std::sync::{Arc, Mutex};

use spotlight_core::{
    item::{Action, Command, Icon, SearchItem, shortcut},
    item_id,
    provider::Provider,
    snippet::{self, Snippet},
    view::{Field, FieldKind, FormView, ListView, Preview, PreviewBody, View},
};
use spotlight_store::Store;

use crate::glyph;

pub const ID: &str = "snippets";
/// The view id of Search Snippets, for `Command::OpenProviderView`, so the
/// list is built again after a snippet is deleted.
const LIST_VIEW: &str = "snippets";
/// How much of a snippet the preview shows.
const PREVIEW_CHARS: usize = 10_000;
/// How much of its text a snippet is found by in the list.
const SEARCHED_CHARS: usize = 200;

pub struct Snippets {
    /// `None` without a database: nothing to list, and nothing can be saved.
    store: Option<Arc<Mutex<Store>>>,
}

impl Snippets {
    pub fn new(store: Option<Arc<Mutex<Store>>>) -> Self {
        Self { store }
    }

    fn load(&self) -> Vec<Snippet> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let snippets = store
            .lock()
            .map_err(|_| "store lock poisoned".to_owned())
            .and_then(|store| store.snippets().map_err(|err| err.to_string()));
        snippets.unwrap_or_else(|err| {
            eprintln!("[snippets] could not read them: {err}");
            Vec::new()
        })
    }

    fn editable(&self) -> bool {
        self.store.is_some()
    }

    /// A row of Search Snippets: Enter pastes it, Ctrl+Enter copies it.
    /// `all` are every snippet, for the ones it includes.
    fn item(&self, snippet: &Snippet, all: &[Snippet]) -> SearchItem {
        let mut item = SearchItem::new(item_id(ID, &snippet.id.to_string()), snippet.name.clone())
            .icon(Icon::Glyph(glyph::SNIPPET));
        if let Some(keyword) = &snippet.keyword {
            item = item.subtitle(keyword.clone());
        }
        item.keywords = snippet
            .keyword
            .iter()
            .cloned()
            .chain([snippet.text.chars().take(SEARCHED_CHARS).collect()])
            .collect();
        item.actions.push(paste_action(snippet, all, true));
        item.actions.push(paste_action(snippet, all, false));
        if self.editable() {
            let edit = edit_form("Edit Snippet", snippet);
            let copy = Snippet {
                id: 0,
                name: snippet::copy_name(&snippet.name),
                keyword: None,
                ..snippet.clone()
            };
            let duplicate = edit_form("Duplicate Snippet", &copy);
            item = item
                .action("Edit Snippet…", Command::PushView(Box::new(edit)))
                .shortcut(shortcut::EDIT)
                .action("Duplicate Snippet…", Command::PushView(Box::new(duplicate)))
                .action("Delete Snippet", Command::DeleteSnippet(snippet.id));
        }
        item.preview = Some(Box::new(preview(snippet)));
        item
    }
}

impl Provider for Snippets {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Snippets"
    }

    fn description(&self) -> &'static str {
        "Text you save and paste, from Search Snippets or by typing its keyword."
    }

    fn catalog(&self) -> Vec<SearchItem> {
        let mut items = vec![
            SearchItem::new(item_id(ID, "search"), "Search Snippets")
                .subtitle("Snippets")
                .keywords(&["text expander", "templates", "paste"])
                .icon(Icon::Glyph(glyph::SNIPPET))
                .action(
                    "Show Snippets",
                    Command::OpenProviderView {
                        provider: ID.into(),
                        view: LIST_VIEW.into(),
                    },
                ),
        ];
        if self.editable() {
            let blank = Snippet {
                id: 0,
                name: String::new(),
                text: String::new(),
                keyword: None,
            };
            let create = edit_form("Create Snippet", &blank);
            items.extend([
                SearchItem::new(item_id(ID, "create"), "Create Snippet")
                    .subtitle("Snippets")
                    .keywords(&["new snippet", "add snippet", "text expander"])
                    .icon(Icon::Glyph(glyph::ADD))
                    .action("Create Snippet", Command::PushView(Box::new(create))),
                SearchItem::new(item_id(ID, "import"), "Import Snippets")
                    .subtitle("Snippets")
                    .keywords(&["raycast", "json"])
                    .icon(Icon::Glyph(glyph::IMPORT))
                    .action("Import from JSON…", Command::ImportSnippets),
                SearchItem::new(item_id(ID, "export"), "Export Snippets")
                    .subtitle("Snippets")
                    .keywords(&["raycast", "json", "backup"])
                    .icon(Icon::Glyph(glyph::EXPORT))
                    .action("Export to JSON…", Command::ExportSnippets),
            ]);
        }
        items
    }

    fn view(&self, id: &str) -> Option<View> {
        if id != LIST_VIEW {
            return None;
        }
        Some(View::List(ListView {
            title: "Snippets".into(),
            placeholder: Some("Search snippets…".into()),
            items: {
                let all = self.load();
                all.iter().map(|snippet| self.item(snippet, &all)).collect()
            },
            empty: Some("No snippets yet. Create one with Create Snippet.".into()),
            filters: Vec::new(),
            live: false,
        }))
    }
}

/// Name, text and keyword; saving runs `SaveSnippet` with `snippet`'s id
/// (0 creates a new one).
fn edit_form(title: &str, snippet: &Snippet) -> View {
    View::Form(FormView {
        title: title.into(),
        fields: vec![
            Field {
                id: "name".into(),
                label: "Name".into(),
                kind: FieldKind::Text {
                    placeholder: "Home address".into(),
                    default: snippet.name.clone(),
                    required: true,
                },
            },
            Field {
                id: "text".into(),
                label: "Snippet".into(),
                kind: FieldKind::TextArea {
                    placeholder: "Text, with {cursor}, {clipboard}, {date}, {argument}…".into(),
                    default: snippet.text.clone(),
                    required: true,
                },
            },
            Field {
                id: "keyword".into(),
                label: "Keyword".into(),
                kind: FieldKind::Text {
                    placeholder: ";addr, typed anywhere to paste it".into(),
                    default: snippet.keyword.clone().unwrap_or_default(),
                    required: false,
                },
            },
        ],
        submit_title: "Save Snippet".into(),
        submit: Command::SaveSnippet(Snippet {
            id: snippet.id,
            name: "{name}".into(),
            text: "{text}".into(),
            keyword: Some("{keyword}".into()),
        }),
    })
}

/// Pastes (or copies) `snippet`, its `{snippet}`s replaced from `all`,
/// after a form for its arguments if it has any.
fn paste_action(snippet: &Snippet, all: &[Snippet], paste: bool) -> Action {
    let text = snippet::inline(&snippet.text, all);
    let command = if snippet::arguments(&text).is_empty() {
        Command::PasteSnippet { text, paste }
    } else {
        Command::PushView(Box::new(snippet::arguments_form(
            &snippet.name,
            &text,
            paste,
        )))
    };
    let title = if paste {
        "Paste Snippet"
    } else {
        "Copy Snippet"
    };
    Action::new(title, command)
}

/// The text as saved, placeholders and all, and its keyword.
fn preview(snippet: &Snippet) -> Preview {
    let characters = snippet.text.chars().count();
    Preview {
        body: PreviewBody::Text(snippet.text.chars().take(PREVIEW_CHARS).collect()),
        metadata: vec![
            (
                "Keyword".into(),
                snippet.keyword.clone().unwrap_or_else(|| "None".into()),
            ),
            ("Characters".into(), characters.to_string()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use spotlight_core::view::FieldValue;

    use super::*;

    fn provider() -> Snippets {
        let store = Store::open_in_memory().unwrap();
        let address = Snippet {
            id: 0,
            name: "Address".into(),
            text: "Main St 1\nSpringfield".into(),
            keyword: Some(";addr".into()),
        };
        let greeting = Snippet {
            id: 0,
            name: "Greeting".into(),
            text: r#"Hi {argument name="Who" default="team"}, {cursor}"#.into(),
            keyword: None,
        };
        store.save_snippet(&address, 1).unwrap();
        store.save_snippet(&greeting, 2).unwrap();
        Snippets::new(Some(Arc::new(Mutex::new(store))))
    }

    fn list(provider: &Snippets) -> ListView {
        match provider.view(LIST_VIEW) {
            Some(View::List(list)) => list,
            other => panic!("not a list: {other:?}"),
        }
    }

    #[test]
    fn catalog_has_the_commands() {
        let titles: Vec<String> = provider()
            .catalog()
            .into_iter()
            .map(|item| item.title)
            .collect();
        assert_eq!(
            titles,
            [
                "Search Snippets",
                "Create Snippet",
                "Import Snippets",
                "Export Snippets"
            ]
        );
        // Without a database there's only the (empty) list.
        assert_eq!(Snippets::new(None).catalog().len(), 1);
    }

    #[test]
    fn rows_paste_copy_and_preview() {
        let list = list(&provider());
        let address = &list.items[0];
        assert_eq!(address.title, "Address");
        assert_eq!(address.subtitle.as_deref(), Some(";addr"));
        assert!(
            address
                .keywords
                .iter()
                .any(|word| word.contains("Springfield"))
        );
        let titles: Vec<&str> = address.actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Paste Snippet",
                "Copy Snippet",
                "Edit Snippet…",
                "Duplicate Snippet…",
                "Delete Snippet"
            ]
        );
        assert_eq!(
            address.actions[0].command,
            Command::PasteSnippet {
                text: "Main St 1\nSpringfield".into(),
                paste: true
            }
        );
        let preview = address.preview.as_ref().unwrap();
        assert_eq!(
            preview.body,
            PreviewBody::Text("Main St 1\nSpringfield".into())
        );
        assert_eq!(preview.metadata[0], ("Keyword".into(), ";addr".into()));
    }

    #[test]
    fn arguments_are_asked_for_first() {
        let list = list(&provider());
        let greeting = &list.items[1];
        let Command::PushView(view) = &greeting.actions[1].command else {
            panic!("no form: {:?}", greeting.actions[1]);
        };
        let View::Form(form) = view.as_ref() else {
            panic!("not a form");
        };
        assert_eq!(form.submit_title, "Copy");
        assert_eq!(
            form.fill(&[FieldValue::Text("Ann".into())]),
            Command::PasteSnippet {
                text: "Hi Ann, {cursor}".into(),
                paste: false
            }
        );
    }

    #[test]
    fn edit_and_duplicate_forms_fill_the_snippet() {
        let list = list(&provider());
        let address = &list.items[0];
        let form = |ix: usize| match &address.actions[ix].command {
            Command::PushView(view) => match view.as_ref() {
                View::Form(form) => form.clone(),
                other => panic!("not a form: {other:?}"),
            },
            other => panic!("not a view: {other:?}"),
        };
        let values = |form: &FormView| -> Vec<FieldValue> {
            form.fields
                .iter()
                .map(|field| match &field.kind {
                    FieldKind::Text { default, .. } | FieldKind::TextArea { default, .. } => {
                        FieldValue::Text(default.clone())
                    }
                    other => panic!("unexpected field {other:?}"),
                })
                .collect()
        };

        let edit = form(2);
        let Command::SaveSnippet(saved) = edit.fill(&values(&edit)) else {
            panic!("not a save");
        };
        assert_ne!(saved.id, 0);
        assert_eq!(saved.name, "Address");
        assert_eq!(saved.text, "Main St 1\nSpringfield");
        assert_eq!(saved.keyword.as_deref(), Some(";addr"));

        let duplicate = form(3);
        let Command::SaveSnippet(copy) = duplicate.fill(&values(&duplicate)) else {
            panic!("not a save");
        };
        assert_eq!(copy.id, 0);
        assert_eq!(copy.name, "Address Copy");
        assert_eq!(copy.keyword.as_deref(), Some(""));
    }
}
