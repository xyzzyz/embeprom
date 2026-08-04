//! A monotonically-increasing counter.

use portable_atomic::{AtomicU64, Ordering};

/// A Prometheus counter: a cumulative value that only increases (or resets to
/// zero, e.g. on restart).
#[derive(Debug, Default)]
pub struct Counter {
    v: AtomicU64,
}

impl Counter {
    /// Create a new counter starting at zero.
    pub const fn new() -> Self {
        Self { v: AtomicU64::new(0) }
    }

    /// Increment by 1.
    #[inline]
    pub fn inc(&self) {
        self.inc_by(1)
    }

    /// Increment by `n`.
    #[inline]
    pub fn inc_by(&self, n: u64) {
        self.v.fetch_add(n, Ordering::Relaxed);
    }

    /// The current value.
    #[inline]
    pub fn get(&self) -> u64 {
        self.v.load(Ordering::Relaxed)
    }

    /// Reset to zero. Intended for tests/bring-up: a counter that decreases
    /// in a live scrape target violates Prometheus counter semantics.
    #[inline]
    pub fn reset(&self) {
        self.v.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increments() {
        let c = Counter::new();
        assert_eq!(c.get(), 0);
        c.inc();
        c.inc_by(41);
        assert_eq!(c.get(), 42);
    }

    #[test]
    fn resets() {
        let c = Counter::new();
        c.inc_by(10);
        c.reset();
        assert_eq!(c.get(), 0);
    }
}
