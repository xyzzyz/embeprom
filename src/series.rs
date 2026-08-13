//! Borrowed handles to a single histogram series.
//!
//! [`IntHistSeries`] is the shared view returned by both
//! [`crate::IntHistogram::series`] and [`crate::IntHistogramVec::with`].
//! [`HistSeries`] is the floating-point counterpart.

use portable_atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
struct IntHistMetric<'a> {
    buckets: &'a [AtomicU64],
    sum: &'a AtomicU64,
    count: &'a AtomicU64,
}

/// An infallible, reusable handle to one histogram series.
///
/// Returned by [`crate::IntHistogram::series`] and
/// [`crate::IntHistogramVec::with`]. Retaining this handle avoids later
/// label-map lookups. A rejected labeled binding produces an unrendered sink
/// whose observations are no-ops and whose reads return zero.
#[derive(Clone, Copy)]
pub struct IntHistSeries<'a> {
    bounds: &'static [u64],
    metric: Option<IntHistMetric<'a>>,
}

impl<'a> IntHistSeries<'a> {
    pub(crate) fn bound(
        bounds: &'static [u64],
        buckets: &'a [AtomicU64],
        sum: &'a AtomicU64,
        count: &'a AtomicU64,
    ) -> Self {
        Self {
            bounds,
            metric: Some(IntHistMetric {
                buckets,
                sum,
                count,
            }),
        }
    }

    pub(crate) const fn sink(bounds: &'static [u64]) -> Self {
        Self {
            bounds,
            metric: None,
        }
    }

    /// Record an observation, or do nothing if this is a sink. With
    /// `consistent-histograms`, real-series observations are serialized so a
    /// concurrent renderer can take a coherent snapshot.
    #[inline]
    pub fn observe(&self, v: u64) {
        let Some(metric) = self.metric else {
            return;
        };
        #[cfg(feature = "consistent-histograms")]
        critical_section::with(|_cs| Self::observe_inner(self.bounds, metric, v));
        #[cfg(not(feature = "consistent-histograms"))]
        Self::observe_inner(self.bounds, metric, v);
    }

    #[inline]
    fn observe_inner(bounds: &[u64], metric: IntHistMetric<'_>, v: u64) {
        if let Some(i) = bounds.iter().position(|bound| v <= *bound) {
            metric.buckets[i].fetch_add(1, Ordering::Relaxed);
        }
        metric.sum.fetch_add(v, Ordering::Relaxed);
        metric.count.fetch_add(1, Ordering::Relaxed);
    }

    /// The bucket upper bounds, ascending, excluding the implicit `+Inf`
    /// bucket. Shared by every series in the collection, including sinks.
    pub fn bounds(&self) -> &'static [u64] {
        self.bounds
    }

    /// The non-cumulative count for bucket `i`, or zero if this is a sink.
    ///
    /// # Panics
    ///
    /// Panics if `i` is not a finite bucket index.
    pub fn bucket(&self, i: usize) -> u64 {
        assert!(
            i < self.bounds.len(),
            "embeprom: histogram bucket index out of range"
        );
        self.metric
            .map_or(0, |metric| metric.buckets[i].load(Ordering::Relaxed))
    }

    /// Total number of observations (equal to the cumulative `+Inf` bucket),
    /// or zero if this is a sink.
    pub fn count(&self) -> u64 {
        self.metric
            .map_or(0, |metric| metric.count.load(Ordering::Relaxed))
    }

    /// Sum of all observed values, or zero if this is a sink.
    pub fn sum(&self) -> u64 {
        self.metric
            .map_or(0, |metric| metric.sum.load(Ordering::Relaxed))
    }
}

#[cfg(feature = "float")]
#[derive(Clone, Copy)]
struct HistMetric<'a> {
    buckets: &'a [AtomicU64],
    sum: &'a portable_atomic::AtomicF64,
    count: &'a AtomicU64,
}

/// An infallible, reusable handle to one floating-point histogram series.
///
/// Returned by [`crate::Histogram::series`] and [`crate::HistogramVec::with`].
/// Retaining this handle avoids later label-map lookups. A rejected labeled
/// binding produces an unrendered sink whose observations are no-ops and
/// whose reads return zero.
#[cfg(feature = "float")]
#[derive(Clone, Copy)]
pub struct HistSeries<'a> {
    bounds: &'static [f64],
    metric: Option<HistMetric<'a>>,
}

#[cfg(feature = "float")]
impl<'a> HistSeries<'a> {
    pub(crate) fn bound(
        bounds: &'static [f64],
        buckets: &'a [AtomicU64],
        sum: &'a portable_atomic::AtomicF64,
        count: &'a AtomicU64,
    ) -> Self {
        Self {
            bounds,
            metric: Some(HistMetric {
                buckets,
                sum,
                count,
            }),
        }
    }

    pub(crate) const fn sink(bounds: &'static [f64]) -> Self {
        Self {
            bounds,
            metric: None,
        }
    }

    /// Record an observation, or do nothing if this is a sink. With
    /// `consistent-histograms`, real-series observations are serialized so a
    /// concurrent renderer can take a coherent snapshot.
    #[inline]
    pub fn observe(&self, v: f64) {
        let Some(metric) = self.metric else {
            return;
        };
        #[cfg(feature = "consistent-histograms")]
        critical_section::with(|_cs| Self::observe_inner(self.bounds, metric, v));
        #[cfg(not(feature = "consistent-histograms"))]
        Self::observe_inner(self.bounds, metric, v);
    }

    #[inline]
    fn observe_inner(bounds: &[f64], metric: HistMetric<'_>, v: f64) {
        if let Some(i) = bounds.iter().position(|bound| v <= *bound) {
            metric.buckets[i].fetch_add(1, Ordering::Relaxed);
        }
        metric.sum.fetch_add(v, Ordering::Relaxed);
        metric.count.fetch_add(1, Ordering::Relaxed);
    }

    /// The bucket upper bounds, ascending, excluding the implicit `+Inf`
    /// bucket. Shared by every series in the collection, including sinks.
    pub fn bounds(&self) -> &'static [f64] {
        self.bounds
    }

    /// The non-cumulative count for bucket `i`, or zero if this is a sink.
    ///
    /// # Panics
    ///
    /// Panics if `i` is not a finite bucket index.
    pub fn bucket(&self, i: usize) -> u64 {
        assert!(
            i < self.bounds.len(),
            "embeprom: histogram bucket index out of range"
        );
        self.metric
            .map_or(0, |metric| metric.buckets[i].load(Ordering::Relaxed))
    }

    /// Total number of observations (equal to the cumulative `+Inf` bucket),
    /// or zero if this is a sink.
    pub fn count(&self) -> u64 {
        self.metric
            .map_or(0, |metric| metric.count.load(Ordering::Relaxed))
    }

    /// Sum of all observed values, or zero if this is a sink.
    pub fn sum(&self) -> f64 {
        self.metric
            .map_or(0.0, |metric| metric.sum.load(Ordering::Relaxed))
    }
}
