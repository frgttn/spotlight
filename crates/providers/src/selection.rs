//! Commands on the text selected in another app: search the web for it,
//! translate it. The app reads the selection when one runs (see
//! `spotlight_platform_win::selection`); turning this source off in Settings
//! stops Spotlight from ever reading it, for quicklinks' `{selection}` too.

use std::sync::Arc;

use spotlight_core::{
    item::{Command, Icon, SELECTION, SearchItem},
    item_id,
    provider::Provider,
};

use crate::{glyph, web::Web};

pub const ID: &str = "selection";

pub struct Selection {
    /// For its search engine, which Settings can change.
    web: Arc<Web>,
    /// The language translations go to: `"ru"`.
    language: String,
}

impl Selection {
    pub fn new(web: Arc<Web>, language: String) -> Self {
        Self { web, language }
    }
}

impl Provider for Selection {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Selected Text"
    }

    fn description(&self) -> &'static str {
        "Commands for the text selected in other apps, and {selection} in quicklinks. Off: Spotlight never reads your selection."
    }

    fn catalog(&self) -> Vec<SearchItem> {
        let (name, template) = self.web.resolved();
        let search = template.replace("{query}", SELECTION);
        let translate = format!(
            "https://translate.google.com/?sl=auto&tl={}&text={SELECTION}&op=translate",
            self.language
        );
        vec![
            SearchItem::new(item_id(ID, "search"), "Search Selection on the Web")
                .subtitle(name)
                .keywords(&["selected text", "google", "look up"])
                .icon(Icon::Glyph(glyph::SEARCH))
                .action("Search in Browser", Command::OpenUrl(search)),
            SearchItem::new(item_id(ID, "translate"), "Translate Selection")
                .subtitle("Google Translate")
                .keywords(&["selected text", "translator", "language"])
                .icon(Icon::Glyph(glyph::TRANSLATE))
                .action("Translate in Browser", Command::OpenUrl(translate)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use spotlight_core::web::SearchEngine;

    use super::*;

    #[test]
    fn commands_take_the_selection() {
        let selection = Selection::new(Arc::new(Web::new(SearchEngine::default(), None)), "ru".into());
        let items = selection.catalog();
        for item in &items {
            assert!(item.actions[0].command.needs_selection(), "{}", item.title);
        }
        assert_eq!(
            items[1].actions[0].command.with_selection("hello"),
            Command::OpenUrl(
                "https://translate.google.com/?sl=auto&tl=ru&text=hello&op=translate".into()
            )
        );
    }
}
