//! Rendering registered metrics into Prometheus text-exposition format.
//!
//! [`Renderer`] is a pull-based cursor: `embeprom` never calls into an async
//! writer itself (it stays `no_std` + sync), and the caller drives it with
//! [`Renderer::next_line`], awaiting between lines as needed to bridge to an
//! async sink such as picoserve's chunked response.

use core::fmt::{self, Write};

#[cfg(feature = "consistent-histograms")]
use crate::erased::HistogramSnapshotError;
use crate::erased::{ErasedHistogram, ErasedHistogramVec, MetricDesc, MetricRef};
use crate::escape::write_escaped_help;
use crate::registry::{GroupSnapshot, Registry};

/// The `Content-Type` value for Prometheus text exposition format.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// A failure while producing or writing Prometheus exposition text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    /// One complete line did not fit in the renderer's line buffer.
    LineTooLong {
        /// Exact number of bytes required for the line.
        required: usize,
        /// Configured line-buffer capacity.
        capacity: usize,
        /// Namespace of the metric whose line did not fit.
        namespace: &'static str,
        /// Name of the metric whose line did not fit.
        metric: &'static str,
    },
    /// A histogram has more finite buckets than the renderer can snapshot
    /// while `consistent-histograms` is enabled.
    HistogramCapacityExceeded {
        /// Number of finite buckets declared by the histogram.
        required: usize,
        /// Configured histogram snapshot capacity.
        capacity: usize,
        /// Namespace of the histogram.
        namespace: &'static str,
        /// Name of the histogram.
        metric: &'static str,
    },
    /// A type-erased metric implementation failed while formatting its data.
    Formatting,
    /// The [`fmt::Write`] sink passed to [`Renderer::render_to`] failed.
    Sink,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineTooLong {
                required,
                capacity,
                namespace,
                metric,
            } => {
                if namespace.is_empty() {
                    write!(
                        f,
                        "metric \"{metric}\" requires a {required}-byte render line, but the renderer capacity is {capacity} bytes"
                    )
                } else {
                    write!(
                        f,
                        "metric \"{namespace}_{metric}\" requires a {required}-byte render line, but the renderer capacity is {capacity} bytes"
                    )
                }
            }
            Self::HistogramCapacityExceeded {
                required,
                capacity,
                namespace,
                metric,
            } => {
                if namespace.is_empty() {
                    write!(
                        f,
                        "histogram \"{metric}\" has {required} finite buckets, but the renderer snapshot capacity is {capacity}"
                    )
                } else {
                    write!(
                        f,
                        "histogram \"{namespace}_{metric}\" has {required} finite buckets, but the renderer snapshot capacity is {capacity}"
                    )
                }
            }
            Self::Formatting => f.write_str("a metric failed while formatting its render line"),
            Self::Sink => f.write_str("the render output sink rejected a line"),
        }
    }
}

impl core::error::Error for RenderError {}

/// A fixed-capacity line buffer that continues counting after it fills. This
/// lets an overflow report the exact required capacity without rendering the
/// metric a second time (when its live value may already have changed).
struct LineBuffer<const N: usize> {
    text: heapless::String<N>,
    required: usize,
    overflowed: bool,
}

impl<const N: usize> LineBuffer<N> {
    const fn new() -> Self {
        Self {
            text: heapless::String::new(),
            required: 0,
            overflowed: false,
        }
    }

    fn clear(&mut self) {
        self.text.clear();
        self.required = 0;
        self.overflowed = false;
    }
}

impl<const N: usize> Write for LineBuffer<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.required = self.required.saturating_add(s.len());
        if !self.overflowed && self.text.push_str(s).is_err() {
            self.overflowed = true;
        }
        Ok(())
    }
}

/// Scratch state for one scalar histogram or one histogram-vector series.
/// It is zero-sized unless coherent histogram snapshots are enabled.
struct HistogramScratch<const N: usize> {
    #[cfg(feature = "consistent-histograms")]
    buckets: [u64; N],
    #[cfg(feature = "consistent-histograms")]
    sum: crate::Value,
    #[cfg(feature = "consistent-histograms")]
    count: u64,
}

impl<const N: usize> HistogramScratch<N> {
    const fn new() -> Self {
        Self {
            #[cfg(feature = "consistent-histograms")]
            buckets: [0; N],
            #[cfg(feature = "consistent-histograms")]
            sum: crate::Value::U64(0),
            #[cfg(feature = "consistent-histograms")]
            count: 0,
        }
    }

    #[cfg(feature = "consistent-histograms")]
    fn capture_scalar(
        &mut self,
        histogram: &dyn ErasedHistogram,
    ) -> Result<(), HistogramSnapshotError> {
        let snapshot = histogram.snapshot(&mut self.buckets)?;
        debug_assert_eq!(snapshot.buckets.len(), histogram.bucket_count());
        self.sum = snapshot.sum;
        self.count = snapshot.count;
        Ok(())
    }

    #[cfg(feature = "consistent-histograms")]
    fn capture_vec(
        &mut self,
        histogram: &dyn ErasedHistogramVec,
        series: usize,
    ) -> Result<(), HistogramSnapshotError> {
        let snapshot = histogram.snapshot(series, &mut self.buckets)?;
        debug_assert_eq!(snapshot.buckets.len(), histogram.bucket_count());
        self.sum = snapshot.sum;
        self.count = snapshot.count;
        Ok(())
    }

    #[cfg(feature = "consistent-histograms")]
    fn capture(
        &mut self,
        metric: &MetricRef,
        series: Option<usize>,
    ) -> Result<(), HistogramSnapshotError> {
        match metric {
            MetricRef::Histogram { h } => {
                debug_assert!(series.is_none());
                self.capture_scalar(*h)
            }
            MetricRef::HistogramVec { h } => self.capture_vec(
                *h,
                series.expect("embeprom: histogram-vector snapshot requires a series"),
            ),
            _ => unreachable!("embeprom: histogram snapshot requested for non-histogram metric"),
        }
    }

    fn scalar_bucket(&self, histogram: &dyn ErasedHistogram, bucket: usize) -> u64 {
        #[cfg(feature = "consistent-histograms")]
        {
            let _ = histogram;
            self.buckets[bucket]
        }
        #[cfg(not(feature = "consistent-histograms"))]
        {
            let _ = self;
            histogram.bucket(bucket)
        }
    }

    fn vec_bucket(&self, histogram: &dyn ErasedHistogramVec, series: usize, bucket: usize) -> u64 {
        #[cfg(feature = "consistent-histograms")]
        {
            let _ = (histogram, series);
            self.buckets[bucket]
        }
        #[cfg(not(feature = "consistent-histograms"))]
        {
            let _ = self;
            histogram.bucket(series, bucket)
        }
    }

    fn scalar_sum(&self, histogram: &dyn ErasedHistogram) -> crate::Value {
        #[cfg(feature = "consistent-histograms")]
        {
            let _ = histogram;
            self.sum
        }
        #[cfg(not(feature = "consistent-histograms"))]
        {
            let _ = self;
            histogram.sum()
        }
    }

    fn vec_sum(&self, histogram: &dyn ErasedHistogramVec, series: usize) -> crate::Value {
        #[cfg(feature = "consistent-histograms")]
        {
            let _ = (histogram, series);
            self.sum
        }
        #[cfg(not(feature = "consistent-histograms"))]
        {
            let _ = self;
            histogram.sum(series)
        }
    }

    fn scalar_count(&self, histogram: &dyn ErasedHistogram) -> u64 {
        #[cfg(feature = "consistent-histograms")]
        {
            let _ = histogram;
            self.count
        }
        #[cfg(not(feature = "consistent-histograms"))]
        {
            let _ = self;
            histogram.total_count()
        }
    }

    fn vec_count(&self, histogram: &dyn ErasedHistogramVec, series: usize) -> u64 {
        #[cfg(feature = "consistent-histograms")]
        {
            let _ = (histogram, series);
            self.count
        }
        #[cfg(not(feature = "consistent-histograms"))]
        {
            let _ = self;
            histogram.total_count(series)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Help,
    Type,
    /// A scalar `Counter`/`Gauge` sample.
    Sample,
    /// One series of a `CounterVec`/`GaugeVec`, by index.
    Series(usize),
    /// One finite bucket of a histogram (`s` is `None` for a plain
    /// [`Histogram`](crate::histogram::Histogram), `Some(series)` for a vec).
    Bucket {
        s: Option<usize>,
        b: usize,
    },
    BucketInf {
        s: Option<usize>,
    },
    Sum {
        s: Option<usize>,
    },
    Count {
        s: Option<usize>,
    },
    /// Marks that the current metric is fully rendered; advance to the next
    /// metric (or group) on the next call. Never itself rendered.
    Advance,
}

fn first_series_step(series_count: usize) -> Step {
    if series_count == 0 {
        Step::Advance
    } else {
        Step::Series(0)
    }
}

fn first_hist_step(s: Option<usize>, bucket_count: usize) -> Step {
    if bucket_count == 0 {
        Step::BucketInf { s }
    } else {
        Step::Bucket { s, b: 0 }
    }
}

fn type_word(m: &MetricRef) -> &'static str {
    match m {
        MetricRef::Counter(_) | MetricRef::CounterVec(_) => "counter",
        MetricRef::Gauge(_) | MetricRef::GaugeVec(_) => "gauge",
        MetricRef::Histogram { .. } | MetricRef::HistogramVec { .. } => "histogram",
    }
}

fn write_full_name(out: &mut dyn Write, desc: &MetricDesc, suffix: &str) -> fmt::Result {
    if !desc.namespace.is_empty() {
        out.write_str(desc.namespace)?;
        out.write_char('_')?;
    }
    out.write_str(desc.name)?;
    out.write_str(suffix)
}

/// Determine the next step to attempt after `step`, given the metric being
/// rendered. Only ever called with a step that was just successfully
/// rendered (never [`Step::Advance`]).
fn next_step(step: Step, desc: &MetricDesc) -> Step {
    match step {
        Step::Help => Step::Type,
        Step::Type => match &desc.metric {
            MetricRef::Counter(_) | MetricRef::Gauge(_) => Step::Sample,
            MetricRef::CounterVec(cv) => first_series_step(cv.series_count()),
            MetricRef::GaugeVec(gv) => first_series_step(gv.series_count()),
            MetricRef::Histogram { h, .. } => first_hist_step(None, h.bucket_count()),
            MetricRef::HistogramVec { h, .. } => {
                if h.series_count() == 0 {
                    Step::Advance
                } else {
                    first_hist_step(Some(0), h.bucket_count())
                }
            }
        },
        Step::Sample => Step::Advance,
        Step::Series(i) => {
            let n = match &desc.metric {
                MetricRef::CounterVec(cv) => cv.series_count(),
                MetricRef::GaugeVec(gv) => gv.series_count(),
                _ => 0,
            };
            if i + 1 < n {
                Step::Series(i + 1)
            } else {
                Step::Advance
            }
        }
        Step::Bucket { s, b } => {
            let bucket_count = match &desc.metric {
                MetricRef::Histogram { h, .. } => h.bucket_count(),
                MetricRef::HistogramVec { h, .. } => h.bucket_count(),
                _ => 0,
            };
            if b + 1 < bucket_count {
                Step::Bucket { s, b: b + 1 }
            } else {
                Step::BucketInf { s }
            }
        }
        Step::BucketInf { s } => Step::Sum { s },
        Step::Sum { s } => Step::Count { s },
        Step::Count { s } => match s {
            None => Step::Advance,
            Some(idx) => {
                let (n, bucket_count) = match &desc.metric {
                    MetricRef::HistogramVec { h, .. } => (h.series_count(), h.bucket_count()),
                    _ => (0, 0),
                };
                if idx + 1 < n {
                    first_hist_step(Some(idx + 1), bucket_count)
                } else {
                    Step::Advance
                }
            }
        },
        Step::Advance => unreachable!("embeprom: next_step called with Step::Advance"),
    }
}

fn render_bucket<const H: usize>(
    line: &mut dyn Write,
    desc: &MetricDesc,
    snapshot: &HistogramScratch<H>,
    histogram_cumulative: &mut u64,
    series: Option<usize>,
    bucket: usize,
) -> fmt::Result {
    write_full_name(line, desc, "_bucket")?;
    line.write_char('{')?;
    let (bound, bucket_delta) = match &desc.metric {
        MetricRef::Histogram { h } => {
            debug_assert!(series.is_none());
            (h.bound(bucket), snapshot.scalar_bucket(*h, bucket))
        }
        MetricRef::HistogramVec { h } => {
            let series = series.expect("embeprom: HistogramVec Bucket step requires Some(series)");
            h.write_labels(series, line)?;
            line.write_char(',')?;
            (h.bound(bucket), snapshot.vec_bucket(*h, series, bucket))
        }
        _ => unreachable!("embeprom: Bucket step on non-histogram metric"),
    };
    *histogram_cumulative = if bucket == 0 {
        bucket_delta
    } else {
        histogram_cumulative.wrapping_add(bucket_delta)
    };
    line.write_str("le=\"")?;
    bound.write_prom(line)?;
    line.write_str("\"} ")?;
    write!(line, "{histogram_cumulative}")?;
    line.write_char('\n')
}

fn render_infinite_bucket<const H: usize>(
    line: &mut dyn Write,
    desc: &MetricDesc,
    snapshot: &HistogramScratch<H>,
    series: Option<usize>,
) -> fmt::Result {
    write_full_name(line, desc, "_bucket")?;
    line.write_char('{')?;
    let total = match &desc.metric {
        MetricRef::Histogram { h, .. } => {
            debug_assert!(series.is_none());
            snapshot.scalar_count(*h)
        }
        MetricRef::HistogramVec { h, .. } => {
            let series =
                series.expect("embeprom: HistogramVec BucketInf step requires Some(series)");
            h.write_labels(series, line)?;
            line.write_char(',')?;
            snapshot.vec_count(*h, series)
        }
        _ => unreachable!("embeprom: BucketInf step on non-histogram metric"),
    };
    line.write_str("le=\"+Inf\"} ")?;
    write!(line, "{total}")?;
    line.write_char('\n')
}

fn render_histogram_sum<const H: usize>(
    line: &mut dyn Write,
    desc: &MetricDesc,
    snapshot: &HistogramScratch<H>,
    series: Option<usize>,
) -> fmt::Result {
    write_full_name(line, desc, "_sum")?;
    let value = match &desc.metric {
        MetricRef::Histogram { h, .. } => {
            debug_assert!(series.is_none());
            snapshot.scalar_sum(*h)
        }
        MetricRef::HistogramVec { h, .. } => {
            let series = series.expect("embeprom: HistogramVec Sum step requires Some(series)");
            line.write_char('{')?;
            h.write_labels(series, line)?;
            line.write_char('}')?;
            snapshot.vec_sum(*h, series)
        }
        _ => unreachable!("embeprom: Sum step on non-histogram metric"),
    };
    line.write_char(' ')?;
    value.write_prom(line)?;
    line.write_char('\n')
}

fn render_histogram_count<const H: usize>(
    line: &mut dyn Write,
    desc: &MetricDesc,
    snapshot: &HistogramScratch<H>,
    series: Option<usize>,
) -> fmt::Result {
    write_full_name(line, desc, "_count")?;
    let total = match &desc.metric {
        MetricRef::Histogram { h, .. } => {
            debug_assert!(series.is_none());
            snapshot.scalar_count(*h)
        }
        MetricRef::HistogramVec { h, .. } => {
            let series = series.expect("embeprom: HistogramVec Count step requires Some(series)");
            line.write_char('{')?;
            h.write_labels(series, line)?;
            line.write_char('}')?;
            snapshot.vec_count(*h, series)
        }
        _ => unreachable!("embeprom: Count step on non-histogram metric"),
    };
    line.write_char(' ')?;
    write!(line, "{total}")?;
    line.write_char('\n')
}

/// Render `step` for `desc` into `line`. `line` is not cleared here; the
/// caller clears it before calling.
fn render_step<const H: usize>(
    line: &mut dyn Write,
    step: Step,
    desc: &MetricDesc,
    snapshot: &HistogramScratch<H>,
    histogram_cumulative: &mut u64,
) -> fmt::Result {
    match step {
        Step::Help => {
            line.write_str("# HELP ")?;
            write_full_name(line, desc, "")?;
            line.write_char(' ')?;
            write_escaped_help(line, desc.help)?;
            line.write_char('\n')
        }
        Step::Type => {
            line.write_str("# TYPE ")?;
            write_full_name(line, desc, "")?;
            line.write_char(' ')?;
            line.write_str(type_word(&desc.metric))?;
            line.write_char('\n')
        }
        Step::Sample => {
            write_full_name(line, desc, "")?;
            line.write_char(' ')?;
            match &desc.metric {
                MetricRef::Counter(c) => write!(line, "{}", c.get())?,
                MetricRef::Gauge(v) => v.write_prom(line)?,
                _ => unreachable!("embeprom: Sample step on non-scalar metric"),
            }
            line.write_char('\n')
        }
        Step::Series(i) => {
            write_full_name(line, desc, "")?;
            line.write_char('{')?;
            match &desc.metric {
                MetricRef::CounterVec(cv) => {
                    cv.write_labels(i, line)?;
                    line.write_str("} ")?;
                    write!(line, "{}", cv.value(i))?;
                }
                MetricRef::GaugeVec(gv) => {
                    gv.write_labels(i, line)?;
                    line.write_str("} ")?;
                    gv.value(i).write_prom(line)?;
                }
                _ => unreachable!("embeprom: Series step on non-vec metric"),
            }
            line.write_char('\n')
        }
        Step::Bucket { s, b } => render_bucket(line, desc, snapshot, histogram_cumulative, s, b),
        Step::BucketInf { s } => render_infinite_bucket(line, desc, snapshot, s),
        Step::Sum { s } => render_histogram_sum(line, desc, snapshot, s),
        Step::Count { s } => render_histogram_count(line, desc, snapshot, s),
        Step::Advance => unreachable!("embeprom: render_step called with Step::Advance"),
    }
}

/// A pull-based cursor over a snapshot of registered metric groups, producing
/// Prometheus text-exposition-format output one complete line at a time,
/// without requiring the whole output to fit in memory at once.
///
/// `N` is the group snapshot capacity, `L` is the maximum rendered line
/// length, and `H` is the finite-bucket scratch capacity used by
/// `consistent-histograms`. Callers can tune each independently instead of
/// paying a crate-wide memory cost. Capacity failures are explicit and
/// terminal.
pub struct Renderer<
    const N: usize = { crate::config::MAX_GROUPS },
    const L: usize = { crate::config::MAX_LINE },
    const H: usize = { crate::config::MAX_HISTOGRAM_BUCKETS },
> {
    groups: GroupSnapshot<N>,
    g: usize,
    m: usize,
    step: Step,
    line: LineBuffer<L>,
    histogram: HistogramScratch<H>,
    histogram_cumulative: u64,
    failed: Option<RenderError>,
}

impl<const N: usize, const L: usize, const H: usize> Renderer<N, L, H> {
    fn from_snapshot(groups: GroupSnapshot<N>) -> Self {
        Self {
            groups,
            g: 0,
            m: 0,
            step: Step::Help,
            line: LineBuffer::new(),
            histogram: HistogramScratch::new(),
            histogram_cumulative: 0,
            failed: None,
        }
    }

    /// Build a renderer over a snapshot of the given [`Registry`].
    ///
    /// ```
    /// let registry: embeprom::Registry<4> = embeprom::Registry::new();
    /// let renderer = embeprom::Renderer::<4>::from_registry(&registry);
    /// assert!(renderer.is_done());
    /// ```
    ///
    /// The registry may have a smaller capacity than the renderer, but not a
    /// larger one. A capacity mismatch is rejected at compile time:
    ///
    /// ```compile_fail
    /// let registry: embeprom::Registry<2> = embeprom::Registry::new();
    /// let _ = embeprom::Renderer::<1>::from_registry(&registry);
    /// ```
    pub fn from_registry<const M: usize>(registry: &Registry<M>) -> Self {
        const {
            assert!(
                M <= N,
                "embeprom: registry capacity exceeds renderer capacity"
            );
        }

        let source = registry.snapshot();
        let mut groups = GroupSnapshot::<N>::new();
        for group in source {
            if groups.push(group).is_err() {
                unreachable!("embeprom: registry capacity was checked at compile time");
            }
        }
        Self::from_snapshot(groups)
    }

    /// Whether every group has been rendered successfully. A renderer in a
    /// terminal error state is not considered done.
    pub fn is_done(&self) -> bool {
        self.failed.is_none() && self.g >= self.groups.len()
    }

    /// The next complete output line, including its trailing `\n`, or
    /// `Ok(None)` once every registered group has been fully rendered.
    ///
    /// An error is terminal and is returned again by every later call. In
    /// particular, a line that exceeds capacity `L`, or a histogram that
    /// exceeds snapshot capacity `H`, is never silently skipped and rendering
    /// cannot continue with a partial metric family.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::LineTooLong`] or
    /// [`RenderError::HistogramCapacityExceeded`] when a configured capacity
    /// is insufficient, and [`RenderError::Formatting`] if a type-erased
    /// metric cannot format its value.
    pub fn next_line(&mut self) -> Result<Option<&str>, RenderError> {
        if let Some(error) = self.failed {
            return Err(error);
        }

        loop {
            if self.g >= self.groups.len() {
                return Ok(None);
            }
            let group = self.groups[self.g];
            if self.m >= group.len() {
                self.g += 1;
                self.m = 0;
                self.step = Step::Help;
                continue;
            }
            let Some(desc) = group.get(self.m) else {
                self.m += 1;
                self.step = Step::Help;
                continue;
            };
            if matches!(self.step, Step::Advance) {
                self.m += 1;
                self.step = Step::Help;
                continue;
            }

            let cur_step = self.step;

            #[cfg(feature = "consistent-histograms")]
            {
                let bucket_count = match &desc.metric {
                    MetricRef::Histogram { h } => Some(h.bucket_count()),
                    MetricRef::HistogramVec { h } => Some(h.bucket_count()),
                    _ => None,
                };
                if let Some(required) =
                    bucket_count.filter(|required| matches!(cur_step, Step::Help) && *required > H)
                {
                    let error = RenderError::HistogramCapacityExceeded {
                        required,
                        capacity: H,
                        namespace: desc.namespace,
                        metric: desc.name,
                    };
                    self.failed = Some(error);
                    return Err(error);
                }

                let capture = match cur_step {
                    Step::Bucket { s, b: 0 } => self.histogram.capture(&desc.metric, s),
                    Step::BucketInf { s } if bucket_count == Some(0) => {
                        self.histogram.capture(&desc.metric, s)
                    }
                    _ => Ok(()),
                };
                if let Err(buffer) = capture {
                    let error = RenderError::HistogramCapacityExceeded {
                        required: buffer.required,
                        capacity: buffer.capacity,
                        namespace: desc.namespace,
                        metric: desc.name,
                    };
                    self.failed = Some(error);
                    return Err(error);
                }
            }

            self.line.clear();
            if render_step(
                &mut self.line,
                cur_step,
                &desc,
                &self.histogram,
                &mut self.histogram_cumulative,
            )
            .is_err()
            {
                self.failed = Some(RenderError::Formatting);
                return Err(RenderError::Formatting);
            }
            if self.line.overflowed {
                let error = RenderError::LineTooLong {
                    required: self.line.required,
                    capacity: L,
                    namespace: desc.namespace,
                    metric: desc.name,
                };
                self.failed = Some(error);
                return Err(error);
            }

            self.step = next_step(cur_step, &desc);
            return Ok(Some(self.line.text.as_str()));
        }
    }

    /// Drain this renderer into `w`. A render-capacity, metric-formatting, or
    /// sink failure is terminal and leaves the renderer in its error state.
    ///
    /// # Errors
    ///
    /// Returns the rendering failures documented by [`Self::next_line`], or
    /// [`RenderError::Sink`] if `w` rejects a rendered line.
    pub fn render_to<W: Write>(&mut self, w: &mut W) -> Result<(), RenderError> {
        while let Some(line) = self.next_line()? {
            if w.write_str(line).is_err() {
                self.failed = Some(RenderError::Sink);
                return Err(RenderError::Sink);
            }
        }
        Ok(())
    }
}

impl<const L: usize, const H: usize> Renderer<{ crate::config::MAX_GROUPS }, L, H> {
    /// Build a renderer with line capacity `L` over a snapshot of the global
    /// registry.
    pub fn from_global() -> Self {
        Self::from_snapshot(crate::registry::snapshot())
    }
}

impl Renderer {
    /// Build a renderer with the default line capacity over a snapshot of the
    /// global registry.
    pub fn new() -> Self {
        Self::from_global()
    }

    /// Build a renderer with caller-selected line capacity `L` over a snapshot
    /// of the global registry.
    ///
    /// ```
    /// let _: embeprom::Renderer<{ embeprom::MAX_GROUPS }, 512> =
    ///     embeprom::Renderer::with_line_capacity::<512>();
    /// ```
    pub fn with_line_capacity<const L: usize>()
    -> Renderer<{ crate::config::MAX_GROUPS }, L, { crate::config::MAX_HISTOGRAM_BUCKETS }> {
        Renderer::from_global()
    }

    /// Build a renderer with caller-selected line capacity `L` and coherent
    /// histogram snapshot capacity `H` over the global registry.
    ///
    /// ```
    /// let _: embeprom::Renderer<{ embeprom::MAX_GROUPS }, 512, 32> =
    ///     embeprom::Renderer::with_capacities::<512, 32>();
    /// ```
    pub fn with_capacities<const L: usize, const H: usize>()
    -> Renderer<{ crate::config::MAX_GROUPS }, L, H> {
        Renderer::from_global()
    }
}

impl<const L: usize, const H: usize> Default for Renderer<{ crate::config::MAX_GROUPS }, L, H> {
    fn default() -> Self {
        Self::from_global()
    }
}

/// Render the global registry's metrics into `w`. This is a convenience for
/// synchronous sinks; asynchronous sinks can await between
/// [`Renderer::next_line`] calls.
///
/// # Errors
///
/// Returns a [`RenderError`] if rendering exceeds a configured capacity, a
/// metric cannot format its value, or `w` rejects a rendered line.
pub fn write_all<W: Write>(w: &mut W) -> Result<(), RenderError> {
    Renderer::new().render_to(w)
}

/// The byte length of a full render pass over the global registry.
///
/// This does a complete, discarded render, so it costs as much as an actual
/// scrape and the returned value can go stale immediately if a metric
/// changes afterwards. It exists only for integrations that need
/// `Content-Length` up front; prefer a chunked/streaming response
/// driven by [`Renderer::next_line`] wherever possible.
///
/// # Errors
///
/// Returns a [`RenderError`] if the discarded render exceeds a configured
/// capacity or a metric cannot format its value.
pub fn rendered_len() -> Result<usize, RenderError> {
    rendered_len_from(Renderer::new())
}

fn rendered_len_from<const N: usize, const L: usize, const H: usize>(
    mut renderer: Renderer<N, L, H>,
) -> Result<usize, RenderError> {
    struct ByteCount(usize);
    impl Write for ByteCount {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            self.0 += s.len();
            Ok(())
        }
    }
    let mut counter = ByteCount(0);
    renderer.render_to(&mut counter)?;
    Ok(counter.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counter::Counter;
    use crate::erased::{
        ErasedCounterVec, ErasedHistogram, ErasedHistogramVec, MetricDesc, MetricGroup, MetricRef,
    };
    use crate::gauge::Gauge;
    use crate::histogram::IntHistogram;
    use crate::value::Value;
    #[cfg(feature = "consistent-histograms")]
    use crate::vec::IntHistogramVec;

    static RENDERED_LEN_REGISTRY: Registry<1> = Registry::new();

    mod rendered_len_metrics {
        crate::metrics! {
            registry = super::RENDERED_LEN_REGISTRY;
            namespace = "rendered_len_test";

            /// Total test requests.
            requests: Counter,
        }
    }

    struct FixtureCounterVec {
        labels: [&'static str; 2],
        values: [Counter; 2],
    }

    impl ErasedCounterVec for FixtureCounterVec {
        fn series_count(&self) -> usize {
            2
        }
        fn write_labels(&self, s: usize, out: &mut dyn fmt::Write) -> fmt::Result {
            write!(out, "reason=\"{}\"", self.labels[s])
        }
        fn value(&self, s: usize) -> u64 {
            self.values[s].get()
        }
    }

    struct FixtureHistVec {
        bounds: &'static [u64],
        label: &'static str,
        buckets: [u64; 2],
        total_count: u64,
        total_sum: u64,
    }

    impl ErasedHistogramVec for FixtureHistVec {
        fn bucket_count(&self) -> usize {
            self.bounds.len()
        }
        fn bound(&self, b: usize) -> Value {
            Value::U64(self.bounds[b])
        }
        fn series_count(&self) -> usize {
            1
        }
        fn write_labels(&self, _s: usize, out: &mut dyn fmt::Write) -> fmt::Result {
            write!(out, "peer=\"{}\"", self.label)
        }
        fn bucket(&self, _s: usize, b: usize) -> u64 {
            self.buckets[b]
        }
        fn total_count(&self, _s: usize) -> u64 {
            self.total_count
        }
        fn sum(&self, _s: usize) -> Value {
            Value::U64(self.total_sum)
        }
        #[cfg(feature = "consistent-histograms")]
        fn snapshot<'a>(
            &self,
            _s: usize,
            buckets: &'a mut [u64],
        ) -> Result<crate::HistogramSnapshot<'a>, crate::HistogramSnapshotError> {
            let buckets = crate::erased::snapshot_bucket_prefix(buckets, self.buckets.len())?;
            buckets.copy_from_slice(&self.buckets);
            Ok(crate::HistogramSnapshot {
                buckets,
                sum: Value::U64(self.total_sum),
                count: self.total_count,
            })
        }
    }

    struct Fixture {
        requests: Counter,
        rssi: Gauge,
        disconnects: FixtureCounterVec,
        latency: IntHistogram<4>,
        peer_latency: FixtureHistVec,
    }

    impl MetricGroup for Fixture {
        fn group_name(&self) -> &'static str {
            "wifi"
        }

        fn len(&self) -> usize {
            5
        }

        fn get(&self, index: usize) -> Option<MetricDesc<'_>> {
            Some(match index {
                0 => MetricDesc {
                    namespace: "wifi",
                    name: "packets_sent",
                    help: "Total Wi-Fi frames transmitted.",
                    metric: MetricRef::Counter(&self.requests),
                },
                1 => MetricDesc {
                    namespace: "wifi",
                    name: "rssi_dbm",
                    help: "Last measured RSSI in dBm.",
                    metric: MetricRef::Gauge(Value::I64(self.rssi.get())),
                },
                2 => MetricDesc {
                    namespace: "wifi",
                    name: "disconnects_total",
                    help: "Disconnects, by reason.",
                    metric: MetricRef::CounterVec(&self.disconnects),
                },
                3 => MetricDesc {
                    namespace: "wifi",
                    name: "tx_latency_us",
                    help: "TX completion latency in microseconds.",
                    metric: MetricRef::Histogram { h: &self.latency },
                },
                4 => MetricDesc {
                    namespace: "wifi",
                    name: "peer_latency_us",
                    help: "Per-peer latency.",
                    metric: MetricRef::HistogramVec {
                        h: &self.peer_latency,
                    },
                },
                _ => return None,
            })
        }
    }

    static GOLDEN_FIXTURE_A: Fixture = Fixture {
        requests: Counter::new(),
        rssi: Gauge::new(0),
        disconnects: FixtureCounterVec {
            labels: ["beacon_timeout", "auth_fail"],
            values: [Counter::new(), Counter::new()],
        },
        latency: IntHistogram::new(&[100, 500, 1000, 5000]),
        peer_latency: FixtureHistVec {
            bounds: &[10, 50],
            label: "ap-1",
            buckets: [2, 3],
            total_count: 6,
            total_sum: 123,
        },
    };

    static GOLDEN_FIXTURE_B: Fixture = Fixture {
        requests: Counter::new(),
        rssi: Gauge::new(0),
        disconnects: FixtureCounterVec {
            labels: ["beacon_timeout", "auth_fail"],
            values: [Counter::new(), Counter::new()],
        },
        latency: IntHistogram::new(&[100, 500, 1000, 5000]),
        peer_latency: FixtureHistVec {
            bounds: &[10, 50],
            label: "ap-1",
            buckets: [2, 3],
            total_count: 6,
            total_sum: 123,
        },
    };

    const EXPECTED: &str = "\
# HELP wifi_packets_sent Total Wi-Fi frames transmitted.
# TYPE wifi_packets_sent counter
wifi_packets_sent 1843
# HELP wifi_rssi_dbm Last measured RSSI in dBm.
# TYPE wifi_rssi_dbm gauge
wifi_rssi_dbm -67
# HELP wifi_disconnects_total Disconnects, by reason.
# TYPE wifi_disconnects_total counter
wifi_disconnects_total{reason=\"beacon_timeout\"} 3
wifi_disconnects_total{reason=\"auth_fail\"} 1
# HELP wifi_tx_latency_us TX completion latency in microseconds.
# TYPE wifi_tx_latency_us histogram
wifi_tx_latency_us_bucket{le=\"100\"} 1
wifi_tx_latency_us_bucket{le=\"500\"} 3
wifi_tx_latency_us_bucket{le=\"1000\"} 4
wifi_tx_latency_us_bucket{le=\"5000\"} 6
wifi_tx_latency_us_bucket{le=\"+Inf\"} 7
wifi_tx_latency_us_sum 21969
wifi_tx_latency_us_count 7
# HELP wifi_peer_latency_us Per-peer latency.
# TYPE wifi_peer_latency_us histogram
wifi_peer_latency_us_bucket{peer=\"ap-1\",le=\"10\"} 2
wifi_peer_latency_us_bucket{peer=\"ap-1\",le=\"50\"} 5
wifi_peer_latency_us_bucket{peer=\"ap-1\",le=\"+Inf\"} 6
wifi_peer_latency_us_sum{peer=\"ap-1\"} 123
wifi_peer_latency_us_count{peer=\"ap-1\"} 6
";

    fn seed_golden_fixture(f: &Fixture) {
        f.requests.inc_by(1843);
        f.rssi.set(-67);
        f.disconnects.values[0].inc_by(3);
        f.disconnects.values[1].inc_by(1);
        for v in [12, 480, 480, 999, 4999, 4999, 10_000] {
            f.latency.observe(v);
        }
    }

    #[test]
    fn golden_output_covers_every_metric_kind() {
        seed_golden_fixture(&GOLDEN_FIXTURE_A);

        let registry = Registry::<1>::new();
        registry.register(&GOLDEN_FIXTURE_A);
        let mut out = heapless::String::<4096>::new();
        Renderer::<4>::from_registry(&registry)
            .render_to(&mut out)
            .unwrap();

        assert_eq!(out.as_str(), EXPECTED);
    }

    #[test]
    fn line_cursor_reassembles_the_golden_output() {
        seed_golden_fixture(&GOLDEN_FIXTURE_B);

        let registry = Registry::<1>::new();
        registry.register(&GOLDEN_FIXTURE_B);
        let mut renderer = Renderer::<4>::from_registry(&registry);
        let mut assembled = heapless::String::<4096>::new();
        while let Some(line) = renderer.next_line().unwrap() {
            assert!(line.ends_with('\n'));
            assembled.push_str(line).unwrap();
        }
        assert!(renderer.is_done());
        assert_eq!(assembled.as_str(), EXPECTED);
    }

    #[test]
    fn empty_registry_renders_nothing() {
        let registry = Registry::<1>::new();
        let mut r: Renderer<4> = Renderer::from_registry(&registry);
        assert!(r.is_done());
        assert_eq!(r.next_line(), Ok(None));
    }

    #[test]
    fn from_registry_accepts_a_smaller_registry_capacity() {
        let registry = Registry::<2>::new();
        registry.register(&GOLDEN_FIXTURE_A);
        registry.register(&GOLDEN_FIXTURE_B);

        let renderer = Renderer::<4>::from_registry(&registry);
        assert_eq!(renderer.groups.len(), 2);
        assert!(core::ptr::addr_eq(
            renderer.groups[0],
            &raw const GOLDEN_FIXTURE_A
        ));
        assert!(core::ptr::addr_eq(
            renderer.groups[1],
            &raw const GOLDEN_FIXTURE_B
        ));
    }

    #[test]
    fn line_overflow_is_exact_explicit_and_terminal() {
        const FIRST_LINE: &str = "# HELP wifi_packets_sent Total Wi-Fi frames transmitted.\n";
        let registry = Registry::<1>::new();
        registry.register(&GOLDEN_FIXTURE_A);
        let mut renderer = Renderer::<1, 16>::from_registry(&registry);
        let expected = RenderError::LineTooLong {
            required: FIRST_LINE.len(),
            capacity: 16,
            namespace: "wifi",
            metric: "packets_sent",
        };

        assert_eq!(renderer.next_line(), Err(expected));
        assert_eq!(renderer.next_line(), Err(expected));
        assert!(!renderer.is_done());
    }

    #[test]
    fn exact_line_capacity_succeeds() {
        const FIRST_LINE: &str = "# HELP wifi_packets_sent Total Wi-Fi frames transmitted.\n";
        let registry = Registry::<1>::new();
        registry.register(&GOLDEN_FIXTURE_A);
        let mut renderer = Renderer::<1, { FIRST_LINE.len() }>::from_registry(&registry);
        assert_eq!(renderer.next_line(), Ok(Some(FIRST_LINE)));
    }

    #[test]
    fn sink_failure_is_terminal() {
        struct Reject;
        impl Write for Reject {
            fn write_str(&mut self, _s: &str) -> fmt::Result {
                Err(fmt::Error)
            }
        }

        let registry = Registry::<1>::new();
        registry.register(&GOLDEN_FIXTURE_A);
        let mut renderer = Renderer::<1>::from_registry(&registry);
        assert_eq!(renderer.render_to(&mut Reject), Err(RenderError::Sink));
        assert_eq!(renderer.next_line(), Err(RenderError::Sink));
        assert!(!renderer.is_done());
    }

    struct CountingHistogram {
        bucket_reads: portable_atomic::AtomicU64,
    }

    impl ErasedHistogram for CountingHistogram {
        fn bucket_count(&self) -> usize {
            3
        }

        fn bound(&self, b: usize) -> Value {
            Value::U64((b as u64 + 1) * 10)
        }

        fn bucket(&self, b: usize) -> u64 {
            self.bucket_reads
                .fetch_add(1, portable_atomic::Ordering::Relaxed);
            b as u64 + 1
        }

        fn total_count(&self) -> u64 {
            6
        }

        fn sum(&self) -> Value {
            Value::U64(0)
        }

        #[cfg(feature = "consistent-histograms")]
        fn snapshot<'a>(
            &self,
            buckets: &'a mut [u64],
        ) -> Result<crate::HistogramSnapshot<'a>, crate::HistogramSnapshotError> {
            let buckets = crate::erased::snapshot_bucket_prefix(buckets, 3)?;
            for (b, bucket) in buckets.iter_mut().enumerate() {
                *bucket = self.bucket(b);
            }
            Ok(crate::HistogramSnapshot {
                buckets,
                sum: Value::U64(0),
                count: 6,
            })
        }
    }

    struct CountingHistogramGroup {
        histogram: CountingHistogram,
    }

    impl MetricGroup for CountingHistogramGroup {
        fn group_name(&self) -> &'static str {
            "linear"
        }

        fn len(&self) -> usize {
            1
        }

        fn get(&self, index: usize) -> Option<MetricDesc<'_>> {
            (index == 0).then_some(MetricDesc {
                namespace: "linear",
                name: "latency",
                help: "A bucket-read counting fixture.",
                metric: MetricRef::Histogram { h: &self.histogram },
            })
        }
    }

    static COUNTING_HISTOGRAM_GROUP: CountingHistogramGroup = CountingHistogramGroup {
        histogram: CountingHistogram {
            bucket_reads: portable_atomic::AtomicU64::new(0),
        },
    };

    #[test]
    fn histogram_render_reads_each_finite_bucket_once() {
        let before = COUNTING_HISTOGRAM_GROUP
            .histogram
            .bucket_reads
            .load(portable_atomic::Ordering::Relaxed);
        let registry = Registry::<1>::new();
        registry.register(&COUNTING_HISTOGRAM_GROUP);
        let mut out = heapless::String::<512>::new();
        Renderer::<1>::from_registry(&registry)
            .render_to(&mut out)
            .unwrap();
        let reads = COUNTING_HISTOGRAM_GROUP
            .histogram
            .bucket_reads
            .load(portable_atomic::Ordering::Relaxed)
            - before;

        assert_eq!(reads, 3);
        assert!(out.contains("linear_latency_bucket{le=\"10\"} 1\n"));
        assert!(out.contains("linear_latency_bucket{le=\"20\"} 3\n"));
        assert!(out.contains("linear_latency_bucket{le=\"30\"} 6\n"));
    }

    #[cfg(feature = "consistent-histograms")]
    struct ConsistentScalarGroup {
        histogram: IntHistogram<1>,
    }

    #[cfg(feature = "consistent-histograms")]
    impl MetricGroup for ConsistentScalarGroup {
        fn group_name(&self) -> &'static str {
            "consistent_scalar"
        }
        fn len(&self) -> usize {
            1
        }
        fn get(&self, index: usize) -> Option<MetricDesc<'_>> {
            (index == 0).then_some(MetricDesc {
                namespace: "consistent",
                name: "scalar",
                help: "A scalar histogram snapshot fixture.",
                metric: MetricRef::Histogram { h: &self.histogram },
            })
        }
    }

    #[cfg(feature = "consistent-histograms")]
    static CONSISTENT_SCALAR_GROUP: ConsistentScalarGroup = ConsistentScalarGroup {
        histogram: IntHistogram::new(&[10]),
    };

    #[cfg(feature = "consistent-histograms")]
    #[test]
    fn scalar_histogram_snapshot_survives_updates_between_lines() {
        CONSISTENT_SCALAR_GROUP.histogram.observe(1);
        let registry = Registry::<1>::new();
        registry.register(&CONSISTENT_SCALAR_GROUP);
        let mut renderer = Renderer::<1, 256, 1>::from_registry(&registry);

        assert!(renderer.next_line().unwrap().unwrap().starts_with("# HELP"));
        assert!(renderer.next_line().unwrap().unwrap().starts_with("# TYPE"));
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_scalar_bucket{le=\"10\"} 1\n")
        );

        CONSISTENT_SCALAR_GROUP.histogram.observe(2);
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_scalar_bucket{le=\"+Inf\"} 1\n")
        );
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_scalar_sum 1\n")
        );
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_scalar_count 1\n")
        );
    }

    #[cfg(feature = "consistent-histograms")]
    struct ZeroBucketSnapshotGroup {
        histogram: IntHistogram<0>,
    }

    #[cfg(feature = "consistent-histograms")]
    impl MetricGroup for ZeroBucketSnapshotGroup {
        fn group_name(&self) -> &'static str {
            "zero_bucket_snapshot"
        }
        fn len(&self) -> usize {
            1
        }
        fn get(&self, index: usize) -> Option<MetricDesc<'_>> {
            (index == 0).then_some(MetricDesc {
                namespace: "consistent",
                name: "zero_bucket",
                help: "A zero-bucket snapshot fixture.",
                metric: MetricRef::Histogram { h: &self.histogram },
            })
        }
    }

    #[cfg(feature = "consistent-histograms")]
    static ZERO_BUCKET_SNAPSHOT_GROUP: ZeroBucketSnapshotGroup = ZeroBucketSnapshotGroup {
        histogram: IntHistogram::new(&[]),
    };

    #[cfg(feature = "consistent-histograms")]
    #[test]
    fn zero_bucket_histogram_is_snapshotted_before_the_infinite_bucket() {
        ZERO_BUCKET_SNAPSHOT_GROUP.histogram.observe(5);
        let registry = Registry::<1>::new();
        registry.register(&ZERO_BUCKET_SNAPSHOT_GROUP);
        let mut renderer = Renderer::<1, 256, 0>::from_registry(&registry);

        assert!(renderer.next_line().unwrap().unwrap().starts_with("# HELP"));
        assert!(renderer.next_line().unwrap().unwrap().starts_with("# TYPE"));
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_zero_bucket_bucket{le=\"+Inf\"} 1\n")
        );

        ZERO_BUCKET_SNAPSHOT_GROUP.histogram.observe(7);
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_zero_bucket_sum 5\n")
        );
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_zero_bucket_count 1\n")
        );
    }

    #[cfg(feature = "consistent-histograms")]
    struct ConsistentVecGroup {
        histogram: IntHistogramVec<1, 1>,
    }

    #[cfg(feature = "consistent-histograms")]
    impl MetricGroup for ConsistentVecGroup {
        fn group_name(&self) -> &'static str {
            "consistent_vec"
        }
        fn len(&self) -> usize {
            1
        }
        fn get(&self, index: usize) -> Option<MetricDesc<'_>> {
            (index == 0).then_some(MetricDesc {
                namespace: "consistent",
                name: "vector",
                help: "A histogram-vector snapshot fixture.",
                metric: MetricRef::HistogramVec { h: &self.histogram },
            })
        }
    }

    #[cfg(feature = "consistent-histograms")]
    static CONSISTENT_VEC_GROUP: ConsistentVecGroup = ConsistentVecGroup {
        histogram: IntHistogramVec::new(&["kind"], &[10]),
    };

    #[cfg(feature = "consistent-histograms")]
    #[test]
    fn histogram_vec_snapshot_survives_updates_between_lines() {
        CONSISTENT_VEC_GROUP.histogram.observe(&["a"], 1);
        let registry = Registry::<1>::new();
        registry.register(&CONSISTENT_VEC_GROUP);
        let mut renderer = Renderer::<1, 256, 1>::from_registry(&registry);

        assert!(renderer.next_line().unwrap().unwrap().starts_with("# HELP"));
        assert!(renderer.next_line().unwrap().unwrap().starts_with("# TYPE"));
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_vector_bucket{kind=\"a\",le=\"10\"} 1\n")
        );

        CONSISTENT_VEC_GROUP.histogram.observe(&["a"], 2);
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_vector_bucket{kind=\"a\",le=\"+Inf\"} 1\n")
        );
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_vector_sum{kind=\"a\"} 1\n")
        );
        assert_eq!(
            renderer.next_line().unwrap(),
            Some("consistent_vector_count{kind=\"a\"} 1\n")
        );
    }

    #[cfg(feature = "consistent-histograms")]
    struct SnapshotCapacityGroup {
        histogram: IntHistogram<3>,
    }

    #[cfg(feature = "consistent-histograms")]
    impl MetricGroup for SnapshotCapacityGroup {
        fn group_name(&self) -> &'static str {
            "snapshot_capacity"
        }
        fn len(&self) -> usize {
            1
        }
        fn get(&self, index: usize) -> Option<MetricDesc<'_>> {
            (index == 0).then_some(MetricDesc {
                namespace: "consistent",
                name: "capacity",
                help: "A snapshot-capacity fixture.",
                metric: MetricRef::Histogram { h: &self.histogram },
            })
        }
    }

    #[cfg(feature = "consistent-histograms")]
    static SNAPSHOT_CAPACITY_GROUP: SnapshotCapacityGroup = SnapshotCapacityGroup {
        histogram: IntHistogram::new(&[1, 2, 3]),
    };

    #[cfg(feature = "consistent-histograms")]
    #[test]
    fn histogram_snapshot_capacity_error_is_explicit_and_terminal() {
        let registry = Registry::<1>::new();
        registry.register(&SNAPSHOT_CAPACITY_GROUP);
        let mut renderer = Renderer::<1, 256, 2>::from_registry(&registry);
        let expected = RenderError::HistogramCapacityExceeded {
            required: 3,
            capacity: 2,
            namespace: "consistent",
            metric: "capacity",
        };

        assert_eq!(renderer.next_line(), Err(expected));
        assert_eq!(renderer.next_line(), Err(expected));
        assert!(!renderer.is_done());
    }

    struct EmptyGroup;
    impl MetricGroup for EmptyGroup {
        fn group_name(&self) -> &'static str {
            "empty"
        }
        fn len(&self) -> usize {
            0
        }
        fn get(&self, _index: usize) -> Option<MetricDesc<'_>> {
            None
        }
    }
    static EMPTY_GROUP: EmptyGroup = EmptyGroup;

    #[test]
    fn groups_with_no_metrics_are_skipped() {
        let registry = Registry::<1>::new();
        registry.register(&EMPTY_GROUP);
        let mut out = heapless::String::<64>::new();
        Renderer::<4>::from_registry(&registry)
            .render_to(&mut out)
            .unwrap();
        assert_eq!(out.as_str(), "");
    }

    struct ZeroSeriesVecGroup {
        v: FixtureCounterVecEmpty,
    }
    struct FixtureCounterVecEmpty;
    impl ErasedCounterVec for FixtureCounterVecEmpty {
        fn series_count(&self) -> usize {
            0
        }
        fn write_labels(&self, _s: usize, _out: &mut dyn fmt::Write) -> fmt::Result {
            unreachable!()
        }
        fn value(&self, _s: usize) -> u64 {
            unreachable!()
        }
    }
    impl MetricGroup for ZeroSeriesVecGroup {
        fn group_name(&self) -> &'static str {
            "zero"
        }
        fn len(&self) -> usize {
            1
        }
        fn get(&self, index: usize) -> Option<MetricDesc<'_>> {
            (index == 0).then_some(MetricDesc {
                namespace: "zero",
                name: "requests_by_code",
                help: "Requests by status code.",
                metric: MetricRef::CounterVec(&self.v),
            })
        }
    }
    static ZERO_SERIES_GROUP: ZeroSeriesVecGroup = ZeroSeriesVecGroup {
        v: FixtureCounterVecEmpty,
    };

    #[test]
    fn zero_series_vec_still_emits_help_and_type() {
        let registry = Registry::<1>::new();
        registry.register(&ZERO_SERIES_GROUP);
        let mut out = heapless::String::<256>::new();
        Renderer::<4>::from_registry(&registry)
            .render_to(&mut out)
            .unwrap();
        assert_eq!(
            out.as_str(),
            "# HELP zero_requests_by_code Requests by status code.\n\
             # TYPE zero_requests_by_code counter\n"
        );
    }

    #[test]
    fn rendered_len_matches_render_for_an_isolated_registry() {
        rendered_len_metrics::get().requests.inc_by(9_999);

        let mut out = heapless::String::<256>::new();
        Renderer::<1>::from_registry(&RENDERED_LEN_REGISTRY)
            .render_to(&mut out)
            .unwrap();
        let len = rendered_len_from(Renderer::<1>::from_registry(&RENDERED_LEN_REGISTRY)).unwrap();

        assert_eq!(len, out.len());
    }
}
