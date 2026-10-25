//! Root search: one result list from every provider.
//!
//! The list is, in order: answers computed from the query (calculator, URL),
//! catalog items ranked by [`Searcher`], then fallbacks (web search).

use std::{collections::HashMap, sync::Arc};

use spotlight_core::{item::SearchItem, provider::Provider};

use crate::{Candidate, Searcher};

/// One row of the result list.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// Index into [`RootSearch::catalog`].
    Catalog(usize),
    /// Computed for this query only: an answer or a fallback. Not tracked by frecency.
    Computed(SearchItem),
}

pub struct RootSearch {
    searcher: Searcher,
    providers: Vec<Arc<dyn Provider>>,
    /// Whether each provider takes part, by provider index. A turned-off one
    /// keeps no catalog and adds no answers or fallbacks.
    enabled: Vec<bool>,
    /// Each provider's catalog, by provider index, as last loaded.
    catalogs: Vec<Vec<SearchItem>>,
    /// `catalogs` flattened in provider order, so empty-query order is stable
    /// whichever provider finishes loading first.
    catalog: Vec<SearchItem>,
    /// How the last search's rows divide: answers, then catalog matches,
    /// then fallbacks.
    last_split: Split,
}

/// How a search's rows divide into its parts, by count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Split {
    pub answers: usize,
    pub matches: usize,
    pub fallbacks: usize,
}

impl RootSearch {
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        Self {
            searcher: Searcher::default(),
            enabled: vec![true; providers.len()],
            catalogs: vec![Vec::new(); providers.len()],
            providers,
            catalog: Vec::new(),
            last_split: Split::default(),
        }
    }

    /// How the rows of the last [`Self::search`] divide.
    pub fn last_split(&self) -> Split {
        self.last_split
    }

    pub fn providers(&self) -> &[Arc<dyn Provider>] {
        &self.providers
    }

    pub fn is_enabled(&self, provider: usize) -> bool {
        self.enabled[provider]
    }

    /// Turns provider `provider` on or off. Turning it off drops its catalog;
    /// turning it on leaves the catalog empty until [`Self::set_catalog`].
    pub fn set_enabled(&mut self, provider: usize, enabled: bool) {
        self.enabled[provider] = enabled;
        if !enabled {
            self.set_catalog(provider, Vec::new());
        }
    }

    /// Replaces provider `provider`'s catalog (an index into [`Self::providers`]).
    /// Ignored for a turned-off provider, so a load that finishes after it was
    /// turned off adds nothing. Indices in earlier [`Row::Catalog`]s are
    /// invalid afterwards.
    pub fn set_catalog(&mut self, provider: usize, items: Vec<SearchItem>) {
        self.catalogs[provider] = if self.enabled[provider] {
            items
        } else {
            Vec::new()
        };
        self.catalog = self.catalogs.iter().flatten().cloned().collect();
    }

    fn enabled_providers(&self) -> impl Iterator<Item = &Arc<dyn Provider>> {
        self.providers
            .iter()
            .zip(&self.enabled)
            .filter_map(|(provider, enabled)| enabled.then_some(provider))
    }

    pub fn catalog(&self) -> &[SearchItem] {
        &self.catalog
    }

    /// `frecency` maps an item id to its current (decayed) score; `aliases`
    /// maps item ids to the user's aliases.
    pub fn search(
        &mut self,
        query: &str,
        frecency: impl Fn(&str) -> f64,
        aliases: &HashMap<String, String>,
    ) -> Vec<Row> {
        let query = query.trim();
        let mut rows: Vec<Row> = self
            .enabled_providers()
            .flat_map(|provider| provider.answers(query))
            .map(Row::Computed)
            .collect();
        let answers = rows.len();

        let candidates = self.catalog.iter().map(|item| Candidate {
            title: &item.title,
            keywords: &item.keywords,
            alias: aliases.get(&item.id).map(String::as_str),
            frecency: frecency(&item.id),
        });
        rows.extend(
            self.searcher
                .filter(query, candidates)
                .into_iter()
                .map(Row::Catalog),
        );
        let matches = rows.len() - answers;

        if !query.is_empty() {
            rows.extend(
                self.enabled_providers()
                    .flat_map(|provider| provider.fallbacks(query))
                    .map(Row::Computed),
            );
        }
        self.last_split = Split {
            answers,
            matches,
            fallbacks: rows.len() - answers - matches,
        };
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed {
        catalog: &'static [&'static str],
    }

    impl Provider for Fixed {
        fn id(&self) -> &'static str {
            "fixed"
        }

        fn catalog(&self) -> Vec<SearchItem> {
            self.catalog
                .iter()
                .map(|title| SearchItem::new(format!("fixed:{title}"), *title))
                .collect()
        }

        fn answers(&self, query: &str) -> Vec<SearchItem> {
            if query == "1+1" {
                vec![SearchItem::new("calc:answer".into(), "2")]
            } else {
                Vec::new()
            }
        }

        fn fallbacks(&self, query: &str) -> Vec<SearchItem> {
            vec![SearchItem::new(
                "web:search".into(),
                format!("Search {query}"),
            )]
        }
    }

    fn titles(search: &RootSearch, rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Catalog(ix) => search.catalog()[*ix].title.clone(),
                Row::Computed(item) => item.title.clone(),
            })
            .collect()
    }

    fn search_with(catalogs: &[&'static [&'static str]]) -> RootSearch {
        let providers: Vec<Arc<dyn Provider>> = catalogs
            .iter()
            .map(|&catalog| Arc::new(Fixed { catalog }) as Arc<dyn Provider>)
            .collect();
        let mut search = RootSearch::new(providers.clone());
        for (ix, provider) in providers.iter().enumerate() {
            search.set_catalog(ix, provider.catalog());
        }
        search
    }

    #[test]
    fn answers_first_then_ranked_then_fallbacks() {
        let mut search = search_with(&[&["Calculator", "Paint"]]);

        let rows = search.search("paint", |_| 0., &HashMap::new());
        assert_eq!(titles(&search, &rows), ["Paint", "Search paint"]);

        let rows = search.search("1+1", |_| 0., &HashMap::new());
        assert_eq!(titles(&search, &rows), ["2", "Search 1+1"]);
    }

    #[test]
    fn empty_query_lists_catalog_without_fallbacks() {
        let mut search = search_with(&[&["Calculator", "Paint"]]);
        let rows = search.search("  ", |_| 0., &HashMap::new());
        assert_eq!(titles(&search, &rows), ["Calculator", "Paint"]);
    }

    #[test]
    fn catalog_keeps_provider_order_whatever_loads_first() {
        let providers: Vec<Arc<dyn Provider>> = vec![
            Arc::new(Fixed { catalog: &["Apps"] }),
            Arc::new(Fixed { catalog: &["Lock"] }),
        ];
        let mut search = RootSearch::new(providers.clone());
        search.set_catalog(1, providers[1].catalog());
        search.set_catalog(0, providers[0].catalog());
        let titles: Vec<_> = search.catalog().iter().map(|item| &item.title).collect();
        assert_eq!(titles, ["Apps", "Lock"]);
    }

    #[test]
    fn turned_off_provider_adds_nothing() {
        let providers: Vec<Arc<dyn Provider>> = vec![
            Arc::new(Fixed { catalog: &["Apps"] }),
            Arc::new(Fixed { catalog: &["Lock"] }),
        ];
        let mut search = RootSearch::new(providers.clone());
        search.set_catalog(0, providers[0].catalog());
        search.set_enabled(1, false);
        // A load that was already running when it was turned off.
        search.set_catalog(1, providers[1].catalog());

        let rows = search.search("", |_| 0., &HashMap::new());
        assert_eq!(titles(&search, &rows), ["Apps"]);
        // Answers and fallbacks come from the provider still on, once each.
        let rows = search.search("1+1", |_| 0., &HashMap::new());
        assert_eq!(titles(&search, &rows), ["2", "Search 1+1"]);

        search.set_enabled(0, false);
        assert!(search.search("1+1", |_| 0., &HashMap::new()).is_empty());

        search.set_enabled(1, true);
        search.set_catalog(1, providers[1].catalog());
        let rows = search.search("", |_| 0., &HashMap::new());
        assert_eq!(titles(&search, &rows), ["Lock"]);
    }

    #[test]
    fn frecency_reorders_catalog() {
        let mut search = search_with(&[&["Calculator", "Paint"]]);
        let rows = search.search(
            "",
            |id| if id == "fixed:Paint" { 1. } else { 0. },
            &HashMap::new(),
        );
        assert_eq!(titles(&search, &rows), ["Paint", "Calculator"]);
    }
}
