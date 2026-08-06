//! Type-erased view over metrics, used for dynamic dispatch from the
//! registry without the exporter knowing any crate's concrete metrics type.

use core::fmt;

use crate::counter::Counter;
use crate::value::Value;

/// A coherent histogram reading backed by the initialized prefix of the
/// caller-provided bucket buffer.
#[cfg(feature = "consistent-histograms")]
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistogramSnapshot<'a> {
    /// Non-cumulative counts for the finite buckets, in bound order.
    pub buckets: &'a [u64],
    /// Sum of every observation represented by this snapshot.
    pub sum: Value,
    /// Total observation count, also used for the implicit `+Inf` bucket.
    pub count: u64,
}

/// The caller-provided histogram snapshot buffer was too small.
#[cfg(feature = "consistent-histograms")]
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistogramSnapshotError {
    /// Number of finite bucket slots required.
    pub required: usize,
    /// Number of bucket slots provided.
    pub capacity: usize,
}

#[cfg(feature = "consistent-histograms")]
pub(crate) fn snapshot_bucket_prefix(
    buckets: &mut [u64],
    required: usize,
) -> Result<&mut [u64], HistogramSnapshotError> {
    let capacity = buckets.len();
    if capacity < required {
        return Err(HistogramSnapshotError { required, capacity });
    }
    Ok(&mut buckets[..required])
}

/// Object-safe view over a labeled counter collection ([`crate::vec::CounterVec`]).
#[doc(hidden)]
pub trait ErasedCounterVec: Sync {
    /// Number of distinct label-value combinations currently recorded.
    fn series_count(&self) -> usize;
    /// Write the label block (e.g. `reason="timeout"`, no braces) for series `s`.
    fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result;
    /// The current value of series `s`.
    fn value(&self, s: usize) -> u64;
}

/// Object-safe view over a labeled gauge collection ([`crate::vec::GaugeVec`]).
#[doc(hidden)]
pub trait ErasedGaugeVec: Sync {
    /// Number of distinct label-value combinations currently recorded.
    fn series_count(&self) -> usize;
    /// Write the label block for series `s`.
    fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result;
    /// The current value of series `s`.
    fn value(&self, s: usize) -> Value;
}

/// Object-safe view over an unlabeled histogram
/// ([`crate::histogram::IntHistogram`] or [`crate::histogram::Histogram`]).
#[doc(hidden)]
pub trait ErasedHistogram: Sync {
    /// Number of finite buckets. The mandatory implicit `+Inf` bucket is not
    /// stored separately because its value is exactly [`Self::total_count`].
    fn bucket_count(&self) -> usize;
    /// The upper bound of finite bucket `b`. Bounds must be strictly
    /// increasing; floating-point bounds must also be finite.
    fn bound(&self, b: usize) -> Value;
    /// The non-cumulative count for finite bucket `b`.
    fn bucket(&self, b: usize) -> u64;
    /// Total number of observations (equal to the cumulative `+Inf` bucket).
    fn total_count(&self) -> u64;
    /// Sum of all observed values.
    fn sum(&self) -> Value;
    /// Copy one coherent finite-bucket/sum/count snapshot into `buckets`.
    ///
    /// The returned [`HistogramSnapshot::buckets`] slice identifies exactly
    /// the initialized prefix. Implementations must prevent an observation
    /// from interleaving with the copy.
    ///
    /// # Errors
    ///
    /// Returns [`HistogramSnapshotError`] if `buckets` has fewer than
    /// [`Self::bucket_count`] entries.
    #[cfg(feature = "consistent-histograms")]
    fn snapshot<'a>(
        &self,
        buckets: &'a mut [u64],
    ) -> Result<HistogramSnapshot<'a>, HistogramSnapshotError>;
}

/// Object-safe view over a labeled histogram collection
/// ([`crate::vec::IntHistogramVec`] or [`crate::vec::HistogramVec`]).
#[doc(hidden)]
pub trait ErasedHistogramVec: Sync {
    /// Number of finite buckets. The mandatory implicit `+Inf` bucket is not
    /// stored separately because its value is exactly [`Self::total_count`].
    fn bucket_count(&self) -> usize;
    /// The upper bound of finite bucket `b`, shared by every series. Bounds
    /// must be strictly increasing; floating-point bounds must also be finite.
    fn bound(&self, b: usize) -> Value;
    /// Number of distinct label-value combinations currently recorded.
    fn series_count(&self) -> usize;
    /// Write the label block for series `s` (without the `le` label).
    fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result;
    /// The non-cumulative count for finite bucket `b` of series `s`.
    fn bucket(&self, s: usize, b: usize) -> u64;
    /// Total number of observations for series `s`.
    fn total_count(&self, s: usize) -> u64;
    /// Sum of all observed values for series `s`.
    fn sum(&self, s: usize) -> Value;
    /// Copy one coherent bucket/sum/count snapshot for series `s` into
    /// `buckets`. See [`ErasedHistogram::snapshot`] for the contract.
    ///
    /// # Errors
    ///
    /// Returns [`HistogramSnapshotError`] if `buckets` has fewer than
    /// [`Self::bucket_count`] entries.
    #[cfg(feature = "consistent-histograms")]
    fn snapshot<'a>(
        &self,
        s: usize,
        buckets: &'a mut [u64],
    ) -> Result<HistogramSnapshot<'a>, HistogramSnapshotError>;
}

/// A type-erased reference to one metric's data.
#[doc(hidden)]
pub enum MetricRef<'a> {
    /// Counters have one concrete representation, so retaining a reference
    /// avoids loading their atomic value while rendering HELP and TYPE lines.
    Counter(&'a Counter),
    /// Gauges have integer and floating-point representations, so normalize
    /// their current value eagerly instead of adding an erased gauge trait.
    Gauge(Value),
    Histogram {
        h: &'a dyn ErasedHistogram,
    },
    CounterVec(&'a dyn ErasedCounterVec),
    GaugeVec(&'a dyn ErasedGaugeVec),
    HistogramVec {
        h: &'a dyn ErasedHistogramVec,
    },
}

/// One metric's identity and current data, as produced by a [`MetricGroup`].
#[doc(hidden)]
pub struct MetricDesc<'a> {
    /// The metric group's namespace, or `""` if none. Prepended to `name` as
    /// `<namespace>_<name>` when rendered.
    pub namespace: &'static str,
    pub name: &'static str,
    pub help: &'static str,
    pub metric: MetricRef<'a>,
}

/// Implemented by the struct generated by [`crate::metrics!`]. Registered
/// with [`crate::register`] and dispatched to dynamically by the renderer, so
/// the exporter never needs to know any crate's concrete metrics type.
///
/// This is public only so [`crate::metrics!`] can implement it in downstream
/// crates. Manual implementations are unsupported; use the macro instead.
#[doc(hidden)]
pub trait MetricGroup: core::any::Any + Sync {
    /// The group's namespace (see [`MetricDesc::namespace`]).
    fn group_name(&self) -> &'static str;
    /// Number of metrics in this group.
    fn len(&self) -> usize;
    /// The metric at `index` (in `0..self.len()`), or `None` if out of range.
    fn get(&self, index: usize) -> Option<MetricDesc<'_>>;

    /// Whether this group has no metrics.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod fixture {
        crate::metrics! {
            namespace = "fixture";

            counter requests = "Total requests.";
            gauge temperature = "Current temperature.";
        }
    }

    #[test]
    fn dynamic_dispatch_over_metric_group() {
        let g = fixture::get();
        g.temperature.set(21);
        g.requests.inc_by(3);

        let group: &dyn MetricGroup = g;
        assert_eq!(group.len(), 2);
        assert!(!group.is_empty());

        let d0 = group.get(0).unwrap();
        assert_eq!(d0.name, "requests");
        match d0.metric {
            MetricRef::Counter(c) => assert_eq!(c.get(), 3),
            _ => panic!("expected Counter"),
        }

        let d1 = group.get(1).unwrap();
        match d1.metric {
            MetricRef::Gauge(Value::I64(v)) => assert_eq!(v, 21),
            _ => panic!("expected Gauge(I64)"),
        }

        assert!(group.get(2).is_none());
    }
}
