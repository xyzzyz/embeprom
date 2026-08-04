//! A value that can go up or down.

use portable_atomic::{AtomicI64, Ordering};

/// A Prometheus gauge holding a signed integer value.
#[derive(Debug, Default)]
pub struct Gauge {
    v: AtomicI64,
}

impl Gauge {
    /// Create a new gauge with the given initial value.
    pub const fn new(initial: i64) -> Self {
        Self { v: AtomicI64::new(initial) }
    }

    /// Set the value.
    #[inline]
    pub fn set(&self, v: i64) {
        self.v.store(v, Ordering::Relaxed);
    }

    /// Add `d` to the value (`d` may be negative).
    #[inline]
    pub fn add(&self, d: i64) {
        self.v.fetch_add(d, Ordering::Relaxed);
    }

    /// Subtract `d` from the value.
    #[inline]
    pub fn sub(&self, d: i64) {
        self.v.fetch_sub(d, Ordering::Relaxed);
    }

    /// Increment by 1.
    #[inline]
    pub fn inc(&self) {
        self.add(1);
    }

    /// Decrement by 1.
    #[inline]
    pub fn dec(&self) {
        self.sub(1);
    }

    /// The current value.
    #[inline]
    pub fn get(&self) -> i64 {
        self.v.load(Ordering::Relaxed)
    }
}

/// A Prometheus gauge holding a floating-point value.
///
/// Prefer [`Gauge`] unless a fractional value is genuinely needed: `f64`
/// atomics have no native CAS on most Cortex-M cores, so `add`/`sub` here
/// cost a compare-and-swap retry loop instead of one instruction.
#[cfg(feature = "float")]
#[derive(Debug, Default)]
pub struct GaugeF64 {
    v: portable_atomic::AtomicF64,
}

#[cfg(feature = "float")]
impl GaugeF64 {
    /// Create a new gauge with the given initial value.
    pub const fn new(initial: f64) -> Self {
        Self { v: portable_atomic::AtomicF64::new(initial) }
    }

    /// Set the value.
    #[inline]
    pub fn set(&self, v: f64) {
        self.v.store(v, Ordering::Relaxed);
    }

    /// Add `d` to the value (`d` may be negative). Implemented as a CAS loop.
    #[inline]
    pub fn add(&self, d: f64) {
        self.v.fetch_add(d, Ordering::Relaxed);
    }

    /// The current value.
    #[inline]
    pub fn get(&self) -> f64 {
        self.v.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_moves_up_and_down() {
        let g = Gauge::new(0);
        g.set(10);
        assert_eq!(g.get(), 10);
        g.inc();
        g.add(5);
        g.dec();
        g.sub(2);
        assert_eq!(g.get(), 13);
    }

    #[test]
    fn gauge_can_be_negative() {
        let g = Gauge::new(-5);
        assert_eq!(g.get(), -5);
    }

    #[cfg(feature = "float")]
    #[test]
    fn gauge_f64_moves_up_and_down() {
        let g = GaugeF64::new(0.0);
        g.set(1.5);
        g.add(0.5);
        assert_eq!(g.get(), 2.0);
    }
}
