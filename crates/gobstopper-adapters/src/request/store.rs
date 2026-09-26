//! Prefix store: chain hash of an original prefix -> its compacted
//! replacement. An entry holds only `(head_len, summary, cut)`, the base
//! threshold it was computed under, and the conversation's carried words
//! for the next compaction; the substitution for a request extending that
//! prefix is `messages[..head_len] + [summary] + messages[cut..]`, with
//! head bytes taken from the current request so volatile fields are never
//! replayed stale. The store is a cache: compaction is deterministic for
//! one base threshold, so an evicted entry is recomputed identically on
//! demand, and an entry from another threshold is never substituted.

use serde_json::Value;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub head_len: usize,
    pub summary: Value,
    /// Length of the original prefix this entry replaces.
    pub cut: usize,
    /// The selected base threshold the entry was computed under
    /// (`RequestCtx::base_threshold_tokens`). Requests under another
    /// threshold skip the entry.
    pub base_threshold_tokens: u64,
    /// The words carried into the next compaction's summary, oldest first
    /// (`CompactResult::carry`). Held in memory only; empty when carrying
    /// is off or a truncated summary dropped part of its text.
    pub carry: Vec<String>,
}

/// LRU cache bounded by entry count and total size: each entry's summary
/// characters plus the characters of its carried parts.
#[derive(Debug)]
pub struct PrefixStore {
    max_entries: usize,
    max_bytes: usize,
    entries: HashMap<String, (Entry, usize)>,
    order: VecDeque<String>,
    bytes: usize,
}

impl Default for PrefixStore {
    fn default() -> Self {
        Self::new(4_096, 64 * 1024 * 1024)
    }
}

impl PrefixStore {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            max_bytes,
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.order.iter().position(|k| k == key) {
            if let Some(key) = self.order.remove(position) {
                self.order.push_back(key);
            }
        }
    }

    pub fn get(&mut self, key: &str) -> Option<Entry> {
        let entry = self.entries.get(key).map(|(entry, _)| entry.clone())?;
        self.touch(key);
        Some(entry)
    }

    pub fn put(&mut self, key: String, entry: Entry) {
        let size = super::json_chars(&entry.summary)
            + entry
                .carry
                .iter()
                .map(|part| part.chars().count())
                .sum::<usize>();
        if let Some((_, old)) = self.entries.remove(&key) {
            self.bytes -= old;
            self.order.retain(|k| *k != key);
        }
        self.bytes += size;
        self.entries.insert(key.clone(), (entry, size));
        self.order.push_back(key);
        // Never evict down to empty: an entry over budget on its own is still
        // the one the next request will look for.
        while self.entries.len() > 1
            && (self.entries.len() > self.max_entries || self.bytes > self.max_bytes)
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((_, size)) = self.entries.remove(&oldest) {
                self.bytes -= size;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(text: &str) -> Entry {
        Entry {
            head_len: 1,
            summary: json!({"role": "user", "content": text}),
            cut: 3,
            base_threshold_tokens: 128_000,
            carry: Vec::new(),
        }
    }

    #[test]
    fn evicts_least_recently_used_within_bounds() {
        let mut store = PrefixStore::new(2, usize::MAX);
        store.put("a".into(), entry("a"));
        store.put("b".into(), entry("b"));
        assert!(store.get("a").is_some());
        store.put("c".into(), entry("c"));
        assert_eq!(store.len(), 2);
        assert!(store.get("b").is_none(), "b was least recently used");
        assert!(store.get("a").is_some() && store.get("c").is_some());
    }

    #[test]
    fn byte_bound_keeps_the_newest_entry_even_when_oversized() {
        let mut store = PrefixStore::new(10, 10);
        store.put("a".into(), entry(&"x".repeat(100)));
        store.put("b".into(), entry(&"y".repeat(100)));
        assert_eq!(store.len(), 1);
        assert!(store.get("b").is_some());
        store.put("b".into(), entry("z"));
        assert_eq!(store.len(), 1);
        assert_eq!(store.bytes(), json_size("z"));
    }

    #[test]
    fn an_entrys_size_counts_its_carried_characters() {
        let carried = |text: &str, carry: &[&str]| Entry {
            carry: carry.iter().map(|part| part.to_string()).collect(),
            ..entry(text)
        };
        let mut store = PrefixStore::new(10, usize::MAX);
        // Characters, not bytes: "é" counts once.
        store.put("a".into(), carried("s", &["user: abc", "assistant: dé"]));
        assert_eq!(store.bytes(), json_size("s") + 9 + 13);
        // Replacing the entry replaces its size.
        store.put("a".into(), carried("s", &[]));
        assert_eq!(store.bytes(), json_size("s"));

        // The byte bound sees the carry: two entries whose summaries fit
        // together but whose carries do not keep only the newest.
        let bound = 2 * json_size("s") + 50;
        let mut store = PrefixStore::new(10, bound);
        store.put("a".into(), carried("s", &["x"]));
        store.put("b".into(), carried("s", &["y"]));
        assert_eq!(store.len(), 2);
        store.put("c".into(), carried("s", &[&"z".repeat(60)]));
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get("c").map(|entry| entry.carry),
            Some(vec!["z".repeat(60)])
        );
        assert_eq!(store.bytes(), json_size("s") + 60);
    }

    fn json_size(text: &str) -> usize {
        super::super::json_chars(&json!({"role": "user", "content": text}))
    }
}
