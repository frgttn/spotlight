//! Fuzzy filtering with `nucleo-matcher` (the matcher used by the Helix editor).

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

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
    /// Returns indices into `haystacks`, best match first. An empty query keeps
    /// the original order. Ties prefer shorter titles, then original order.
    pub fn filter<'a>(
        &mut self,
        query: &str,
        haystacks: impl IntoIterator<Item = &'a str>,
    ) -> Vec<usize> {
        let query = query.trim();
        if query.is_empty() {
            return haystacks
                .into_iter()
                .enumerate()
                .map(|(ix, _)| ix)
                .collect();
        }

        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, usize, usize)> = haystacks
            .into_iter()
            .enumerate()
            .filter_map(|(ix, text)| {
                let score = pattern.score(Utf32Str::new(text, &mut self.buf), &mut self.matcher)?;
                Some((score, text.len(), ix))
            })
            .collect();
        scored.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        scored.into_iter().map(|(_, _, ix)| ix).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_prefix_and_subsequence_matches() {
        let names = [
            "Visual Studio Code",
            "Calculator",
            "Command Prompt",
            "Paint",
        ];
        let mut searcher = Searcher::default();

        assert_eq!(searcher.filter("", names), vec![0, 1, 2, 3]);
        assert_eq!(searcher.filter("calc", names)[0], 1);
        assert_eq!(searcher.filter("vsc", names)[0], 0);
        assert!(searcher.filter("zzzz", names).is_empty());
    }
}
