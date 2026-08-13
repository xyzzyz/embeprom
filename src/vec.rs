//! Labeled metric collections with a compile-time-declared maximum
//! cardinality: at most `N` distinct label-value combinations are tracked
//! per metric. Once full, new combinations are routed to unrendered sink
//! handles whose updates are no-ops, and existing series keep updating.
//!
//! When label values are stable, call a collection's `with` method once and
//! retain the returned series handle. Updates through that handle access the
//! metric storage directly and avoid rebuilding and looking up the label
//! block on every observation. The convenience methods accepting label
//! values are intended for dynamic or infrequent labels.

use core::cell::RefCell;
use core::fmt;

use critical_section::Mutex;
use portable_atomic::{AtomicU64, Ordering};

use crate::config::LABEL_VALUE_LEN;
use crate::counter::Counter;
use crate::erased::{ErasedCounterVec, ErasedGaugeVec, ErasedHistogramVec};
#[cfg(feature = "consistent-histograms")]
use crate::erased::{HistogramSnapshot, HistogramSnapshotError, snapshot_bucket_prefix};
use crate::escape::valid_label_name;
use crate::gauge::Gauge;
#[cfg(feature = "float")]
use crate::gauge::GaugeF64;
#[cfg(feature = "float")]
use crate::histogram::validate_f64_bounds;
use crate::histogram::validate_u64_bounds;
use crate::labels::{LabelBlock, build_block};
use crate::value::Value;

type KeyMap<const N: usize, const V: usize> = Mutex<RefCell<heapless::Vec<LabelBlock<V>, N>>>;

const fn str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn validate_label_names<const K: usize>(names: &'static [&'static str; K], histogram: bool) {
    if histogram {
        assert!(
            K > 0,
            "embeprom: histogram vectors require at least one label"
        );
    }
    let mut i = 0;
    while i < K {
        assert!(valid_label_name(names[i]), "embeprom: invalid label name");
        assert!(
            !(histogram && str_eq(names[i], "le")),
            "embeprom: histogram label name `le` is reserved"
        );

        let mut j = 0;
        while j < i {
            assert!(
                !str_eq(names[i], names[j]),
                "embeprom: duplicate label name"
            );
            j += 1;
        }
        i += 1;
    }
}

/// Look up `values`'s slot in `keys`, creating it (and returning its index)
/// if not already present. Returns `None` if the label block doesn't fit in
/// `V` bytes, or if capacity `N` is exhausted.
fn slot_index<const N: usize, const K: usize, const V: usize>(
    keys: &KeyMap<N, V>,
    names: &'static [&'static str; K],
    values: &[&str; K],
) -> Option<usize> {
    let block = build_block::<K, V>(names, values).ok()?;
    critical_section::with(|cs| {
        let mut keys = keys.borrow_ref_mut(cs);
        if let Some(i) = keys.iter().position(|k| *k == block) {
            return Some(i);
        }
        keys.push(block).ok().map(|()| keys.len() - 1)
    })
}

fn write_labels_at<const N: usize, const V: usize>(
    keys: &KeyMap<N, V>,
    s: usize,
    out: &mut dyn fmt::Write,
) -> fmt::Result {
    let block = critical_section::with(|cs| keys.borrow_ref(cs)[s].clone());
    out.write_str(&block)
}

fn series_count_of<const N: usize, const V: usize>(keys: &KeyMap<N, V>) -> usize {
    critical_section::with(|cs| keys.borrow_ref(cs).len())
}

/// An infallible handle to one [`CounterVec`] series.
///
/// If the requested label block did not fit or the collection was full, this
/// handle is a sink: updates are no-ops and [`get`](Self::get) returns zero.
#[derive(Debug, Clone, Copy)]
pub struct CounterSeries<'a> {
    counter: Option<&'a Counter>,
}

impl CounterSeries<'_> {
    fn metric(counter: &Counter) -> CounterSeries<'_> {
        CounterSeries {
            counter: Some(counter),
        }
    }

    const fn sink() -> Self {
        Self { counter: None }
    }

    /// Increment this series by 1, or do nothing if it is a sink.
    #[inline]
    pub fn inc(&self) {
        if let Some(counter) = self.counter {
            counter.inc();
        }
    }

    /// Increment this series by `n`, or do nothing if it is a sink.
    #[inline]
    pub fn inc_by(&self, n: u64) {
        if let Some(counter) = self.counter {
            counter.inc_by(n);
        }
    }

    /// Return the current value, or zero if this is a sink.
    #[inline]
    pub fn get(&self) -> u64 {
        self.counter.map_or(0, Counter::get)
    }

    /// Reset this series to zero, or do nothing if it is a sink.
    #[inline]
    pub fn reset(&self) {
        if let Some(counter) = self.counter {
            counter.reset();
        }
    }
}

/// An infallible handle to one [`GaugeVec`] series.
///
/// If the requested label block did not fit or the collection was full, this
/// handle is a sink: updates are no-ops and [`get`](Self::get) returns zero.
#[derive(Debug, Clone, Copy)]
pub struct GaugeSeries<'a> {
    gauge: Option<&'a Gauge>,
}

impl GaugeSeries<'_> {
    fn metric(gauge: &Gauge) -> GaugeSeries<'_> {
        GaugeSeries { gauge: Some(gauge) }
    }

    const fn sink() -> Self {
        Self { gauge: None }
    }

    /// Set this series, or do nothing if it is a sink.
    #[inline]
    pub fn set(&self, value: i64) {
        if let Some(gauge) = self.gauge {
            gauge.set(value);
        }
    }

    /// Add `delta` to this series, or do nothing if it is a sink.
    #[inline]
    pub fn add(&self, delta: i64) {
        if let Some(gauge) = self.gauge {
            gauge.add(delta);
        }
    }

    /// Subtract `delta` from this series, or do nothing if it is a sink.
    #[inline]
    pub fn sub(&self, delta: i64) {
        if let Some(gauge) = self.gauge {
            gauge.sub(delta);
        }
    }

    /// Increment this series by 1, or do nothing if it is a sink.
    #[inline]
    pub fn inc(&self) {
        if let Some(gauge) = self.gauge {
            gauge.inc();
        }
    }

    /// Decrement this series by 1, or do nothing if it is a sink.
    #[inline]
    pub fn dec(&self) {
        if let Some(gauge) = self.gauge {
            gauge.dec();
        }
    }

    /// Return the current value, or zero if this is a sink.
    #[inline]
    pub fn get(&self) -> i64 {
        self.gauge.map_or(0, Gauge::get)
    }
}

/// A labeled counter collection with at most `N` distinct label-value
/// combinations, `K` label names, and a rendered label-block byte budget `V`.
pub struct CounterVec<const N: usize, const K: usize = 1, const V: usize = LABEL_VALUE_LEN> {
    names: &'static [&'static str; K],
    keys: KeyMap<N, V>,
    vals: [Counter; N],
}

impl<const N: usize, const K: usize, const V: usize> CounterVec<N, K, V> {
    /// Create an empty labeled counter collection with the given distinct,
    /// valid Prometheus label names. Invalid or duplicate names panic,
    /// including during const evaluation.
    pub const fn new(names: &'static [&'static str; K]) -> Self {
        validate_label_names(names, false);
        Self {
            names,
            keys: Mutex::new(RefCell::new(heapless::Vec::new())),
            vals: [const { Counter::new() }; N],
        }
    }

    /// The series for `values`, creating it if not already present. If
    /// capacity `N` is exhausted or the rendered label block exceeds `V`
    /// bytes, returns an unrendered sink handle whose updates are no-ops.
    /// Retain the returned handle for repeated hot-path updates with stable
    /// label values.
    ///
    /// ```
    /// static LABELS: [&str; 1] = ["reason"];
    /// let counters: embeprom::CounterVec<4> = embeprom::CounterVec::new(&LABELS);
    /// let timeout = counters.with(&["timeout"]);
    /// timeout.inc();
    /// timeout.inc();
    /// assert_eq!(timeout.get(), 2);
    /// ```
    pub fn with(&self, values: &[&str; K]) -> CounterSeries<'_> {
        match slot_index(&self.keys, self.names, values) {
            Some(idx) => CounterSeries::metric(&self.vals[idx]),
            None => CounterSeries::sink(),
        }
    }

    /// Increment the series for `values` by 1. This performs a label lookup on
    /// every call; rejected label blocks are routed to a sink.
    #[inline]
    pub fn inc(&self, values: &[&str; K]) {
        self.with(values).inc();
    }

    /// Increment the series for `values` by `n`. This performs a label lookup
    /// on every call; rejected label blocks are routed to a sink.
    #[inline]
    pub fn inc_by(&self, values: &[&str; K], n: u64) {
        self.with(values).inc_by(n);
    }

    /// Number of distinct label-value combinations currently recorded.
    pub fn series_count(&self) -> usize {
        series_count_of(&self.keys)
    }
}

impl<const N: usize, const K: usize, const V: usize> ErasedCounterVec for CounterVec<N, K, V> {
    fn series_count(&self) -> usize {
        CounterVec::series_count(self)
    }
    fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result {
        write_labels_at(&self.keys, s, out)
    }
    fn value(&self, s: usize) -> u64 {
        self.vals[s].get()
    }
}

/// A labeled gauge collection with at most `N` distinct label-value
/// combinations, `K` label names, and a rendered label-block byte budget `V`.
pub struct GaugeVec<const N: usize, const K: usize = 1, const V: usize = LABEL_VALUE_LEN> {
    names: &'static [&'static str; K],
    keys: KeyMap<N, V>,
    vals: [Gauge; N],
}

impl<const N: usize, const K: usize, const V: usize> GaugeVec<N, K, V> {
    /// Create an empty labeled gauge collection with the given distinct,
    /// valid Prometheus label names. Invalid or duplicate names panic,
    /// including during const evaluation.
    pub const fn new(names: &'static [&'static str; K]) -> Self {
        validate_label_names(names, false);
        Self {
            names,
            keys: Mutex::new(RefCell::new(heapless::Vec::new())),
            vals: [const { Gauge::new(0) }; N],
        }
    }

    /// The series for `values`, creating it if not already present. See
    /// [`CounterVec::with`] for the capacity/length failure behavior and the
    /// cached-handle hot-path pattern.
    pub fn with(&self, values: &[&str; K]) -> GaugeSeries<'_> {
        match slot_index(&self.keys, self.names, values) {
            Some(idx) => GaugeSeries::metric(&self.vals[idx]),
            None => GaugeSeries::sink(),
        }
    }

    /// Set the series for `values`. This performs a label lookup on every call
    /// and routes rejected label blocks to a sink.
    #[inline]
    pub fn set(&self, values: &[&str; K], v: i64) {
        self.with(values).set(v);
    }

    /// Add `d` to the series for `values`. This performs a label lookup on
    /// every call and routes rejected label blocks to a sink.
    #[inline]
    pub fn add(&self, values: &[&str; K], d: i64) {
        self.with(values).add(d);
    }

    /// Number of distinct label-value combinations currently recorded.
    pub fn series_count(&self) -> usize {
        series_count_of(&self.keys)
    }
}

impl<const N: usize, const K: usize, const V: usize> ErasedGaugeVec for GaugeVec<N, K, V> {
    fn series_count(&self) -> usize {
        GaugeVec::series_count(self)
    }
    fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result {
        write_labels_at(&self.keys, s, out)
    }
    fn value(&self, s: usize) -> Value {
        Value::I64(self.vals[s].get())
    }
}

/// An infallible handle to one [`GaugeF64Vec`] series.
///
/// If the requested label block did not fit or the collection was full, this
/// handle is a sink: updates are no-ops and [`get`](Self::get) returns zero.
#[cfg(feature = "float")]
#[derive(Debug, Clone, Copy)]
pub struct GaugeF64Series<'a> {
    gauge: Option<&'a GaugeF64>,
}

#[cfg(feature = "float")]
impl GaugeF64Series<'_> {
    fn metric(gauge: &GaugeF64) -> GaugeF64Series<'_> {
        GaugeF64Series { gauge: Some(gauge) }
    }

    const fn sink() -> Self {
        Self { gauge: None }
    }

    /// Set this series, or do nothing if it is a sink.
    #[inline]
    pub fn set(&self, value: f64) {
        if let Some(gauge) = self.gauge {
            gauge.set(value);
        }
    }

    /// Add `delta` to this series, or do nothing if it is a sink.
    #[inline]
    pub fn add(&self, delta: f64) {
        if let Some(gauge) = self.gauge {
            gauge.add(delta);
        }
    }

    /// Subtract `delta` from this series, or do nothing if it is a sink.
    #[inline]
    pub fn sub(&self, delta: f64) {
        self.add(-delta);
    }

    /// Increment this series by 1, or do nothing if it is a sink.
    #[inline]
    pub fn inc(&self) {
        self.add(1.0);
    }

    /// Decrement this series by 1, or do nothing if it is a sink.
    #[inline]
    pub fn dec(&self) {
        self.add(-1.0);
    }

    /// Return the current value, or zero if this is a sink.
    #[inline]
    pub fn get(&self) -> f64 {
        self.gauge.map_or(0.0, GaugeF64::get)
    }
}

/// A labeled `f64` gauge collection with at most `N` distinct label-value
/// combinations, `K` label names, and a rendered label-block byte budget `V`.
///
/// Prefer [`GaugeVec`] unless a fractional value is genuinely needed.
#[cfg(feature = "float")]
pub struct GaugeF64Vec<const N: usize, const K: usize = 1, const V: usize = LABEL_VALUE_LEN> {
    names: &'static [&'static str; K],
    keys: KeyMap<N, V>,
    vals: [GaugeF64; N],
}

#[cfg(feature = "float")]
impl<const N: usize, const K: usize, const V: usize> GaugeF64Vec<N, K, V> {
    /// Create an empty labeled `f64` gauge collection with the given distinct,
    /// valid Prometheus label names. Invalid or duplicate names panic,
    /// including during const evaluation.
    pub const fn new(names: &'static [&'static str; K]) -> Self {
        validate_label_names(names, false);
        Self {
            names,
            keys: Mutex::new(RefCell::new(heapless::Vec::new())),
            vals: [const { GaugeF64::new(0.0) }; N],
        }
    }

    /// The series for `values`, creating it if not already present. See
    /// [`CounterVec::with`] for the capacity/length failure behavior and the
    /// cached-handle hot-path pattern.
    pub fn with(&self, values: &[&str; K]) -> GaugeF64Series<'_> {
        match slot_index(&self.keys, self.names, values) {
            Some(idx) => GaugeF64Series::metric(&self.vals[idx]),
            None => GaugeF64Series::sink(),
        }
    }

    /// Set the series for `values`. This performs a label lookup on every call
    /// and routes rejected label blocks to a sink.
    #[inline]
    pub fn set(&self, values: &[&str; K], v: f64) {
        self.with(values).set(v);
    }

    /// Add `d` to the series for `values`. This performs a label lookup on
    /// every call and routes rejected label blocks to a sink.
    #[inline]
    pub fn add(&self, values: &[&str; K], d: f64) {
        self.with(values).add(d);
    }

    /// Number of distinct label-value combinations currently recorded.
    pub fn series_count(&self) -> usize {
        series_count_of(&self.keys)
    }
}

#[cfg(feature = "float")]
impl<const N: usize, const K: usize, const V: usize> ErasedGaugeVec for GaugeF64Vec<N, K, V> {
    fn series_count(&self) -> usize {
        GaugeF64Vec::series_count(self)
    }
    fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result {
        write_labels_at(&self.keys, s, out)
    }
    fn value(&self, s: usize) -> Value {
        Value::F64(self.vals[s].get())
    }
}

#[derive(Clone, Copy)]
struct IntHistMetric<'a> {
    buckets: &'a [AtomicU64],
    sum: &'a AtomicU64,
    count: &'a AtomicU64,
}

/// An infallible, reusable handle to one histogram series.
///
/// Returned by [`IntHistogram::series`] and [`IntHistogramVec::with`].
/// Retaining this handle avoids later label-map lookups. A rejected labeled
/// binding produces an unrendered sink whose observations are no-ops and
/// whose reads return zero.
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

    const fn sink(bounds: &'static [u64]) -> Self {
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

/// A labeled histogram collection over `u64` observations, with at most `N`
/// distinct label-value combinations, `B` finite buckets shared by all
/// series, `K` label names, and a rendered label-block byte budget `V`.
pub struct IntHistogramVec<
    const N: usize,
    const B: usize,
    const K: usize = 1,
    const V: usize = LABEL_VALUE_LEN,
> {
    names: &'static [&'static str; K],
    bounds: &'static [u64],
    keys: KeyMap<N, V>,
    buckets: [[AtomicU64; B]; N],
    sums: [AtomicU64; N],
    counts: [AtomicU64; N],
}

impl<const N: usize, const B: usize, const K: usize, const V: usize> IntHistogramVec<N, B, K, V> {
    /// Create an empty labeled histogram collection with distinct, valid
    /// Prometheus label names and strictly increasing bucket upper bounds.
    /// `K` must be greater than zero; unlabeled histograms use
    /// [`crate::IntHistogram`]. The name `le` is reserved for generated
    /// bucket labels. Invalid names, duplicate names, reserved names, empty
    /// label lists, and invalid bounds panic, including during const
    /// evaluation.
    pub const fn new(names: &'static [&'static str; K], bounds: &'static [u64]) -> Self {
        validate_label_names(names, true);
        validate_u64_bounds::<B>(bounds);
        Self {
            names,
            bounds,
            keys: Mutex::new(RefCell::new(heapless::Vec::new())),
            buckets: [const { [const { AtomicU64::new(0) }; B] }; N],
            sums: [const { AtomicU64::new(0) }; N],
            counts: [const { AtomicU64::new(0) }; N],
        }
    }

    /// The series for `values`, creating it if not already present. See
    /// [`CounterVec::with`] for the capacity/length failure behavior and the
    /// cached-handle hot-path pattern.
    pub fn with(&self, values: &[&str; K]) -> IntHistSeries<'_> {
        match slot_index(&self.keys, self.names, values) {
            Some(idx) => IntHistSeries::bound(
                self.bounds,
                &self.buckets[idx],
                &self.sums[idx],
                &self.counts[idx],
            ),
            None => IntHistSeries::sink(self.bounds),
        }
    }

    /// Record an observation for the series matching `values`. This performs
    /// a label lookup on every call and routes rejected label blocks to a
    /// sink.
    #[inline]
    pub fn observe(&self, values: &[&str; K], v: u64) {
        self.with(values).observe(v);
    }

    /// The bucket upper bounds, ascending, excluding the implicit `+Inf`
    /// bucket. Shared by every series.
    pub fn bounds(&self) -> &'static [u64] {
        self.bounds
    }

    /// Number of distinct label-value combinations currently recorded.
    pub fn series_count(&self) -> usize {
        series_count_of(&self.keys)
    }
}

impl<const N: usize, const B: usize, const K: usize, const V: usize> ErasedHistogramVec
    for IntHistogramVec<N, B, K, V>
{
    fn bucket_count(&self) -> usize {
        B
    }
    fn bound(&self, b: usize) -> Value {
        Value::U64(self.bounds[b])
    }
    fn series_count(&self) -> usize {
        IntHistogramVec::series_count(self)
    }
    fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result {
        write_labels_at(&self.keys, s, out)
    }
    fn bucket(&self, s: usize, b: usize) -> u64 {
        self.buckets[s][b].load(Ordering::Relaxed)
    }
    fn total_count(&self, s: usize) -> u64 {
        self.counts[s].load(Ordering::Relaxed)
    }
    fn sum(&self, s: usize) -> Value {
        Value::U64(self.sums[s].load(Ordering::Relaxed))
    }
    #[cfg(feature = "consistent-histograms")]
    fn snapshot<'a>(
        &self,
        s: usize,
        buckets: &'a mut [u64],
    ) -> Result<HistogramSnapshot<'a>, HistogramSnapshotError> {
        let buckets = snapshot_bucket_prefix(buckets, B)?;
        critical_section::with(|_cs| {
            for (snapshot_bucket, bucket) in buckets.iter_mut().zip(&self.buckets[s]) {
                *snapshot_bucket = bucket.load(Ordering::Relaxed);
            }
            Ok(HistogramSnapshot {
                buckets,
                sum: Value::U64(self.sums[s].load(Ordering::Relaxed)),
                count: self.counts[s].load(Ordering::Relaxed),
            })
        })
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
/// Returned by [`Histogram::series`] and [`HistogramVec::with`]. Retaining
/// this handle avoids later label-map lookups. A rejected labeled binding
/// produces an unrendered sink whose observations are no-ops and whose reads
/// return zero.
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

    const fn sink(bounds: &'static [f64]) -> Self {
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

/// A labeled histogram collection over `f64` observations. See
/// [`IntHistogramVec`] for the storage/capacity model; prefer that type when
/// values are naturally integral.
#[cfg(feature = "float")]
pub struct HistogramVec<
    const N: usize,
    const B: usize,
    const K: usize = 1,
    const V: usize = LABEL_VALUE_LEN,
> {
    names: &'static [&'static str; K],
    bounds: &'static [f64],
    keys: KeyMap<N, V>,
    buckets: [[AtomicU64; B]; N],
    sums: [portable_atomic::AtomicF64; N],
    counts: [AtomicU64; N],
}

#[cfg(feature = "float")]
impl<const N: usize, const B: usize, const K: usize, const V: usize> HistogramVec<N, B, K, V> {
    /// Create an empty labeled histogram collection with distinct, valid
    /// Prometheus label names and strictly increasing, finite bucket upper
    /// bounds. `K` must be greater than zero; unlabeled histograms use
    /// [`crate::Histogram`]. The name `le` is reserved for generated bucket
    /// labels. Invalid names, duplicate names, reserved names, empty label
    /// lists, and invalid bounds panic, including during const evaluation.
    pub const fn new(names: &'static [&'static str; K], bounds: &'static [f64]) -> Self {
        validate_label_names(names, true);
        validate_f64_bounds::<B>(bounds);
        Self {
            names,
            bounds,
            keys: Mutex::new(RefCell::new(heapless::Vec::new())),
            buckets: [const { [const { AtomicU64::new(0) }; B] }; N],
            sums: [const { portable_atomic::AtomicF64::new(0.0) }; N],
            counts: [const { AtomicU64::new(0) }; N],
        }
    }

    /// The series for `values`, creating it if not already present. See
    /// [`CounterVec::with`] for the capacity/length failure behavior and the
    /// cached-handle hot-path pattern.
    pub fn with(&self, values: &[&str; K]) -> HistSeries<'_> {
        match slot_index(&self.keys, self.names, values) {
            Some(idx) => HistSeries::bound(
                self.bounds,
                &self.buckets[idx],
                &self.sums[idx],
                &self.counts[idx],
            ),
            None => HistSeries::sink(self.bounds),
        }
    }

    /// Record an observation for the series matching `values`. This performs
    /// a label lookup on every call and routes rejected label blocks to a
    /// sink.
    #[inline]
    pub fn observe(&self, values: &[&str; K], v: f64) {
        self.with(values).observe(v);
    }

    /// The bucket upper bounds, ascending, excluding the implicit `+Inf`
    /// bucket. Shared by every series.
    pub fn bounds(&self) -> &'static [f64] {
        self.bounds
    }

    /// Number of distinct label-value combinations currently recorded.
    pub fn series_count(&self) -> usize {
        series_count_of(&self.keys)
    }
}

#[cfg(feature = "float")]
impl<const N: usize, const B: usize, const K: usize, const V: usize> ErasedHistogramVec
    for HistogramVec<N, B, K, V>
{
    fn bucket_count(&self) -> usize {
        B
    }
    fn bound(&self, b: usize) -> Value {
        Value::F64(self.bounds[b])
    }
    fn series_count(&self) -> usize {
        HistogramVec::series_count(self)
    }
    fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result {
        write_labels_at(&self.keys, s, out)
    }
    fn bucket(&self, s: usize, b: usize) -> u64 {
        self.buckets[s][b].load(Ordering::Relaxed)
    }
    fn total_count(&self, s: usize) -> u64 {
        self.counts[s].load(Ordering::Relaxed)
    }
    fn sum(&self, s: usize) -> Value {
        Value::F64(self.sums[s].load(Ordering::Relaxed))
    }
    #[cfg(feature = "consistent-histograms")]
    fn snapshot<'a>(
        &self,
        s: usize,
        buckets: &'a mut [u64],
    ) -> Result<HistogramSnapshot<'a>, HistogramSnapshotError> {
        let buckets = snapshot_bucket_prefix(buckets, B)?;
        critical_section::with(|_cs| {
            for (snapshot_bucket, bucket) in buckets.iter_mut().zip(&self.buckets[s]) {
                *snapshot_bucket = bucket.load(Ordering::Relaxed);
            }
            Ok(HistogramSnapshot {
                buckets,
                sum: Value::F64(self.sums[s].load(Ordering::Relaxed)),
                count: self.counts[s].load(Ordering::Relaxed),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "float")]
    use crate::histogram::Histogram;
    use crate::histogram::IntHistogram;

    static REASON: [&str; 1] = ["reason"];
    static PEER: [&str; 1] = ["peer"];

    fn labels_of<E: ErasedCounterVec>(v: &E, s: usize) -> heapless::String<64> {
        let mut out = heapless::String::<64>::new();
        v.write_labels(s, &mut out).unwrap();
        out
    }

    #[test]
    fn counter_vec_creates_and_increments_series() {
        let cv: CounterVec<4> = CounterVec::new(&REASON);
        cv.inc(&["timeout"]);
        cv.inc_by(&["timeout"], 2);
        cv.inc(&["auth_fail"]);

        assert_eq!(cv.series_count(), 2);
        assert_eq!(cv.with(&["timeout"]).get(), 3);
        assert_eq!(cv.with(&["auth_fail"]).get(), 1);
        assert_eq!(labels_of(&cv, 0), "reason=\"timeout\"");
    }

    #[test]
    #[should_panic(expected = "invalid label name")]
    fn counter_vec_rejects_invalid_label_names_at_runtime() {
        let _ = CounterVec::<1, 1>::new(&["not-valid"]);
    }

    #[test]
    #[should_panic(expected = "duplicate label name")]
    fn counter_vec_rejects_duplicate_label_names_at_runtime() {
        let _ = CounterVec::<1, 2>::new(&["kind", "kind"]);
    }

    #[test]
    fn cached_counter_series_can_be_reused_for_hot_path_updates() {
        let cv: CounterVec<4> = CounterVec::new(&REASON);
        let timeout = cv.with(&["timeout"]);

        timeout.inc();
        timeout.inc_by(4);

        assert_eq!(timeout.get(), 5);
        assert_eq!(cv.series_count(), 1);
    }

    #[test]
    fn counter_vec_drops_excess_series_but_keeps_existing_ones_updating() {
        let cv: CounterVec<2> = CounterVec::new(&REASON);
        cv.inc(&["a"]);
        cv.inc(&["b"]);
        let sink = cv.with(&["c"]); // capacity exhausted
        sink.inc_by(100);
        assert_eq!(sink.get(), 0);
        cv.inc(&["a"]); // existing series keeps updating
        assert_eq!(cv.series_count(), 2);
        assert_eq!(cv.with(&["a"]).get(), 2);
    }

    #[test]
    fn counter_vec_drops_oversized_label_values_without_affecting_existing_series() {
        let cv: CounterVec<4, 1, 20> = CounterVec::new(&REASON);
        cv.inc(&["ok"]);
        let sink = cv.with(&["this value is far too long to fit"]);
        sink.inc();
        assert_eq!(sink.get(), 0);
        assert_eq!(cv.series_count(), 1);
        assert_eq!(cv.with(&["ok"]).get(), 1);
    }

    #[test]
    fn gauge_vec_creates_and_sets_series() {
        let gv: GaugeVec<4> = GaugeVec::new(&PEER);
        gv.set(&["ap-1"], 10);
        gv.add(&["ap-1"], -3);
        gv.set(&["ap-2"], 5);
        assert_eq!(gv.with(&["ap-1"]).get(), 7);
        assert_eq!(gv.with(&["ap-2"]).get(), 5);
        assert_eq!(gv.series_count(), 2);
    }

    #[test]
    fn gauge_vec_returns_zero_reading_sink_when_full() {
        let gv: GaugeVec<1> = GaugeVec::new(&PEER);
        gv.set(&["ap-1"], 7);

        let sink = gv.with(&["ap-2"]);
        sink.set(99);
        sink.inc();

        assert_eq!(sink.get(), 0);
        assert_eq!(gv.with(&["ap-1"]).get(), 7);
        assert_eq!(gv.series_count(), 1);
    }

    #[cfg(feature = "float")]
    #[test]
    fn gauge_f64_vec_creates_and_sets_series() {
        let gv: GaugeF64Vec<4> = GaugeF64Vec::new(&PEER);
        gv.set(&["ap-1"], 10.5);
        gv.add(&["ap-1"], -3.0);
        gv.set(&["ap-2"], 5.25);

        let ap1 = gv.with(&["ap-1"]);
        ap1.sub(0.5);
        ap1.inc();
        ap1.dec();

        assert_eq!(ap1.get().to_bits(), 7.0_f64.to_bits());
        assert_eq!(gv.with(&["ap-2"]).get().to_bits(), 5.25_f64.to_bits());
        assert_eq!(gv.series_count(), 2);
    }

    #[cfg(feature = "float")]
    #[test]
    fn gauge_f64_vec_returns_zero_reading_sink_when_full() {
        let gv: GaugeF64Vec<1> = GaugeF64Vec::new(&PEER);
        gv.set(&["ap-1"], 7.0);

        let sink = gv.with(&["ap-2"]);
        sink.set(99.0);
        sink.add(1.0);

        assert_eq!(sink.get().to_bits(), 0.0_f64.to_bits());
        assert_eq!(gv.with(&["ap-1"]).get().to_bits(), 7.0_f64.to_bits());
        assert_eq!(gv.series_count(), 1);
    }

    #[test]
    fn int_histogram_vec_tracks_independent_series() {
        let hv: IntHistogramVec<4, 2> = IntHistogramVec::new(&PEER, &[100, 1000]);
        hv.observe(&["ap-1"], 50);
        hv.observe(&["ap-1"], 500);
        hv.observe(&["ap-2"], 2000);

        let ap1 = hv.with(&["ap-1"]);
        let ap2 = hv.with(&["ap-2"]);

        assert_eq!(hv.series_count(), 2);
        assert_eq!(hv.bounds(), &[100, 1000]);
        assert_eq!(ap1.bounds(), &[100, 1000]);
        assert_eq!(ap1.bucket(0), 1);
        assert_eq!(ap1.bucket(1), 1);
        assert_eq!(ap1.count(), 2);
        assert_eq!(ap1.sum(), 550);

        assert_eq!(ap2.bucket(0), 0);
        assert_eq!(ap2.bucket(1), 0);
        assert_eq!(ap2.count(), 1); // +Inf only
        assert_eq!(ap2.sum(), 2000);
    }

    fn observe_us<'a>(metric: impl Into<IntHistSeries<'a>>, v: u64) {
        metric.into().observe(v);
    }

    #[test]
    fn scalar_histogram_series_is_the_same_handle_as_a_bound_vec_series() {
        let scalar = IntHistogram::<2>::new(&[100, 1000]);
        let labeled: IntHistogramVec<4, 2> = IntHistogramVec::new(&PEER, &[100, 1000]);

        observe_us(&scalar, 50);
        observe_us(scalar.series(), 500);
        observe_us(labeled.with(&["ap-1"]), 50);

        assert_eq!(scalar.bucket(0), 1);
        assert_eq!(scalar.bucket(1), 1);
        assert_eq!(scalar.count(), 2);
        assert_eq!(scalar.sum(), 550);
        assert_eq!(scalar.series().sum(), 550);
        assert_eq!(labeled.with(&["ap-1"]).count(), 1);
        assert_eq!(labeled.with(&["ap-1"]).sum(), 50);
    }

    #[test]
    fn cached_histogram_series_can_be_reused_for_hot_path_observations() {
        let hv: IntHistogramVec<4, 2> = IntHistogramVec::new(&PEER, &[100, 1000]);
        let ap = hv.with(&["ap-1"]);

        ap.observe(50);
        ap.observe(500);

        assert_eq!(hv.series_count(), 1);
        assert_eq!(ap.bucket(0), 1);
        assert_eq!(ap.bucket(1), 1);
        assert_eq!(ap.count(), 2);
        assert_eq!(ap.sum(), 550);
    }

    #[test]
    fn int_histogram_vec_sink_discards_observations_when_full() {
        let hv: IntHistogramVec<1, 2> = IntHistogramVec::new(&PEER, &[100, 1000]);
        hv.observe(&["ap-1"], 50);

        let sink = hv.with(&["ap-2"]);
        sink.observe(500);

        assert_eq!(hv.series_count(), 1);
        assert_eq!(sink.bounds(), &[100, 1000]);
        assert_eq!(sink.bucket(0), 0);
        assert_eq!(sink.bucket(1), 0);
        assert_eq!(sink.count(), 0);
        assert_eq!(sink.sum(), 0);
        assert_eq!(hv.with(&["ap-1"]).count(), 1);
        assert_eq!(hv.with(&["ap-1"]).sum(), 50);
    }

    #[test]
    #[should_panic(expected = "histogram bounds must be strictly increasing")]
    fn int_histogram_vec_rejects_invalid_bounds() {
        let _ = IntHistogramVec::<1, 2>::new(&PEER, &[1000, 100]);
    }

    #[test]
    #[should_panic(expected = "histogram label name `le` is reserved")]
    fn int_histogram_vec_rejects_the_generated_bucket_label_name() {
        let _ = IntHistogramVec::<1, 1>::new(&["le"], &[100]);
    }

    #[test]
    #[should_panic(expected = "histogram vectors require at least one label")]
    fn int_histogram_vec_requires_at_least_one_label() {
        let _ = IntHistogramVec::<1, 1, 0>::new(&[], &[100]);
    }

    #[cfg(feature = "float")]
    #[test]
    fn float_histogram_series_is_the_same_handle_as_a_bound_vec_series() {
        fn observe<'a>(metric: impl Into<HistSeries<'a>>, v: f64) {
            metric.into().observe(v);
        }

        let scalar = Histogram::<2>::new(&[0.5, 5.0]);
        let labeled: HistogramVec<4, 2> = HistogramVec::new(&PEER, &[0.5, 5.0]);

        observe(&scalar, 0.25);
        observe(labeled.with(&["ap-1"]), 2.0);

        assert_eq!(scalar.count(), 1);
        assert_eq!(labeled.with(&["ap-1"]).count(), 1);
        assert_eq!(labeled.with(&["ap-1"]).sum().to_bits(), 2.0_f64.to_bits());
    }

    #[cfg(feature = "float")]
    #[test]
    fn histogram_vec_tracks_independent_series() {
        let hv: HistogramVec<4, 2> = HistogramVec::new(&PEER, &[0.5, 5.0]);
        hv.observe(&["ap-1"], 0.25);
        hv.observe(&["ap-1"], 2.0);
        let ap = hv.with(&["ap-1"]);
        assert_eq!(hv.bounds(), &[0.5, 5.0]);
        assert_eq!(ap.bounds(), &[0.5, 5.0]);
        assert_eq!(ap.bucket(0), 1);
        assert_eq!(ap.bucket(1), 1);
        assert_eq!(ap.count(), 2);
        assert_eq!(ap.sum().to_bits(), 2.25_f64.to_bits());
    }

    #[cfg(feature = "float")]
    #[test]
    fn histogram_vec_routes_nan_only_to_the_implicit_infinite_bucket() {
        let hv: HistogramVec<1, 2> = HistogramVec::new(&PEER, &[0.5, 5.0]);
        hv.observe(&["ap-1"], f64::NAN);

        let ap = hv.with(&["ap-1"]);
        assert_eq!(ap.bucket(0), 0);
        assert_eq!(ap.bucket(1), 0);
        assert_eq!(ap.count(), 1);
        assert!(ap.sum().is_nan());
    }

    #[cfg(feature = "float")]
    #[test]
    #[should_panic(expected = "histogram vectors require at least one label")]
    fn histogram_vec_requires_at_least_one_label() {
        let _ = HistogramVec::<1, 1, 0>::new(&[], &[0.5]);
    }

    #[cfg(feature = "float")]
    #[test]
    fn float_histogram_vec_sink_discards_observations_when_full() {
        let hv: HistogramVec<1, 2> = HistogramVec::new(&PEER, &[0.5, 5.0]);
        hv.observe(&["ap-1"], 0.25);

        let sink = hv.with(&["ap-2"]);
        sink.observe(2.0);

        assert_eq!(hv.series_count(), 1);
        assert_eq!(sink.count(), 0);
        assert_eq!(sink.sum().to_bits(), 0.0_f64.to_bits());
        assert_eq!(hv.with(&["ap-1"]).count(), 1);
        assert_eq!(hv.with(&["ap-1"]).sum().to_bits(), 0.25_f64.to_bits());
    }
}
