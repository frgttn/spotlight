//! Ranked search over an [`Index`].
//!
//! 1. Each query word is matched once against the unique names, not every
//!    file: `neo_frizbee` in parallel, reading the name arena in place, or the
//!    fst's prefix lookup for one- and two-character words. The result is a
//!    score per name.
//! 2. When a word may match a folder instead (several words, or `in:`), the
//!    names are also matched as substrings, and each folder gets a mask of
//!    the words matched by it or a folder above it. The masks are computed
//!    once per folder, so this stays linear.
//! 3. One pass over the entry table combines the two, applies filters and
//!    keeps the best few hundred; those are re-ranked with checks too slow
//!    for every file (exact name without extension) and cut to the limit.

use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use neo_frizbee::{Config, Matcher, Matching, Pattern, PatternConfig, UnicodeMatching};

use crate::{
    index::{Entry, EntryId, Index, NO_PARENT, flag, from_mtime},
    kind::{ExtId, Kind, stem},
    names::{MAX_CHUNKS, NameId, fold},
    query::{MAX_WORDS, Query},
};

/// Credit per character for a word matched by a folder above the file rather
/// than by its name. Below `neo_frizbee`'s 12 per matched character plus
/// bonuses, so a clean match in the name still ranks higher, but above a match
/// scattered through the name ("rea" in "03-rust-native-plan").
const FOLDER_WORD_POINTS: i32 = 10;
/// `neo_frizbee`'s defaults, for one- and two-letter words matched through
/// the fst instead: per matched character, for matching the first character,
/// for matching the whole name.
const PREFIX_POINTS_PER_CHAR: u16 = 12;
const PREFIX_BONUS: u16 = 12;
const EXACT_BONUS: u16 = 8;
/// Lost per folder between the scope root and the file.
const DEPTH_PENALTY: i32 = 2;
const HIDDEN_PENALTY: i32 = 15;
/// For extensions in [`Kind::is_noise`]: logs, caches, build output.
const NOISE_PENALTY: i32 = 20;
/// Added when the name without its extension equals the query.
const STEM_BONUS: i32 = 30;
/// Points per unit of `ln(1 + frecency)`, as in root search.
const FRECENCY_WEIGHT: f64 = 15.;
/// Bonus for a file modified within a day, a week, a month.
const RECENT_BONUS: [(i64, i32); 3] = [(86_400, 8), (7 * 86_400, 5), (30 * 86_400, 2)];
/// Candidates kept for re-ranking, per result asked for.
const POOL_FACTOR: usize = 4;
/// Below this many entries, one thread is faster than starting more.
const PARALLEL_MIN: usize = 100_000;
/// Entries between checks of the cancel flag.
const CANCEL_STRIDE: usize = 16_384;

/// Mask bits: words 0..16, `in:` folders 16..24, depth 24..30, hidden 30,
/// deleted 31.
const FOLDER_BITS: u32 = MAX_WORDS as u32;
const DEPTH_SHIFT: u32 = 24;
const DEPTH_MAX: u32 = 0x3f;
const HIDDEN: u32 = 1 << 30;
const DEAD: u32 = 1 << 31;
const UNSET: u32 = u32::MAX;

pub struct SearchOptions<'a> {
    pub limit: usize,
    /// Unix seconds, for the recency bonus.
    pub now: i64,
    /// Include hidden and system files (ranked lower).
    pub include_hidden: bool,
    /// The entry's current frecency, e.g. from opening it in Spotlight.
    pub frecency: Option<&'a (dyn Fn(EntryId) -> f64 + Sync)>,
    /// Set by the caller when a newer query makes this one moot.
    pub cancel: Option<&'a AtomicBool>,
}

impl Default for SearchOptions<'_> {
    fn default() -> Self {
        Self {
            limit: 50,
            now: 0,
            include_hidden: false,
            frecency: None,
            cancel: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub id: EntryId,
    pub score: i32,
}

/// What every entry is checked against, built once per query.
struct Plan<'a> {
    /// Per word, the score of each name; 0 for no match.
    names: Vec<Vec<u16>>,
    /// Per word, its character count, for the folder credit.
    word_chars: Vec<i32>,
    /// Names excluded by `!word`.
    excluded: Option<Bits>,
    exts: Vec<ExtId>,
    kind: Option<Kind>,
    /// Bits every candidate's folders must have (the `in:` filters).
    folders_required: u32,
    /// Per entry, for folders only: see the mask bits above.
    masks: Vec<u32>,
    options: &'a SearchOptions<'a>,
}

impl Index {
    /// The best matches, best first. `None` when cancelled.
    pub fn search(&self, query: &Query, options: &SearchOptions) -> Option<Vec<Hit>> {
        if query.is_empty() || options.limit == 0 {
            return Some(Vec::new());
        }
        let plan = self.plan(query, options)?;
        let pool = options.limit * POOL_FACTOR;
        let mut hits = self.scan(&plan, pool)?;
        self.rerank(query, &mut hits);
        hits.truncate(options.limit);
        Some(hits)
    }

    fn plan<'a>(&self, query: &Query, options: &'a SearchOptions<'a>) -> Option<Plan<'a>> {
        let mut exts = Vec::new();
        let mut unknown_ext = false;
        for ext in &query.exts {
            match self.exts.find(ext) {
                Some(id) => exts.push(id),
                None => unknown_ext = true,
            }
        }
        let mut words = Vec::new();
        for word in &query.words {
            match word
                .maybe_ext
                .then(|| self.exts.find(&word.text[1..]))
                .flatten()
            {
                Some(id) => exts.push(id),
                None => words.push(word),
            }
        }
        if unknown_ext && exts.is_empty() {
            // Only extensions no file has: nothing can match.
            return Some(self.empty_plan(options));
        }

        let cancelled = || {
            options
                .cancel
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
        };
        // With several words, each must appear unbroken, in the name or in a
        // folder above it: scattered letters of two words in one long name
        // ("rea plan" in "reward_table_items_player…") are noise. One word
        // keeps fuzzy matching with typos.
        let several = words.len() > 1;
        let mut names = Vec::with_capacity(words.len());
        for word in &words {
            let matching = match word.matching {
                Matching::Fuzzy if several => Matching::Substring,
                matching => matching,
            };
            names.push(self.score_names(&word.text, matching));
            if cancelled() {
                return None;
            }
        }

        // Folder matching, only when a word may sit in a folder name: the
        // same unbroken matches, as bits.
        let mut folder_words: Vec<Bits> = Vec::new();
        if several {
            for scores in &names {
                let mut bits = Bits::new(self.names.len());
                for (id, score) in scores.iter().enumerate() {
                    if *score > 0 {
                        bits.set(id as NameId);
                    }
                }
                folder_words.push(bits);
            }
        }
        let mut folder_filters: Vec<Bits> = Vec::new();
        for folder in &query.folders {
            folder_filters.push(self.substring_names(folder));
        }
        let excluded = (!query.excluded.is_empty()).then(|| {
            let mut bits = Bits::new(self.names.len());
            for word in &query.excluded {
                bits.or(&self.substring_names(word));
            }
            bits
        });
        if cancelled() {
            return None;
        }

        let folders_required = ((1u32 << folder_filters.len()) - 1) << FOLDER_BITS;
        let masks = self.folder_masks(&folder_words, &folder_filters);
        Some(Plan {
            word_chars: words
                .iter()
                .map(|word| word.text.chars().count() as i32)
                .collect(),
            names,
            excluded,
            exts,
            kind: query.kind,
            folders_required,
            masks,
            options,
        })
    }

    fn empty_plan<'a>(&self, options: &'a SearchOptions<'a>) -> Plan<'a> {
        Plan {
            names: vec![vec![0; self.names.len()]],
            word_chars: vec![0],
            excluded: None,
            exts: Vec::new(),
            kind: None,
            folders_required: 0,
            masks: Vec::new(),
            options,
        }
    }

    /// The score of each name for one word.
    fn score_names(&self, word: &str, matching: Matching) -> Vec<u16> {
        let mut scores = vec![0u16; self.names.len()];
        let chars = word.chars().count();
        let config = Config::default()
            .casing(neo_frizbee::CaseMatching::Ignore)
            .unicode(UnicodeMatching::Smart)
            .sort(neo_frizbee::SortStrategy::Unsorted);
        // One missing letter from 5 characters, two from 9, three from 13:
        // with one in four letters, "plan" would match "launcher".
        let typos = if matching == Matching::Fuzzy {
            (chars.saturating_sub(1) / 4).min(3) as u16
        } else {
            0
        };
        let pattern = Pattern::new(
            word,
            PatternConfig::default()
                .matching(Some(matching))
                .max_typos(Some(typos)),
        );
        let mut matcher = Matcher::new(pattern, &config);

        if matching == Matching::Fuzzy && chars <= 2 {
            // Fuzzy one- and two-letter words match nearly everything: take
            // names that start with them, through the fst, scored like
            // `neo_frizbee` scores a prefix match, shorter names first.
            let prefix = PREFIX_POINTS_PER_CHAR * chars as u16 + PREFIX_BONUS;
            self.names.for_each_prefixed(&fold(word), |id| {
                let len = self.names.byte_len(id);
                let exact = if len == word.len() { EXACT_BONUS } else { 0 };
                scores[id as usize] =
                    (prefix + exact).saturating_sub((len / 4).min(prefix as usize - 1) as u16);
            });
            return scores;
        }

        let resolve = |id: u32, chunks: &mut [*const u8; MAX_CHUNKS]| self.names.chunks(id, chunks);
        for found in matcher.match_range_parallel_resolved(self.names.len(), &resolve, 0) {
            scores[found.index as usize] = found.score.max(1);
        }
        scores
    }

    /// Names that contain `word`, ignoring case.
    fn substring_names(&self, word: &str) -> Bits {
        let mut bits = Bits::new(self.names.len());
        let config = Config::default()
            .casing(neo_frizbee::CaseMatching::Ignore)
            .matching(Matching::Substring)
            .sort(neo_frizbee::SortStrategy::Unsorted);
        let mut matcher = Matcher::new(word, &config);
        let resolve = |id: u32, chunks: &mut [*const u8; MAX_CHUNKS]| self.names.chunks(id, chunks);
        for found in matcher.match_range_parallel_resolved(self.names.len(), &resolve, 0) {
            bits.set(found.index);
        }
        bits
    }

    /// Per folder: the words (and `in:` filters) its name or a folder above it
    /// matches, its depth below the scope root, and whether it or a folder
    /// above it is hidden or deleted. Scope roots take no words: their name
    /// is a path.
    fn folder_masks(&self, words: &[Bits], filters: &[Bits]) -> Vec<u32> {
        let mut masks = vec![UNSET; self.entries.len()];
        let own = |entry: &Entry| -> u32 {
            if entry.has(flag::DELETED) {
                return DEAD;
            }
            if entry.has(flag::ROOT) {
                return 0;
            }
            let mut mask = if entry.flags & (flag::HIDDEN | flag::SYSTEM) != 0 {
                HIDDEN
            } else {
                0
            };
            for (bit, bits) in words.iter().enumerate() {
                if bits.get(entry.name) {
                    mask |= 1 << bit;
                }
            }
            for (bit, bits) in filters.iter().enumerate() {
                if bits.get(entry.name) {
                    mask |= 1 << (FOLDER_BITS + bit as u32);
                }
            }
            mask
        };
        let mut chain = Vec::new();
        for id in 0..self.entries.len() {
            if masks[id] != UNSET || !self.entries[id].has(flag::DIR) {
                continue;
            }
            chain.clear();
            let mut at = id as EntryId;
            while at != NO_PARENT && masks[at as usize] == UNSET {
                chain.push(at);
                at = self.entries[at as usize].parent;
            }
            let mut above = if at == NO_PARENT {
                None
            } else {
                Some(masks[at as usize])
            };
            for &id in chain.iter().rev() {
                let entry = &self.entries[id as usize];
                let mask = match above {
                    // A scope root: depth 0.
                    None => own(entry),
                    Some(parent) => {
                        let depth = ((parent >> DEPTH_SHIFT) & DEPTH_MAX)
                            .saturating_add(1)
                            .min(DEPTH_MAX);
                        (parent & !(DEPTH_MAX << DEPTH_SHIFT)) | own(entry) | (depth << DEPTH_SHIFT)
                    }
                };
                masks[id as usize] = mask;
                above = Some(mask);
            }
        }
        masks
    }

    /// The best `pool` entries by score, best first.
    fn scan(&self, plan: &Plan, pool: usize) -> Option<Vec<Hit>> {
        let len = self.entries.len();
        let threads = if len < PARALLEL_MIN {
            1
        } else {
            thread::available_parallelism()
                .map_or(1, |n| n.get())
                .clamp(1, 8)
        };
        let per_thread = len.div_ceil(threads);
        let heaps: Vec<Option<Best>> = thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|ix| {
                    let start = ix * per_thread;
                    let end = (start + per_thread).min(len);
                    scope.spawn(move || self.scan_range(plan, start..end, pool))
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("scan thread"))
                .collect()
        });
        let mut all: Vec<Hit> = Vec::new();
        for heap in heaps {
            all.extend(
                heap?
                    .into_iter()
                    .map(|Reverse((score, Reverse(id)))| Hit { id, score }),
            );
        }
        all.sort_unstable_by(|a, b| b.score.cmp(&a.score).then(a.id.cmp(&b.id)));
        all.truncate(pool);
        Some(all)
    }

    fn scan_range(&self, plan: &Plan, range: std::ops::Range<usize>, pool: usize) -> Option<Best> {
        let mut heap = BinaryHeap::with_capacity(pool + 1);
        for (offset, entry) in self.entries[range.clone()].iter().enumerate() {
            if offset % CANCEL_STRIDE == 0
                && plan
                    .options
                    .cancel
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                return None;
            }
            let id = (range.start + offset) as EntryId;
            let Some(score) = self.score_entry(plan, id, entry) else {
                continue;
            };
            if heap.len() < pool {
                heap.push(Reverse((score, Reverse(id))));
            } else if let Some(Reverse((worst, _))) = heap.peek()
                && score > *worst
            {
                heap.pop();
                heap.push(Reverse((score, Reverse(id))));
            }
        }
        Some(heap)
    }

    fn score_entry(&self, plan: &Plan, id: EntryId, entry: &Entry) -> Option<i32> {
        if entry.flags & (flag::DELETED | flag::ROOT) != 0 {
            return None;
        }
        let above = match entry.parent {
            NO_PARENT => 0,
            parent => plan.masks.get(parent as usize).copied().unwrap_or(0),
        };
        if above & DEAD != 0 || above & plan.folders_required != plan.folders_required {
            return None;
        }
        // Inside a hidden folder (AppData) counts as hidden too.
        let hidden = entry.flags & (flag::HIDDEN | flag::SYSTEM) != 0 || above & HIDDEN != 0;
        if hidden && !plan.options.include_hidden {
            return None;
        }
        if !plan.exts.is_empty() && !plan.exts.contains(&entry.ext) {
            return None;
        }
        if let Some(kind) = plan.kind {
            let dir = entry.has(flag::DIR);
            let matches = match kind {
                Kind::Folder => dir,
                kind => !dir && self.exts.kind(entry.ext) == kind,
            };
            if !matches {
                return None;
            }
        }
        if let Some(excluded) = &plan.excluded
            && excluded.get(entry.name)
        {
            return None;
        }

        let mut score = 0i32;
        if plan.names.is_empty() {
            // Filters only: newest first. Minutes since 2000 fit an i32.
            score = entry.mtime.min(i32::MAX as u32) as i32;
        } else {
            let mut in_name = false;
            for (word, scores) in plan.names.iter().enumerate() {
                let name = scores[entry.name as usize] as i32;
                let folder = if above & (1 << word) != 0 {
                    FOLDER_WORD_POINTS * plan.word_chars[word]
                } else {
                    0
                };
                if name == 0 && folder == 0 {
                    return None;
                }
                if name >= folder {
                    in_name = true;
                    score += name;
                } else {
                    score += folder;
                }
            }
            if !in_name {
                return None;
            }
            let depth = (above >> DEPTH_SHIFT) & DEPTH_MAX;
            score -= DEPTH_PENALTY * depth as i32;
            if hidden {
                score -= HIDDEN_PENALTY;
            }
            if self.exts.is_noise(entry.ext) {
                score -= NOISE_PENALTY;
            }
            if entry.mtime != 0 && plan.options.now > 0 {
                let age = plan.options.now - from_mtime(entry.mtime);
                if let Some((_, bonus)) = RECENT_BONUS.iter().find(|(within, _)| age < *within) {
                    score += bonus;
                }
            }
        }
        if let Some(frecency) = plan.options.frecency {
            score += (frecency(id).max(0.).ln_1p() * FRECENCY_WEIGHT) as i32;
        }
        Some(score)
    }

    /// Re-ranks the pool with what is too slow for every entry: the name
    /// without its extension equal to the query. Ties go to shorter names.
    fn rerank(&self, query: &Query, hits: &mut [Hit]) {
        if !query.words.is_empty() {
            let typed = fold(
                &query
                    .words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            for hit in hits.iter_mut() {
                if fold(stem(self.name(hit.id))) == typed {
                    hit.score += STEM_BONUS;
                }
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| self.name(a.id).len().cmp(&self.name(b.id).len()))
                .then(a.id.cmp(&b.id))
        });
    }
}

/// The best candidates so far, worst on top, so it is the one replaced.
/// Ties on score keep the lower id.
type Best = BinaryHeap<Reverse<(i32, Reverse<EntryId>)>>;

/// A bit per name.
struct Bits(Vec<u64>);

impl Bits {
    fn new(len: usize) -> Self {
        Self(vec![0; len.div_ceil(64)])
    }

    fn set(&mut self, id: NameId) {
        self.0[id as usize / 64] |= 1 << (id % 64);
    }

    fn get(&self, id: NameId) -> bool {
        self.0[id as usize / 64] & (1 << (id % 64)) != 0
    }

    fn or(&mut self, other: &Bits) {
        for (word, other) in self.0.iter_mut().zip(&other.0) {
            *word |= other;
        }
    }
}
