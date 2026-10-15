//! Paragraph-level LRU cache for check results.

use std::num::NonZeroUsize;

use lru::LruCache;

use crate::router::CheckResult;

/// Maximum number of cached paragraphs.
pub const CAPACITY: usize = 2000;

/// Hit and miss counters. A miss means the engines had to run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
}

pub struct ParagraphCache {
    lru: LruCache<u64, CheckResult>,
    stats: CacheStats,
}

impl ParagraphCache {
    pub fn new() -> Self {
        let cap = NonZeroUsize::new(CAPACITY).expect("capacity is non-zero");
        Self {
            lru: LruCache::new(cap),
            stats: CacheStats::default(),
        }
    }

    /// Returns a clone of the cached result and counts a hit, or counts a miss.
    pub fn get(&mut self, key: u64) -> Option<CheckResult> {
        match self.lru.get(&key) {
            Some(v) => {
                self.stats.hits += 1;
                Some(v.clone())
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    pub fn put(&mut self, key: u64, value: CheckResult) {
        self.lru.put(key, value);
    }

    pub fn stats(&self) -> CacheStats {
        self.stats
    }
}

impl Default for ParagraphCache {
    fn default() -> Self {
        Self::new()
    }
}
