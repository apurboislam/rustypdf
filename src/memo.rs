//! Bounds Typst's memoization cache (`comemo`).
//!
//! comemo keeps the results of every memoized call (parsing, evaluation,
//! layout, shaping) in global caches until `comemo::evict` drops them. Parts
//! that repeat across requests (a template's imports, its fixed text) hit the
//! cache, but most entries of a render belong to that request's data and are
//! never used again: about 1.7 MB per sector invoice. Evicting on a timer
//! makes memory grow with throughput, so eviction runs every `every` renders
//! instead. See docs/performance.md for the measurements behind the defaults.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Default for `MEMO_EVICT_EVERY`: evict after this many renders.
pub const DEFAULT_EVICT_EVERY: u64 = 16;
/// Entries unused for this many evictions in a row are dropped. 1 keeps
/// anything used during the last `every` to `2 * every` renders.
pub const EVICT_MAX_AGE: usize = 1;
/// When idle, evict this often, so memory from the last burst is released.
pub const IDLE_EVICT_INTERVAL: Duration = Duration::from_secs(30);

pub struct MemoEvictor {
    every: u64,
    renders: AtomicU64,
}

impl MemoEvictor {
    /// Evicts after every `every` renders (at least 1).
    pub fn new(every: u64) -> Self {
        Self {
            every: every.max(1),
            renders: AtomicU64::new(0),
        }
    }

    /// Call after each render (successful or not). Returns whether this call
    /// evicted.
    pub fn after_render(&self) -> bool {
        let n = self.renders.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(self.every) {
            typst::comemo::evict(EVICT_MAX_AGE);
            true
        } else {
            false
        }
    }

    /// Timer fallback: evicts only if no render happened since the last
    /// call, so an idle service releases memory without aging the cache
    /// while busy.
    pub fn idle_tick(&self, renders_at_last_tick: &mut u64) -> bool {
        let now = self.renders.load(Ordering::Relaxed);
        let idle = now == *renders_at_last_tick;
        *renders_at_last_tick = now;
        if idle {
            typst::comemo::evict(EVICT_MAX_AGE);
        }
        idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_every_n_renders() {
        let e = MemoEvictor::new(3);
        let evicted: Vec<bool> = (0..7).map(|_| e.after_render()).collect();
        assert_eq!(evicted, [false, false, true, false, false, true, false]);
    }

    #[test]
    fn idle_tick_skips_while_busy() {
        let e = MemoEvictor::new(100);
        let mut last = 0;
        assert!(e.idle_tick(&mut last));
        e.after_render();
        assert!(!e.idle_tick(&mut last));
        assert!(e.idle_tick(&mut last));
    }

    #[test]
    fn zero_means_every_render() {
        assert!(MemoEvictor::new(0).after_render());
    }
}
