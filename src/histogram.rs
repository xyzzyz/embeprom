//! Histograms with fixed, compile-time-declared bucket boundaries.

use portable_atomic::{AtomicU64, Ordering};

use crate::erased::ErasedHistogram;
use crate::value::Value;

pub(crate) const fn validate_u64_bounds<const B: usize>(bounds: &[u64]) {
    assert!(
        bounds.len() == B,
        "embeprom: histogram bounds.len() must equal B"
    );
    let mut i = 1;
    while i < B {
        assert!(
            bounds[i - 1] < bounds[i],
            "embeprom: histogram bounds must be strictly increasing"
        );
        i += 1;
    }
}

#[cfg(feature = "float")]
pub(crate) const fn validate_f64_bounds<const B: usize>(bounds: &[f64]) {
    assert!(
        bounds.len() == B,
        "embeprom: histogram bounds.len() must equal B"
    );
    let mut i = 0;
    while i < B {
        assert!(
            bounds[i].is_finite(),
            "embeprom: histogram bounds must be finite"
        );
        if i > 0 {
            assert!(
                bounds[i - 1] < bounds[i],
                "embeprom: histogram bounds must be strictly increasing"
            );
        }
        i += 1;
    }
}

/// A Prometheus histogram over `u64` observations, with `B` finite buckets
/// plus an implicit `+Inf` bucket (represented by `count`).
///
/// Bucket counts are stored non-cumulatively and accumulated with one rolling
/// total at render time. Each finite bucket is read once, so both observation
/// and rendering are linear scans with `observe` performing exactly 3 atomic
/// ops regardless of `B`.
///
/// The `_bucket`, `_sum`, and `_count` values are three independent atomics,
/// so a concurrent render can observe `count` one ahead of the bucket sum.
/// Prometheus tolerates this; enable the `consistent-histograms` feature to
/// serialize observations and let the renderer take one coherent snapshot
/// for every histogram family if it matters.
pub struct IntHistogram<const B: usize> {
    bounds: &'static [u64],
    buckets: [AtomicU64; B],
    sum: AtomicU64,
    count: AtomicU64,
}

impl<const B: usize> IntHistogram<B> {
    /// Create a histogram with strictly increasing bucket upper bounds
    /// (exclusive of the implicit `+Inf` bucket). `bounds.len()` must equal
    /// `B`; invalid bounds panic, including during const evaluation.
    pub const fn new(bounds: &'static [u64]) -> Self {
        validate_u64_bounds::<B>(bounds);
        Self {
            bounds,
            buckets: [const { AtomicU64::new(0) }; B],
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    /// Record an observation.
    #[cfg(not(feature = "consistent-histograms"))]
    #[inline]
    pub fn observe(&self, v: u64) {
        self.observe_inner(v);
    }

    /// Record an observation. Wrapped in a critical section so `_bucket`,
    /// `_sum`, and `_count` are always mutually consistent under a concurrent
    /// render (feature `consistent-histograms`).
    #[cfg(feature = "consistent-histograms")]
    #[inline]
    pub fn observe(&self, v: u64) {
        critical_section::with(|_cs| self.observe_inner(v));
    }

    #[inline]
    fn observe_inner(&self, v: u64) {
        let mut i = 0;
        while i < B && v > self.bounds[i] {
            i += 1;
        }
        if i < B {
            self.buckets[i].fetch_add(1, Ordering::Relaxed);
        }
        self.sum.fetch_add(v, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// The bucket upper bounds, ascending, excluding the implicit `+Inf` bucket.
    pub fn bounds(&self) -> &'static [u64] {
        self.bounds
    }

    /// The non-cumulative count for bucket `i` (i.e. observations that fell
    /// into `(bounds[i-1], bounds[i]]`, or `[0, bounds[0]]` for `i == 0`).
    pub fn bucket(&self, i: usize) -> u64 {
        self.buckets[i].load(Ordering::Relaxed)
    }

    /// Total number of observations (equal to the cumulative `+Inf` bucket).
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Sum of all observed values.
    pub fn sum(&self) -> u64 {
        self.sum.load(Ordering::Relaxed)
    }
}

impl<const B: usize> ErasedHistogram for IntHistogram<B> {
    fn bucket_count(&self) -> usize {
        B
    }
    fn bound(&self, b: usize) -> Value {
        Value::U64(self.bounds[b])
    }
    fn bucket(&self, b: usize) -> u64 {
        IntHistogram::bucket(self, b)
    }
    fn total_count(&self) -> u64 {
        self.count()
    }
    fn sum(&self) -> Value {
        Value::U64(IntHistogram::sum(self))
    }
    #[cfg(feature = "consistent-histograms")]
    fn snapshot(&self, buckets: &mut [u64]) -> (Value, u64) {
        debug_assert!(buckets.len() >= B);
        critical_section::with(|_cs| {
            for (i, bucket) in buckets[..B].iter_mut().enumerate() {
                *bucket = self.buckets[i].load(Ordering::Relaxed);
            }
            (
                Value::U64(self.sum.load(Ordering::Relaxed)),
                self.count.load(Ordering::Relaxed),
            )
        })
    }
}

/// A Prometheus histogram over `f64` observations. See [`IntHistogram`] for
/// the storage and consistency model; prefer `IntHistogram` when values are
/// naturally integral, since `f64` atomics have no native CAS on most
/// Cortex-M cores.
#[cfg(feature = "float")]
pub struct Histogram<const B: usize> {
    bounds: &'static [f64],
    buckets: [AtomicU64; B],
    sum: portable_atomic::AtomicF64,
    count: AtomicU64,
}

#[cfg(feature = "float")]
impl<const B: usize> Histogram<B> {
    /// Create a histogram with strictly increasing, finite bucket upper bounds
    /// (exclusive of the implicit `+Inf` bucket). `bounds.len()` must equal
    /// `B`; invalid bounds panic, including during const evaluation.
    pub const fn new(bounds: &'static [f64]) -> Self {
        validate_f64_bounds::<B>(bounds);
        Self {
            bounds,
            buckets: [const { AtomicU64::new(0) }; B],
            sum: portable_atomic::AtomicF64::new(0.0),
            count: AtomicU64::new(0),
        }
    }

    /// Record an observation.
    #[cfg(not(feature = "consistent-histograms"))]
    #[inline]
    pub fn observe(&self, v: f64) {
        self.observe_inner(v);
    }

    /// Record an observation, wrapped in a critical section for consistency
    /// (feature `consistent-histograms`). See [`IntHistogram::observe`].
    #[cfg(feature = "consistent-histograms")]
    #[inline]
    pub fn observe(&self, v: f64) {
        critical_section::with(|_cs| self.observe_inner(v));
    }

    #[inline]
    fn observe_inner(&self, v: f64) {
        let mut i = 0;
        while i < B && v > self.bounds[i] {
            i += 1;
        }
        if i < B {
            self.buckets[i].fetch_add(1, Ordering::Relaxed);
        }
        self.sum.fetch_add(v, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// The bucket upper bounds, ascending, excluding the implicit `+Inf` bucket.
    pub fn bounds(&self) -> &'static [f64] {
        self.bounds
    }

    /// The non-cumulative count for bucket `i`.
    pub fn bucket(&self, i: usize) -> u64 {
        self.buckets[i].load(Ordering::Relaxed)
    }

    /// Total number of observations (equal to the cumulative `+Inf` bucket).
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Sum of all observed values.
    pub fn sum(&self) -> f64 {
        self.sum.load(Ordering::Relaxed)
    }
}

#[cfg(feature = "float")]
impl<const B: usize> ErasedHistogram for Histogram<B> {
    fn bucket_count(&self) -> usize {
        B
    }
    fn bound(&self, b: usize) -> Value {
        Value::F64(self.bounds[b])
    }
    fn bucket(&self, b: usize) -> u64 {
        Histogram::bucket(self, b)
    }
    fn total_count(&self) -> u64 {
        self.count()
    }
    fn sum(&self) -> Value {
        Value::F64(Histogram::sum(self))
    }
    #[cfg(feature = "consistent-histograms")]
    fn snapshot(&self, buckets: &mut [u64]) -> (Value, u64) {
        debug_assert!(buckets.len() >= B);
        critical_section::with(|_cs| {
            for (i, bucket) in buckets[..B].iter_mut().enumerate() {
                *bucket = self.buckets[i].load(Ordering::Relaxed);
            }
            (
                Value::F64(self.sum.load(Ordering::Relaxed)),
                self.count.load(Ordering::Relaxed),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_and_sum_and_count() {
        let h: IntHistogram<4> = IntHistogram::new(&[100, 500, 1000, 5000]);
        for v in [12, 480, 480, 999, 4999, 4999, 10_000] {
            h.observe(v);
        }
        assert_eq!(h.bucket(0), 1); // <= 100
        assert_eq!(h.bucket(1), 2); // (100, 500]
        assert_eq!(h.bucket(2), 1); // (500, 1000]
        assert_eq!(h.bucket(3), 2); // (1000, 5000]
        assert_eq!(h.count(), 7); // includes the 10_000 +Inf observation
        assert_eq!(h.sum(), 12 + 480 + 480 + 999 + 4999 + 4999 + 10_000);
    }

    #[test]
    #[should_panic(expected = "histogram bounds must be strictly increasing")]
    fn integer_histogram_rejects_duplicate_bounds() {
        let _ = IntHistogram::<3>::new(&[10, 10, 20]);
    }

    #[test]
    #[should_panic(expected = "histogram bounds must be strictly increasing")]
    fn integer_histogram_rejects_descending_bounds() {
        let _ = IntHistogram::<3>::new(&[10, 30, 20]);
    }

    #[cfg(feature = "float")]
    #[test]
    fn float_histogram_buckets() {
        let h: Histogram<2> = Histogram::new(&[0.5, 1.0]);
        h.observe(0.25);
        h.observe(0.75);
        h.observe(2.0);
        assert_eq!(h.bucket(0), 1);
        assert_eq!(h.bucket(1), 1);
        assert_eq!(h.count(), 3);
        assert_eq!(h.sum(), 3.0);
    }

    #[cfg(feature = "float")]
    #[test]
    #[should_panic(expected = "histogram bounds must be finite")]
    fn float_histogram_rejects_nan_bounds() {
        let _ = Histogram::<2>::new(&[0.5, f64::NAN]);
    }

    #[cfg(feature = "float")]
    #[test]
    #[should_panic(expected = "histogram bounds must be finite")]
    fn float_histogram_rejects_infinite_bounds() {
        let _ = Histogram::<2>::new(&[0.5, f64::INFINITY]);
    }
}
