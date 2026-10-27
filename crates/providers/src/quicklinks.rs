//! Quicklinks: links the user saved under a name, as root-search commands.
//! A link with arguments opens a form for them first; `!keyword text` in
//! root search opens it straight away with `text` filled in.

use std::sync::{Arc, Mutex, OnceLock, RwLock};

use spotlight_core::{
    deeplink, favicon,
    item::{Action, Command, Icon, SearchItem, shortcut},
    item_id,
    provider::Provider,
    quicklink::{self, Argument, DEFAULTS, Quicklink},
    view::{Field, FieldKind, FormView, ListView, View},
    web::SearchEngine,
};
use spotlight_platform_win::links::{self, Browser};
use spotlight_store::Store;

use crate::glyph;

pub const ID: &str = "quicklinks";

/// How many quicklinks `!` followed by part of a keyword lists.
const BANG_SUGGESTIONS: usize = 8;

pub struct Quicklinks {
    /// `None` without a database: the defaults are then offered, read-only.
    store: Option<Arc<Mutex<Store>>>,
    /// As of the last catalog load; [`Provider::answers`] reads it on every keystroke.
    links: RwLock<Vec<Quicklink>>,
    /// Looked up once, for the Open With choice.
    browsers: OnceLock<Vec<Browser>>,
}

impl Quicklinks {
    pub fn new(store: Option<Arc<Mutex<Store>>>) -> Self {
        Self {
            store,
            links: RwLock::new(Vec::new()),
            browsers: OnceLock::new(),
        }
    }

    fn load(&self) -> Vec<Quicklink> {
        let Some(store) = &self.store else {
            return DEFAULTS
                .iter()
                .enumerate()
                // Negative ids: never saved, so never edited or deleted.
                .map(|(ix, (name, keyword, link))| Quicklink {
                    id: -(ix as i64) - 1,
                    name: (*name).to_owned(),
                    link: (*link).to_owned(),
                    keyword: Some((*keyword).to_owned()),
                    open_with: None,
                })
                .collect();
        };
        let links = store
            .lock()
            .map_err(|_| "store lock poisoned".to_owned())
            .and_then(|store| store.quicklinks().map_err(|err| err.to_string()));
        links.unwrap_or_else(|err| {
            eprintln!("[quicklinks] could not read them: {err}");
            Vec::new()
        })
    }

    fn browsers(&self) -> &[Browser] {
        self.browsers.get_or_init(links::browsers)
    }

    fn editable(&self) -> bool {
        self.store.is_some()
    }

    fn item(&self, link: &Quicklink) -> SearchItem {
        let mut subtitle = "Quicklink".to_owned();
        if let Some(keyword) = &link.keyword {
            subtitle = format!("!{keyword}  ·  {subtitle}");
        }
        let mut item = SearchItem::new(item_id(ID, &link.id.to_string()), link.name.clone())
            .subtitle(subtitle)
            .icon(icon(link));
        item.keywords = link
            .keyword
            .iter()
            .cloned()
            .chain(quicklink::host(&link.link).map(str::to_owned))
            .collect();
        item.actions.push(open_action(link, &[]));
        let mut item = item.action("Copy Link", Command::Copy(link.link.clone()));
        if self.editable() && quicklink::search_template(&link.link).is_some() {
            item = item.action(
                "Use as Web Search",
                Command::SetSearchEngine {
                    engine: SearchEngine::Quicklink(link.id),
                    name: link.name.clone(),
                },
            );
        }
        if self.editable() {
            let edit = self.edit_form("Edit Quicklink", link);
            item = item
                .action("Edit Quicklink…", Command::PushView(Box::new(edit)))
                .shortcut(shortcut::EDIT)
                .action("Delete Quicklink", Command::DeleteQuicklink(link.id));
        }
        item
    }

    /// Name, link, keyword and browser; saving runs `SaveQuicklink` with
    /// `link`'s id (0 creates a new one).
    fn edit_form(&self, title: &str, link: &Quicklink) -> View {
        let text = |id: &str, label: &str, placeholder: &str, default: &str, required| Field {
            id: id.into(),
            label: label.into(),
            kind: FieldKind::Text {
                placeholder: placeholder.into(),
                default: default.into(),
                required,
            },
        };
        let mut apps = vec![("Default App".to_owned(), String::new())];
        apps.extend(
            self.browsers()
                .iter()
                .map(|browser| (browser.name.clone(), browser.exe.clone())),
        );
        let mut current = 0;
        if let Some(app) = &link.open_with {
            current = match apps
                .iter()
                .position(|(_, exe)| exe.eq_ignore_ascii_case(app))
            {
                Some(ix) => ix,
                // No longer registered: still offered, so editing keeps it.
                None => {
                    let name = std::path::Path::new(app)
                        .file_name()
                        .map_or_else(|| app.clone(), |name| name.to_string_lossy().into_owned());
                    apps.push((name, app.clone()));
                    apps.len() - 1
                }
            };
        }
        View::Form(FormView {
            title: title.into(),
            fields: vec![
                text("name", "Name", "Search GitHub", &link.name, true),
                text(
                    "link",
                    "Link",
                    "https://github.com/search?q={query}",
                    &link.link,
                    true,
                ),
                text(
                    "keyword",
                    "Keyword",
                    "gh, to open it with !gh",
                    link.keyword.as_deref().unwrap_or_default(),
                    false,
                ),
                Field {
                    id: "open_with".into(),
                    label: "Open With".into(),
                    kind: FieldKind::Dropdown {
                        options: apps,
                        default: current,
                    },
                },
            ],
            submit_title: "Save Quicklink".into(),
            submit: Command::SaveQuicklink(Quicklink {
                id: link.id,
                name: "{name}".into(),
                link: "{link}".into(),
                keyword: Some("{keyword}".into()),
                open_with: Some("{open_with}".into()),
            }),
        })
    }
}

impl Provider for Quicklinks {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Quicklinks"
    }

    fn description(&self) -> &'static str {
        "Your saved links and !keyword searches."
    }

    /// The form of a quicklink whose arguments all have defaults, for a
    /// link that brings arguments (see `deeplink::ARGUMENTS_VIEW`).
    fn view(&self, id: &str) -> Option<View> {
        let id: i64 = id.strip_prefix(deeplink::ARGUMENTS_VIEW)?.parse().ok()?;
        let links = self.links.read().ok()?;
        let link = links.iter().find(|link| link.id == id)?;
        let arguments = quicklink::arguments(&link.link);
        (!arguments.is_empty()).then(|| arguments_form(link, &arguments, &[]))
    }

    fn catalog(&self) -> Vec<SearchItem> {
        let links = self.load();
        // The stock quicklinks are too many for root search: they're reached
        // with `!keyword` or through Search Quicklinks. Any the user has
        // changed, and their own, are listed in root search as usual.
        let rows: Vec<SearchItem> = links.iter().map(|link| self.item(link)).collect();
        let mut items: Vec<SearchItem> = links
            .iter()
            .zip(&rows)
            .filter(|(link, _)| !is_stock(link))
            .map(|(_, row)| row.clone())
            .collect();
        let list = View::List(ListView {
            title: "Quicklinks".into(),
            placeholder: Some("Search quicklinks…".into()),
            items: rows,
            empty: Some("No quicklinks yet. Create one with Create Quicklink.".into()),
            filters: Vec::new(),
            live: false,
        });
        items.push(
            SearchItem::new(item_id(ID, "search"), "Search Quicklinks")
                .subtitle("Quicklinks")
                .keywords(&["bookmarks", "keywords", "!"])
                .icon(Icon::Glyph(glyph::LINK))
                .action("Show Quicklinks", Command::PushView(Box::new(list))),
        );
        if self.editable() {
            let blank = Quicklink {
                id: 0,
                name: String::new(),
                link: String::new(),
                keyword: None,
                open_with: None,
            };
            let create = self.edit_form("Create Quicklink", &blank);
            items.extend([
                SearchItem::new(item_id(ID, "create"), "Create Quicklink")
                    .subtitle("Quicklinks")
                    .keywords(&["new quicklink", "add link", "bookmark"])
                    .icon(Icon::Glyph(glyph::ADD))
                    .action("Create Quicklink", Command::PushView(Box::new(create))),
                SearchItem::new(item_id(ID, "import"), "Import Quicklinks")
                    .subtitle("Quicklinks")
                    .keywords(&["raycast", "json", "bookmarks"])
                    .icon(Icon::Glyph(glyph::IMPORT))
                    .action("Import from JSON…", Command::ImportQuicklinks),
                SearchItem::new(item_id(ID, "export"), "Export Quicklinks")
                    .subtitle("Quicklinks")
                    .keywords(&["raycast", "json", "backup"])
                    .icon(Icon::Glyph(glyph::EXPORT))
                    .action("Export to JSON…", Command::ExportQuicklinks),
            ]);
        }
        if let Ok(mut cached) = self.links.write() {
            *cached = links;
        }
        items
    }

    /// `!gh tokio` (or `tokio !gh`): the quicklink with keyword "gh" with
    /// "tokio" filled in. While the keyword is typed, `!g` lists those
    /// starting with "g"; Enter completes one to `!gh `.
    fn answers(&self, query: &str) -> Vec<SearchItem> {
        let Some((keyword, text)) = quicklink::parse_bang(query) else {
            return Vec::new();
        };
        let Ok(links) = self.links.read() else {
            return Vec::new();
        };
        if let Some(text) = text {
            return links
                .iter()
                .find(|link| link.keyword.as_deref() == Some(keyword.as_str()))
                .map(|link| self.filled(link, &keyword, text))
                .into_iter()
                .collect();
        }

        // `!gh ` with the space typed: the keyword is complete.
        let complete = query.ends_with(char::is_whitespace);
        let matches = |known: &str| {
            if complete {
                known == keyword
            } else {
                known.starts_with(&keyword)
            }
        };
        links
            .iter()
            .filter(|link| link.keyword.as_deref().is_some_and(matches))
            .take(BANG_SUGGESTIONS)
            .map(|link| {
                let mut item = self.item(link);
                let trigger = link.keyword.as_deref().unwrap_or_default();
                // A link that takes no typed text (`{clipboard}` only) opens
                // on Enter once its keyword is typed in full.
                let takes_text = quicklink::arguments(&link.link)
                    .iter()
                    .any(Argument::is_free_text);
                if !complete && (takes_text || trigger != keyword) {
                    item.actions.insert(0, complete_action(trigger));
                }
                item
            })
            .collect()
    }
}

impl Quicklinks {
    /// `link` opened with `!keyword text`.
    fn filled(&self, link: &Quicklink, keyword: &str, text: &str) -> SearchItem {
        let values = quicklink::bang_values(&quicklink::arguments(&link.link), text);
        let filled = quicklink::fill(&link.link, &values);
        SearchItem::new(
            item_id(ID, &link.id.to_string()),
            format!("{} “{text}”", link.name),
        )
        .subtitle(format!("!{keyword}  ·  Quicklink"))
        .icon(icon(link))
        .action("Open", open_action(link, &values).command)
        .action("Copy Link", Command::Copy(filled))
    }
}

/// Enter on a suggestion: `!g` becomes `!gh `, ready for the text.
fn complete_action(trigger: &str) -> Action {
    Action::new(
        format!("Use !{trigger}"),
        Command::Complete(format!("!{trigger} ")),
    )
}

/// Opens `link`, or its form when an argument is still missing: `values` are
/// the ones already known (in argument order), the rest take defaults.
fn open_action(link: &Quicklink, values: &[String]) -> Action {
    let arguments = quicklink::arguments(&link.link);
    let known = |ix: usize| {
        values.get(ix).is_some_and(|value| !value.trim().is_empty())
            || !arguments[ix].fallback().is_empty()
    };
    let command = if (0..arguments.len()).all(known) {
        Command::OpenQuicklink {
            link: quicklink::fill(&link.link, values),
            app: link.open_with.clone(),
        }
    } else {
        Command::PushView(Box::new(arguments_form(link, &arguments, values)))
    };
    Action::new("Open Quicklink", command)
}

/// A field per argument; submitting opens the link with them.
fn arguments_form(link: &Quicklink, arguments: &[Argument], values: &[String]) -> View {
    let fields = arguments
        .iter()
        .enumerate()
        .map(|(ix, argument)| {
            let value = values
                .get(ix)
                .filter(|value| !value.trim().is_empty())
                .map_or(argument.default.as_str(), String::as_str);
            let kind = if argument.is_free_text() {
                FieldKind::Text {
                    placeholder: argument.name.clone(),
                    default: value.to_owned(),
                    required: true,
                }
            } else {
                FieldKind::Dropdown {
                    default: argument
                        .options
                        .iter()
                        .position(|option| option == value)
                        .unwrap_or(0),
                    options: argument
                        .options
                        .iter()
                        .map(|option| (option.clone(), option.clone()))
                        .collect(),
                }
            };
            Field {
                id: quicklink::field_id(ix),
                label: argument.name.clone(),
                kind,
            }
        })
        .collect();
    View::Form(FormView {
        title: link.name.clone(),
        fields,
        submit_title: "Open".into(),
        submit: Command::OpenQuicklink {
            link: quicklink::with_field_ids(&link.link),
            app: link.open_with.clone(),
        },
    })
}

/// Still exactly as [`DEFAULTS`] saved it.
fn is_stock(link: &Quicklink) -> bool {
    link.open_with.is_none()
        && DEFAULTS.iter().any(|(name, keyword, url)| {
            link.name == *name && link.link == *url && link.keyword.as_deref() == Some(*keyword)
        })
}

/// The browser it opens in, the folder or file it opens, or a link symbol.
fn icon(link: &Quicklink) -> Icon {
    let target = link.link.trim();
    // A site's own icon says more than the browser it opens in.
    if let Some(origin) = favicon::origin(target) {
        return Icon::Site {
            origin,
            glyph: glyph::LINK,
        };
    }
    if let Some(app) = &link.open_with {
        return Icon::Shell(app.clone());
    }
    if !quicklink::is_url(target) && !target.contains(['{', '%']) {
        return Icon::Shell(target.to_owned());
    }
    Icon::Glyph(glyph::LINK)
}

#[cfg(test)]
mod tests {
    use spotlight_core::view::FieldValue;

    use super::*;

    fn provider() -> Quicklinks {
        let store = Store::open_in_memory().unwrap();
        let provider = Quicklinks::new(Some(Arc::new(Mutex::new(store))));
        // Let tests not depend on the browsers of the machine.
        provider.browsers.set(Vec::new()).unwrap();
        provider
    }

    fn titles(items: &[SearchItem]) -> Vec<&str> {
        items.iter().map(|item| item.title.as_str()).collect()
    }

    fn find<'a>(items: &'a [SearchItem], title: &str) -> &'a SearchItem {
        items
            .iter()
            .find(|item| item.title == title)
            .unwrap_or_else(|| panic!("no {title:?}"))
    }

    /// What Search Quicklinks lists: every quicklink, root search or not.
    fn all_links(catalog: &[SearchItem]) -> Vec<SearchItem> {
        let Command::PushView(view) = &find(catalog, "Search Quicklinks").actions[0].command else {
            panic!("expected a list");
        };
        let View::List(list) = view.as_ref() else {
            panic!("expected a list");
        };
        list.items.clone()
    }

    #[test]
    fn stock_quicklinks_stay_out_of_root_search() {
        let provider = provider();
        let items = provider.catalog();
        assert_eq!(
            titles(&items),
            [
                "Search Quicklinks",
                "Create Quicklink",
                "Import Quicklinks",
                "Export Quicklinks"
            ]
        );
        assert_eq!(all_links(&items).len(), DEFAULTS.len());

        // Changing one (or adding one) brings it to root search.
        let links = provider.links.read().unwrap().clone();
        let mut github = links.iter().find(|l| l.name == "GitHub").unwrap().clone();
        github.name = "My GitHub".into();
        provider
            .store
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .save_quicklink(&github)
            .unwrap();
        let items = provider.catalog();
        assert_eq!(titles(&items)[0], "My GitHub");
        assert_eq!(items.len(), 5);
    }

    #[test]
    fn catalog_has_the_defaults_and_commands() {
        let provider = provider();
        let catalog = provider.catalog();
        assert_eq!(catalog.len(), 4);
        let items = all_links(&catalog);
        assert_eq!(items.len(), DEFAULTS.len());
        let github = find(&items, "GitHub");
        assert_eq!(github.subtitle.as_deref(), Some("!gh  ·  Quicklink"));
        assert!(github.keywords.contains(&"github.com".to_owned()));
        // It has an argument, so Enter opens a form for it.
        let Command::PushView(view) = &github.actions[0].command else {
            panic!("expected a form");
        };
        let View::Form(form) = view.as_ref() else {
            panic!("expected a form");
        };
        assert_eq!(form.fields.len(), 1);
        assert_eq!(
            form.fill(&[FieldValue::Text("rust gpui".into())]),
            Command::OpenQuicklink {
                link: "https://github.com/search?q=rust%20gpui".into(),
                app: None
            }
        );
        let titles: Vec<_> = github.actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Open Quicklink",
                "Copy Link",
                "Use as Web Search",
                "Edit Quicklink…",
                "Delete Quicklink"
            ]
        );
        let id = github
            .id
            .strip_prefix("quicklinks:")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            github.actions[2].command,
            Command::SetSearchEngine {
                engine: SearchEngine::Quicklink(id),
                name: "GitHub".into()
            }
        );
        find(&catalog, "Create Quicklink");
    }

    #[test]
    fn keywords_open_with_the_text() {
        let provider = provider();
        provider.catalog();
        let items = provider.answers("!GH tokio rt");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "GitHub “tokio rt”");
        assert_eq!(
            items[0].actions[0].command,
            Command::OpenQuicklink {
                link: "https://github.com/search?q=tokio%20rt".into(),
                app: None
            }
        );
        // The same id as the catalog row, so using it counts for its ranking.
        assert_eq!(
            items[0].id,
            find(&all_links(&provider.catalog()), "GitHub").id
        );

        // Translate's language has a default, so it opens directly too.
        let items = provider.answers("!tr good morning");
        assert!(matches!(
            &items[0].actions[0].command,
            Command::OpenQuicklink { link, .. } if link.ends_with("text=good%20morning&tl=en")
        ));

        assert!(provider.answers("!nope x").is_empty());
        assert!(provider.answers("gh tokio").is_empty());
    }

    #[test]
    fn a_partial_keyword_lists_matching_quicklinks() {
        let provider = provider();
        provider.catalog();
        // By name; Enter completes the keyword, ready for the text.
        let items = provider.answers("!gi");
        assert_eq!(titles(&items), ["Giphy", "Google Images"]);
        assert_eq!(
            items[0].actions[0].command,
            Command::Complete("!giphy ".into())
        );
        assert_eq!(
            items[1].actions[0].command,
            Command::Complete("!gi ".into())
        );
        assert_eq!(titles(&provider.answers("!sp")), ["Spotify"]);
        assert!(provider.answers("!zzz").is_empty());

        // With the space typed, only that keyword, and Enter opens its form.
        let items = provider.answers("!gh ");
        assert_eq!(titles(&items), ["GitHub"]);
        assert_eq!(items[0].actions[0].title, "Open Quicklink");
    }

    #[test]
    fn a_full_keyword_without_text_opens_directly() {
        let provider = provider();
        let link = Quicklink {
            id: 0,
            name: "Translate From Clipboard".into(),
            link: "https://translate.google.com/?text={clipboard}".into(),
            keyword: Some("tracl".into()),
            open_with: None,
        };
        let store = provider.store.as_ref().unwrap();
        store.lock().unwrap().save_quicklink(&link).unwrap();
        provider.catalog();

        let items = provider.answers("!tracl");
        assert_eq!(titles(&items), ["Translate From Clipboard"]);
        assert!(matches!(
            items[0].actions[0].command,
            Command::OpenQuicklink { .. }
        ));
        // Still being typed: Enter completes it.
        let items = provider.answers("!tra");
        let item = find(&items, "Translate From Clipboard");
        assert_eq!(item.actions[0].command, Command::Complete("!tracl ".into()));
    }

    #[test]
    fn links_without_arguments_open_directly() {
        let link = Quicklink {
            id: 5,
            name: "Downloads".into(),
            link: r"%USERPROFILE%\Downloads".into(),
            keyword: None,
            open_with: None,
        };
        assert_eq!(
            open_action(&link, &[]).command,
            Command::OpenQuicklink {
                link: link.link.clone(),
                app: None
            }
        );
        assert_eq!(icon(&link), Icon::Glyph(glyph::LINK));
    }

    #[test]
    fn edit_form_saves_into_the_same_quicklink() {
        let provider = provider();
        let link = Quicklink {
            id: 7,
            name: "Docs".into(),
            link: "https://docs.rs/{query}".into(),
            keyword: Some("d".into()),
            open_with: Some(r"C:\Gone\browser.exe".into()),
        };
        let View::Form(form) = provider.edit_form("Edit Quicklink", &link) else {
            panic!("expected a form");
        };
        let FieldKind::Dropdown { options, default } = &form.fields[3].kind else {
            panic!("expected a dropdown");
        };
        // The browser isn't installed any more, but stays chosen.
        assert_eq!(options[*default].0, "browser.exe");
        let values = [
            FieldValue::Text("Docs".into()),
            FieldValue::Text("https://docs.rs/{query}".into()),
            FieldValue::Text("d".into()),
            FieldValue::Dropdown(*default),
        ];
        assert_eq!(form.fill(&values), Command::SaveQuicklink(link));
    }

    #[test]
    fn without_a_database_the_defaults_are_read_only() {
        let provider = Quicklinks::new(None);
        provider.browsers.set(Vec::new()).unwrap();
        let catalog = provider.catalog();
        assert_eq!(catalog.len(), 1);
        let items = all_links(&catalog);
        assert_eq!(items.len(), DEFAULTS.len());
        let titles: Vec<_> = items[0].actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["Open Quicklink", "Copy Link"]);
    }
}
