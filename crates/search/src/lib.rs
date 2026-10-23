//! Root search. [`Searcher`] ranks: fuzzy match with `nucleo-matcher` (the
//! matcher used by the Helix editor), plus a bonus for prefixes and for items
//! used often and recently. [`RootSearch`] merges what the providers return.

mod root;

pub use root::{RootSearch, Row};

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

/// Added when the title or a keyword starts with the query (case-insensitive).
const PREFIX_BONUS: f64 = 30.;
/// Points per unit of `ln(1 + frecency)`: one recent use ≈ 10, five ≈ 27, ten ≈ 36.
/// For scale, nucleo gives "calc" 114 on "Calculator" and 98 on "Cool Art Lab
/// Collection", so frecency reorders close matches but not a clearly better one.
const FRECENCY_WEIGHT: f64 = 15.;

/// One rankable item.
#[derive(Clone, Copy, Debug)]
pub struct Candidate<'a> {
    pub title: &'a str,
    /// Matched like the title; the best of all of them counts.
    pub keywords: &'a [String],
    /// Current (decayed) frecency score; 0 for items never used.
    pub frecency: f64,
}

pub struct Searcher {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Default for Searcher {
    fn default() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }
}

impl Searcher {
    /// Returns indices into `candidates`, best first.
    ///
    /// An empty query lists everything: used items first by frecency, the rest
    /// in their original order. Otherwise only matches are returned; ties
    /// prefer shorter titles, then original order.
    pub fn filter<'a>(
        &mut self,
        query: &str,
        candidates: impl IntoIterator<Item = Candidate<'a>>,
    ) -> Vec<usize> {
        let query = query.trim();
        if query.is_empty() {
            let mut all: Vec<(f64, usize)> = candidates
                .into_iter()
                .enumerate()
                .map(|(ix, candidate)| (candidate.frecency, ix))
                .collect();
            // Stable, so unused items keep their original order.
            all.sort_by(|a, b| b.0.total_cmp(&a.0));
            return all.into_iter().map(|(_, ix)| ix).collect();
        }

        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let query_lower = query.to_lowercase();
        let mut scored: Vec<(f64, usize, usize)> = candidates
            .into_iter()
            .enumerate()
            .filter_map(|(ix, candidate)| {
                let title = candidate.title;
                let matched = std::iter::once(title)
                    .chain(candidate.keywords.iter().map(String::as_str))
                    .filter_map(|text| {
                        let fuzzy =
                            pattern.score(Utf32Str::new(text, &mut self.buf), &mut self.matcher)?;
                        let prefix = if text.to_lowercase().starts_with(&query_lower) {
                            PREFIX_BONUS
                        } else {
                            0.
                        };
                        Some(fuzzy as f64 + prefix)
                    })
                    .reduce(f64::max)?;
                let used = FRECENCY_WEIGHT * candidate.frecency.max(0.).ln_1p();
                Some((matched + used, title.len(), ix))
            })
            .collect();
        scored.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        scored.into_iter().map(|(_, _, ix)| ix).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unused<'a>(titles: &'a [&'a str]) -> impl Iterator<Item = Candidate<'a>> {
        titles.iter().map(|title| Candidate {
            title,
            keywords: &[],
            frecency: 0.,
        })
    }

    fn with_frecency<'a>(titles: &'a [(&'a str, f64)]) -> impl Iterator<Item = Candidate<'a>> {
        titles.iter().map(|&(title, frecency)| Candidate {
            title,
            keywords: &[],
            frecency,
        })
    }

    #[test]
    fn ranks_prefix_and_subsequence_matches() {
        let names = [
            "Visual Studio Code",
            "Calculator",
            "Command Prompt",
            "Paint",
        ];
        let mut searcher = Searcher::default();

        assert_eq!(searcher.filter("", unused(&names)), vec![0, 1, 2, 3]);
        assert_eq!(searcher.filter("calc", unused(&names))[0], 1);
        assert_eq!(searcher.filter("vsc", unused(&names))[0], 0);
        assert!(searcher.filter("zzzz", unused(&names)).is_empty());
    }

    #[test]
    fn finds_items_by_keyword() {
        let keywords = ["reboot".to_owned()];
        let items = [
            Candidate {
                title: "Restart",
                keywords: &keywords,
                frecency: 0.,
            },
            Candidate {
                title: "Rebel Galaxy",
                keywords: &[],
                frecency: 0.,
            },
        ];
        let mut searcher = Searcher::default();
        assert_eq!(searcher.filter("reboot", items), vec![0]);
        assert_eq!(searcher.filter("reb", items).len(), 2);
    }

    #[test]
    fn empty_query_lists_used_items_first() {
        let items = [("Alpha", 0.), ("Beta", 0.5), ("Gamma", 0.), ("Delta", 3.)];
        let mut searcher = Searcher::default();
        assert_eq!(searcher.filter("", with_frecency(&items)), vec![3, 1, 0, 2]);
    }

    #[test]
    fn frecency_breaks_close_matches() {
        let mut searcher = Searcher::default();
        let items = [("Code", 0.), ("Codex Notes", 0.)];
        assert_eq!(searcher.filter("cod", with_frecency(&items))[0], 0);

        let items = [("Code", 0.), ("Codex Notes", 2.)];
        assert_eq!(searcher.filter("cod", with_frecency(&items))[0], 1);
    }

    #[test]
    fn frecency_does_not_beat_a_clearly_better_match() {
        let items = [("Calculator", 0.), ("Cool Art Lab Collection", 5.)];
        let mut searcher = Searcher::default();
        assert_eq!(searcher.filter("calc", with_frecency(&items))[0], 0);
    }
}
