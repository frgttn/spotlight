//! Unique file names. Many files share a name (`index.js`, `README.md`,
//! `desktop.ini`), so entries store a [`NameId`] and the matcher runs over
//! unique names rather than over every file.
//!
//! Names live back to back in one arena that `neo_frizbee`'s resolved
//! matching reads in place, with no copy per query: it reads whole
//! [`SIMD_CHUNK_BYTES`] chunks and keeps only a name's own bytes, so the arena
//! needs just one spare chunk at its end.
//!
//! Ids follow case-folded order, and an [`fst::Map`] maps each folded form to
//! its first id: a prefix is then one run of ids, found with two lookups, and
//! the fst costs about 8 bytes per name. Names added later (files created
//! since) go to a small hash map until the next [`crate::Index::compact`].

use std::collections::HashMap;

use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use neo_frizbee::SIMD_CHUNK_BYTES;

pub type NameId = u32;

/// Longest name prefix the matcher reads, in bytes. An NTFS name is at most 255
/// UTF-16 units, which is at most 765 bytes of UTF-8, so this covers them all.
pub(crate) const MAX_MATCH_BYTES: usize = 768;
/// Chunk pointers needed for [`MAX_MATCH_BYTES`].
pub(crate) const MAX_CHUNKS: usize = MAX_MATCH_BYTES / SIMD_CHUNK_BYTES;

/// Separates the folded name from the exact name in a [`sort_key`].
const KEY_SEPARATOR: u8 = 0;

pub struct Names {
    arena: Vec<u8>,
    /// Where each name starts in the arena, plus where the next one would:
    /// names are back to back, so a name ends where the next one starts.
    starts: Vec<u32>,
    /// The names known at build time: their case-folded forms, each mapped to
    /// the first id with that form. Their ids come first and follow
    /// [`sort_key`] order, so names that differ only by case are neighbours.
    base: Map<Vec<u8>>,
    base_len: u32,
    /// Names first seen after the fst was built.
    added: HashMap<Box<str>, NameId>,
}

impl Default for Names {
    fn default() -> Self {
        Self::build(&[]).0
    }
}

impl Names {
    /// Builds the table from distinct names. Returns it and the id given to
    /// each input name, in input order. Ids follow fst key order.
    pub fn build(unique: &[&str]) -> (Self, Vec<NameId>) {
        let mut keyed: Vec<(Vec<u8>, u32)> = unique
            .iter()
            .enumerate()
            .map(|(ix, name)| (sort_key(name), ix as u32))
            .collect();
        keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let mut names = Self {
            arena: Vec::with_capacity(
                unique.iter().map(|name| name.len()).sum::<usize>() + SIMD_CHUNK_BYTES,
            ),
            starts: Vec::with_capacity(unique.len() + 1),
            base: Map::default(),
            base_len: unique.len() as u32,
            added: HashMap::new(),
        };
        let mut ids = vec![0; unique.len()];
        let mut builder = MapBuilder::memory();
        let mut previous: Option<&[u8]> = None;
        for (id, (key, input)) in keyed.iter().enumerate() {
            names.push(unique[*input as usize]);
            ids[*input as usize] = id as NameId;
            let folded = folded_part(key);
            if previous != Some(folded) {
                builder
                    .insert(folded, id as u64)
                    .expect("folded forms come sorted, each once");
                previous = Some(folded);
            }
        }
        let bytes = builder.into_inner().expect("fst builds in memory");
        names.base = Map::new(bytes).expect("fst bytes come from MapBuilder");
        (names, ids)
    }

    pub fn len(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    /// Names added since the table was built.
    pub fn added_len(&self) -> usize {
        self.added.len()
    }

    pub fn get(&self, id: NameId) -> &str {
        let bytes =
            &self.arena[self.starts[id as usize] as usize..self.starts[id as usize + 1] as usize];
        // Only whole `str`s are ever pushed.
        std::str::from_utf8(bytes).expect("names are UTF-8")
    }

    /// The name's length in bytes, without decoding it.
    pub fn byte_len(&self, id: NameId) -> usize {
        (self.starts[id as usize + 1] - self.starts[id as usize]) as usize
    }

    pub fn find(&self, name: &str) -> Option<NameId> {
        let folded = fold(name);
        if let Some(first) = self.base.get(&folded) {
            // The names with this folded form follow one another.
            for id in first as NameId..self.base_len {
                let candidate = self.get(id);
                if candidate == name {
                    return Some(id);
                }
                if fold(candidate) != folded {
                    break;
                }
            }
        }
        self.added.get(name).copied()
    }

    /// The id of `name`, adding it if it is new.
    pub fn intern(&mut self, name: &str) -> NameId {
        if let Some(id) = self.find(name) {
            return id;
        }
        let id = self.push(name);
        self.added.insert(name.into(), id);
        id
    }

    /// Calls `f` with every name whose case-folded form starts with `folded`
    /// (which must already be folded with [`fold`]).
    pub fn for_each_prefixed(&self, folded: &str, mut f: impl FnMut(NameId)) {
        // Ids follow key order, so the names known to the fst that start with
        // `folded` are one run of ids: find its two ends. No key contains
        // 0xFF (it never occurs in UTF-8), so `folded` + 0xFF is past them all.
        let start = self.first_id_at_least(folded.as_bytes());
        let mut past = folded.as_bytes().to_vec();
        past.push(0xFF);
        let end = self.first_id_at_least(&past);
        (start..end).for_each(&mut f);
        for (name, id) in &self.added {
            if fold(name).starts_with(folded) {
                f(*id);
            }
        }
    }

    fn first_id_at_least(&self, folded: &[u8]) -> NameId {
        self.base
            .range()
            .ge(folded)
            .into_stream()
            .next()
            .map_or(self.base_len, |(_, id)| id as NameId)
    }

    /// Fills `chunks` with pointers to the name's [`SIMD_CHUNK_BYTES`]-wide
    /// chunks in the arena, as `neo_frizbee`'s resolved matching expects.
    /// Names longer than [`MAX_MATCH_BYTES`] are matched on their start.
    pub(crate) fn chunks(
        &self,
        id: NameId,
        chunks: &mut [*const u8; MAX_CHUNKS],
    ) -> Option<(usize, u16)> {
        let mut len = self.byte_len(id);
        if len == 0 {
            return None;
        }
        if len > MAX_MATCH_BYTES {
            let name = self.get(id);
            len = MAX_MATCH_BYTES;
            while !name.is_char_boundary(len) {
                len -= 1;
            }
        }
        let count = len.div_ceil(SIMD_CHUNK_BYTES);
        // The last chunk may run past the name; `push` keeps a spare chunk at
        // the end of the arena, so every pointer is readable for a full chunk.
        let start = self.arena[self.starts[id as usize] as usize..].as_ptr();
        for (ix, chunk) in chunks.iter_mut().take(count).enumerate() {
            *chunk = start.wrapping_add(ix * SIMD_CHUNK_BYTES);
        }
        Some((count, len as u16))
    }

    pub fn memory_bytes(&self) -> usize {
        self.arena.capacity()
            + self.starts.capacity() * size_of::<u32>()
            + self.base.as_fst().as_bytes().len()
            + self.added.capacity() * (size_of::<Box<str>>() + size_of::<NameId>())
            + self.added.keys().map(|name| name.len()).sum::<usize>()
    }

    fn push(&mut self, name: &str) -> NameId {
        let id = self.len() as NameId;
        let start = self.arena.len().saturating_sub(SIMD_CHUNK_BYTES);
        if self.starts.is_empty() {
            self.starts.push(0);
        }
        self.arena.truncate(start);
        self.arena.extend_from_slice(name.as_bytes());
        let end = u32::try_from(self.arena.len()).expect("names fit in 4 GB");
        self.arena.extend_from_slice(&[0; SIMD_CHUNK_BYTES]);
        self.starts.push(end);
        id
    }
}

/// Case folding for prefix lookups and exact-name checks.
pub fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// Orders names by folded form, then exactly, so names that differ only by
/// case sort next to each other.
fn sort_key(name: &str) -> Vec<u8> {
    let mut key = fold(name).into_bytes();
    key.push(KEY_SEPARATOR);
    key.extend_from_slice(name.as_bytes());
    key
}

fn folded_part(key: &[u8]) -> &[u8] {
    let end = key
        .iter()
        .position(|&byte| byte == KEY_SEPARATOR)
        .unwrap_or(key.len());
    &key[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_follow_key_order() {
        let (names, ids) = Names::build(&["b.txt", "A.txt", "a.txt"]);
        assert_eq!(names.len(), 3);
        for (input, id) in ["b.txt", "A.txt", "a.txt"].iter().zip(&ids) {
            assert_eq!(names.get(*id), *input);
            assert_eq!(names.find(input), Some(*id));
        }
        // Folded "a.txt" sorts before "b.txt"; "A" (0x41) before "a" (0x61).
        assert_eq!(names.get(0), "A.txt");
        assert_eq!(names.get(1), "a.txt");
        assert_eq!(names.get(2), "b.txt");
    }

    #[test]
    fn interns_new_names_once() {
        let (mut names, _) = Names::build(&["one"]);
        let two = names.intern("two");
        assert_eq!(names.intern("two"), two);
        assert_eq!(names.intern("one"), 0);
        assert_eq!(names.added_len(), 1);
        assert_eq!(names.get(two), "two");
    }

    #[test]
    fn prefix_lookup_covers_base_and_added_names() {
        let (mut names, _) = Names::build(&["Report.pdf", "readme.md", "notes.txt", "Отчёт.docx"]);
        names.intern("REsume.pdf");
        let mut found: Vec<&str> = Vec::new();
        names.for_each_prefixed("re", |id| found.push(names.get(id)));
        found.sort_unstable();
        assert_eq!(found, ["REsume.pdf", "Report.pdf", "readme.md"]);

        let mut cyrillic = Vec::new();
        names.for_each_prefixed(&fold("ОТ"), |id| cyrillic.push(names.get(id)));
        assert_eq!(cyrillic, ["Отчёт.docx"]);
    }

    #[test]
    fn chunks_stay_inside_the_arena() {
        let long = "x".repeat(40);
        let (names, ids) = Names::build(&["a", &long]);
        let mut chunks = [std::ptr::null(); MAX_CHUNKS];
        assert_eq!(names.chunks(ids[0], &mut chunks), Some((1, 1)));
        assert_eq!(names.chunks(ids[1], &mut chunks), Some((3, 40)));
        assert_eq!(chunks[1] as usize - chunks[0] as usize, SIMD_CHUNK_BYTES);
        let end = names.arena.as_ptr() as usize + names.arena.len();
        assert!(chunks[2] as usize + SIMD_CHUNK_BYTES <= end);
    }

    #[test]
    fn overlong_names_are_cut_on_a_char_boundary() {
        let long = "ж".repeat(500); // 1000 bytes
        let (names, ids) = Names::build(&[&long]);
        let mut chunks = [std::ptr::null(); MAX_CHUNKS];
        let (count, len) = names.chunks(ids[0], &mut chunks).unwrap();
        assert_eq!(len as usize, MAX_MATCH_BYTES);
        assert_eq!(count, MAX_CHUNKS);
    }
}
