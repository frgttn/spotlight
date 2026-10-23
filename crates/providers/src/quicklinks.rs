//! Quicklinks: links the user saved under a name, as root-search commands.
//! A link with arguments opens a form for them first; `!keyword text` in
//! root search opens it straight away with `text` filled in.
//!
//! `!keyword` also reaches the ten thousand search bangs of [`Bangs`] when no
//! quicklink of the user's has that keyword.

use std::{
    sync::{Arc, Mutex, OnceLock, RwLock},
    thread,
};

use spotlight_core::{
    item::{Action, Command, Icon, SearchItem},
    item_id,
    provider::Provider,
    quicklink::{self, Argument, DEFAULTS, Quicklink},
    view::{Field, FieldKind, FormView, ListView, View},
};
use spotlight_platform_win::links::{self, Browser};
use spotlight_store::Store;

use crate::{
    bangs::{self, Bang, Bangs},
    glyph,
};

pub const ID: &str = "quicklinks";
/// Item ids of bang rows: this, then the bang's trigger.
const BANG_ITEM: &str = "bangs";
/// The view id of the list of every bang, for `Command::OpenProviderView`.
const BANGS_VIEW: &str = "bangs";

/// How many quicklinks and bangs `!` followed by part of a keyword lists.
const BANG_SUGGESTIONS: usize = 8;

pub struct Quicklinks {
    /// `None` without a database: the defaults are then offered, read-only.
    store: Option<Arc<Mutex<Store>>>,
    /// As of the last catalog load; [`Provider::answers`] reads it on every keystroke.
    links: RwLock<Vec<Quicklink>>,
    /// Looked up once, for the Open With choice.
    browsers: OnceLock<Vec<Browser>>,
    bangs: Arc<Bangs>,
}

impl Quicklinks {
    pub fn new(store: Option<Arc<Mutex<Store>>>, bangs: Bangs) -> Self {
        Self {
            store,
            links: RwLock::new(Vec::new()),
            browsers: OnceLock::new(),
            bangs: Arc::new(bangs),
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
        if self.editable() {
            let edit = self.edit_form("Edit Quicklink", link);
            item = item
                .action("Edit Quicklink…", Command::PushView(Box::new(edit)))
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

    fn catalog(&self) -> Vec<SearchItem> {
        // Reading and refreshing the bangs (a 1.4 MB download) mustn't hold
        // up the user's own quicklinks.
        let bangs = self.bangs.clone();
        let loading = thread::Builder::new()
            .name("bangs".into())
            .spawn(move || bangs.load());
        if let Err(err) = loading {
            eprintln!("[quicklinks] could not start loading bangs: {err}");
        }

        let links = self.load();
        let mut items: Vec<SearchItem> = links.iter().map(|link| self.item(link)).collect();
        let list = View::List(ListView {
            title: "Quicklinks".into(),
            placeholder: Some("Search quicklinks…".into()),
            items: items.clone(),
            empty: Some("No quicklinks yet. Create one with Create Quicklink.".into()),
        });
        items.push(
            SearchItem::new(item_id(ID, "search"), "Search Quicklinks")
                .subtitle("Quicklinks")
                .keywords(&["bookmarks", "bangs"])
                .icon(Icon::Glyph(glyph::LINK))
                .action("Show Quicklinks", Command::PushView(Box::new(list))),
        );
        items.push(
            SearchItem::new(item_id(ID, "search-bangs"), "Search Bangs")
                .subtitle("Quicklinks")
                .keywords(&["bangs", "!", "kagi", "helium", "duckduckgo"])
                .icon(Icon::Glyph(glyph::SEARCH))
                .action(
                    "Show Bangs",
                    Command::OpenProviderView {
                        provider: ID.into(),
                        view: BANGS_VIEW.into(),
                    },
                ),
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
            items.push(
                SearchItem::new(item_id(ID, "create"), "Create Quicklink")
                    .subtitle("Quicklinks")
                    .keywords(&["new quicklink", "add link", "bookmark", "bang"])
                    .icon(Icon::Glyph(glyph::ADD))
                    .action("Create Quicklink", Command::PushView(Box::new(create))),
            );
        }
        if let Ok(mut cached) = self.links.write() {
            *cached = links;
        }
        items
    }

    /// `!gh tokio` (or `tokio !gh`): the quicklink with keyword "gh" with
    /// "tokio" filled in, or else the bang "gh". While the keyword is typed,
    /// `!g` lists those starting with "g"; Enter completes one to `!gh `.
    fn answers(&self, query: &str) -> Vec<SearchItem> {
        let Some((keyword, text)) = quicklink::parse_bang(query) else {
            return Vec::new();
        };
        let Ok(links) = self.links.read() else {
            return Vec::new();
        };
        if let Some(text) = text {
            if let Some(link) = links
                .iter()
                .find(|link| link.keyword.as_deref() == Some(keyword.as_str()))
            {
                return vec![self.filled(link, &keyword, text)];
            }
            return self
                .bangs
                .find(&keyword)
                .map(|bang| self.bang_answer(&keyword, &bang, text))
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
        let mut items: Vec<SearchItem> = links
            .iter()
            .filter(|link| link.keyword.as_deref().is_some_and(matches))
            .take(BANG_SUGGESTIONS)
            .map(|link| {
                let mut item = self.item(link);
                if !complete {
                    let trigger = link.keyword.as_deref().unwrap_or_default();
                    item.actions.insert(0, complete_action(trigger));
                }
                item
            })
            .collect();
        let taken = |trigger: &str| {
            links
                .iter()
                .any(|link| link.keyword.as_deref() == Some(trigger))
        };
        if complete {
            if !taken(&keyword)
                && let Some(bang) = self.bangs.find(&keyword)
            {
                items.push(self.bang_suggestion(&keyword, &bang, true));
            }
        } else {
            let room = BANG_SUGGESTIONS.saturating_sub(items.len());
            items.extend(
                self.bangs
                    .starting_with(&keyword, room, taken)
                    .into_iter()
                    .map(|(trigger, bang)| self.bang_suggestion(&trigger, &bang, false)),
            );
        }
        items
    }

    /// Every bang, to browse like Helium's bangs page.
    fn view(&self, id: &str) -> Option<View> {
        if id != BANGS_VIEW {
            return None;
        }
        let bangs = self.bangs.all();
        Some(View::List(ListView {
            title: "Bangs".into(),
            placeholder: Some(format!(
                "Search {} bangs by site or trigger · {}",
                bangs.len(),
                bangs::ATTRIBUTION
            )),
            items: bangs.iter().map(bang_row).collect(),
            empty: Some(
                "The bangs haven't been downloaded yet. Check the connection and try again.".into(),
            ),
        }))
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

    /// The bang `trigger` opened with `text`.
    fn bang_answer(&self, trigger: &str, bang: &Bang, text: &str) -> SearchItem {
        let url = bang.url(text);
        let mut item = SearchItem::new(
            item_id(BANG_ITEM, trigger),
            format!("{} “{text}”", bang.name),
        )
        .subtitle(format!("!{trigger}  ·  {}", bang.host()))
        .icon(Icon::Glyph(glyph::LINK))
        // A list from the web may only open web pages, unlike the user's own links.
        .action("Open", Command::OpenUrl(url.clone()))
        .action("Copy Link", Command::Copy(url));
        self.add_save_action(&mut item, trigger, bang);
        item
    }

    /// A bang whose trigger is being typed (Enter completes it) or is
    /// complete (Enter opens the site).
    fn bang_suggestion(&self, trigger: &str, bang: &Bang, complete: bool) -> SearchItem {
        let mut item = SearchItem::new(item_id(BANG_ITEM, trigger), bang.name.clone())
            .subtitle(format!("!{trigger}  ·  {}", bang.host()))
            .icon(Icon::Glyph(glyph::LINK));
        if !complete {
            item.actions.push(complete_action(trigger));
        }
        let mut item = item.action(
            format!("Open {}", bang.host()),
            Command::OpenUrl(bang.home()),
        );
        self.add_save_action(&mut item, trigger, bang);
        item
    }

    /// "Save as Quicklink…": the bang as a quicklink of the user's, to
    /// change, under the same keyword.
    fn add_save_action(&self, item: &mut SearchItem, trigger: &str, bang: &Bang) {
        if !self.editable() {
            return;
        }
        let link = Quicklink {
            id: 0,
            name: bang.name.clone(),
            link: bang.quicklink(),
            keyword: Some(trigger.to_owned()),
            open_with: None,
        };
        let form = self.edit_form("Save Bang as Quicklink", &link);
        item.actions.push(Action {
            title: "Save as Quicklink…".into(),
            command: Command::PushView(Box::new(form)),
        });
    }
}

/// Enter on a suggestion: `!g` becomes `!gh `, ready for the text.
fn complete_action(trigger: &str) -> Action {
    Action {
        title: format!("Use !{trigger}"),
        command: Command::Complete(format!("!{trigger} ")),
    }
}

/// A row of the bangs list, found by name, trigger or site.
fn bang_row(bang: &Bang) -> SearchItem {
    let trigger = &bang.triggers[0];
    let shown: Vec<String> = bang
        .triggers
        .iter()
        .take(3)
        .map(|trigger| format!("!{trigger}"))
        .collect();
    let mut item = SearchItem::new(item_id(BANG_ITEM, trigger), bang.name.clone())
        .subtitle(shown.join("  "))
        .icon(Icon::Glyph(glyph::LINK))
        .action(
            format!("Use !{trigger}"),
            Command::Complete(format!("!{trigger} ")),
        )
        .action(
            format!("Open {}", bang.host()),
            Command::OpenUrl(bang.home()),
        )
        .action("Copy Bang", Command::Copy(format!("!{trigger}")));
    item.keywords = bang.triggers.clone();
    item.keywords.push(bang.host().to_owned());
    item
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
    Action {
        title: "Open Quicklink".into(),
        command,
    }
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

/// The browser it opens in, the folder or file it opens, or a link symbol.
fn icon(link: &Quicklink) -> Icon {
    if let Some(app) = &link.open_with {
        return Icon::Shell(app.clone());
    }
    let target = link.link.trim();
    if !quicklink::is_url(target) && !target.contains(['{', '%']) {
        return Icon::Shell(target.to_owned());
    }
    Icon::Glyph(glyph::LINK)
}

#[cfg(test)]
mod tests {
    use spotlight_core::view::FieldValue;

    use super::*;

    /// GitHub's bang loses "gh" to the default quicklink, but keeps "git".
    const BANGS: &str = r#"[
        {"s": "GitHub", "ts": ["gh", "git", "github"], "u": "https://github.com/search?q={searchTerms}"},
        {"s": "Startpage", "ts": ["sp", "startpage"], "u": "https://www.startpage.com/do/search?q={searchTerms}"},
        {"s": "Wikipedia", "ts": ["wiki"], "u": "https://wikipedia.org/w/index.php?search={searchTerms}"}
    ]"#;

    fn provider() -> Quicklinks {
        let store = Store::open_in_memory().unwrap();
        let provider = Quicklinks::new(Some(Arc::new(Mutex::new(store))), Bangs::from_json(BANGS));
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

    #[test]
    fn catalog_has_the_defaults_and_commands() {
        let provider = provider();
        let items = provider.catalog();
        assert_eq!(items.len(), DEFAULTS.len() + 3);
        let github = find(&items, "Search GitHub");
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
                "Edit Quicklink…",
                "Delete Quicklink"
            ]
        );
        find(&items, "Create Quicklink");
        find(&items, "Search Bangs");
    }

    #[test]
    fn bangs_open_with_the_text() {
        let provider = provider();
        provider.catalog();
        let items = provider.answers("!GH tokio rt");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Search GitHub “tokio rt”");
        assert_eq!(
            items[0].actions[0].command,
            Command::OpenQuicklink {
                link: "https://github.com/search?q=tokio%20rt".into(),
                app: None
            }
        );
        // The same id as the catalog row, so using it counts for its ranking.
        assert_eq!(items[0].id, find(&provider.catalog(), "Search GitHub").id);

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
    fn bangs_reach_every_other_site() {
        let provider = provider();
        provider.catalog();
        for query in ["!sp rust gpui", "rust gpui !SP"] {
            let items = provider.answers(query);
            assert_eq!(titles(&items), ["Startpage “rust gpui”"], "{query}");
            assert_eq!(
                items[0].actions[0].command,
                Command::OpenUrl("https://www.startpage.com/do/search?q=rust+gpui".into())
            );
            assert_eq!(items[0].actions[2].title, "Save as Quicklink…");
        }
        // The user's quicklink keeps its keyword; the bang its other triggers.
        assert_eq!(titles(&provider.answers("!gh x")), ["Search GitHub “x”"]);
        assert_eq!(titles(&provider.answers("!git x")), ["GitHub “x”"]);
    }

    #[test]
    fn a_partial_bang_lists_matching_keywords() {
        let provider = provider();
        provider.catalog();
        let items = provider.answers("!s");
        assert_eq!(titles(&items), ["Search Stack Overflow", "Startpage"]);
        // Enter completes the keyword, ready for the text.
        assert_eq!(
            items[0].actions[0].command,
            Command::Complete("!so ".into())
        );
        assert_eq!(
            items[1].actions[0].command,
            Command::Complete("!sp ".into())
        );
        assert_eq!(titles(&provider.answers("!g")), ["Search GitHub", "GitHub"]);
        assert_eq!(items[1].subtitle.as_deref(), Some("!sp  ·  startpage.com"));

        // With the space typed, only that keyword, and Enter opens the site.
        let items = provider.answers("!sp ");
        assert_eq!(titles(&items), ["Startpage"]);
        assert_eq!(
            items[0].actions[0].command,
            Command::OpenUrl("https://www.startpage.com/".into())
        );
    }

    #[test]
    fn lists_every_bang() {
        let provider = provider();
        let Some(View::List(list)) = provider.view("bangs") else {
            panic!("expected a list");
        };
        assert_eq!(titles(&list.items), ["GitHub", "Startpage", "Wikipedia"]);
        assert_eq!(
            list.items[0].subtitle.as_deref(),
            Some("!gh  !git  !github")
        );
        assert!(list.items[0].keywords.contains(&"github.com".to_owned()));
        assert_eq!(provider.view("other"), None);
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
        let provider = Quicklinks::new(None, Bangs::from_json(BANGS));
        provider.browsers.set(Vec::new()).unwrap();
        let items = provider.catalog();
        assert_eq!(items.len(), DEFAULTS.len() + 2);
        let titles: Vec<_> = items[0].actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["Open Quicklink", "Copy Link"]);
    }
}
