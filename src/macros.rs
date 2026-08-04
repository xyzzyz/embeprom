//! The `metrics!` declaration macro and its internal dispatch helpers.
//!
//! `metrics!` expands to a real struct (one field per declared metric, so IDE
//! autocomplete/find-usages work), a `const fn new()`, an `impl MetricGroup`
//! for dynamic dispatch from the registry/renderer, a `static` holding the
//! group, and an accessor function.
//!
//! Internally this is decomposed into per-position dispatch macros
//! (`__embeprom_ty!` for the field's type, `__embeprom_init!` for its
//! initializer, `__embeprom_ref!` for the `MetricRef` used by `get()`) that
//! branch on the metric `kind` token. Splitting by position (rather than one
//! macro branching on kind that emits everything) is what keeps this
//! representable in plain `macro_rules!` — each position only needs to know
//! how to produce one kind of output for a given kind, not stitch together a
//! whole item.

/// Declare a metrics group: a struct with one field per metric, renderable
/// via [`crate::Renderer`]. The generated accessor function self-registers
/// the group with the global registry on first call (see
/// [`crate::OnceRegister`]) — no separate registration step is needed, but
/// [`crate::register`] remains available for eager registration.
///
/// # Example
///
/// ```
/// embeprom::metrics! {
///     /// Wi-Fi driver metrics.
///     pub struct WifiMetrics;
///     namespace = "wifi";
///     static METRICS;
///     fn metrics;
///
///     counter        packets_sent            = "Total Wi-Fi frames transmitted.";
///     gauge          rssi_dbm                = "Last measured RSSI in dBm.";
///     counter_vec<4> disconnects_total["reason"] = "Disconnects, by reason.";
///     int_histogram  tx_latency_us[buckets: 100, 500, 1000, 5000]
///                                            = "TX completion latency in microseconds.";
/// }
///
/// // Self-registers on this first call; no `embeprom::register(&METRICS)` needed.
/// metrics().packets_sent.inc();
/// metrics().disconnects_total.inc(&["beacon_timeout"]);
/// ```
///
/// Supported `kind`s: `counter`, `gauge`, `gauge_f64` (feature `float`),
/// `counter_vec<N>`, `gauge_vec<N>`, `int_histogram[buckets: ...]`,
/// `histogram[buckets: ...]` (feature `float`), `int_histogram_vec<N>`, and
/// `histogram_vec<N>` (feature `float`). Vec kinds take label names in
/// brackets (`["name"]` or `["name1", "name2"]`); histogram vec kinds
/// combine label names and buckets separated by `;` (`["peer"; buckets: 10, 50]`).
/// A vec can set its rendered label-block allocation independently of the
/// crate-wide default with an optional second parameter, for example
/// `counter_vec<4, label_bytes: 24>`.
///
/// By default, the generated accessor lazily registers with the active global
/// registry. Put `registry = PATH;` before the struct declaration to lazily
/// register with a named [`crate::Registry`] instead, or put
/// `registration = manual;` there to disable accessor-driven registration.
#[macro_export]
macro_rules! metrics {
    (registry = $registry:path; $($rest:tt)*) => {
        $crate::__embeprom_metrics_generate!([registry $registry] $($rest)*);
    };
    (registration = manual; $($rest:tt)*) => {
        $crate::__embeprom_metrics_generate!([manual] $($rest)*);
    };
    ($($rest:tt)*) => {
        $crate::__embeprom_metrics_generate!([global] $($rest)*);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_metrics_generate {
    (
        [$($registration:tt)*]
        $(#[$smeta:meta])*
        $svis:vis struct $Group:ident;
        $(namespace = $ns:literal;)?
        static $STATIC:ident;
        fn $accessor:ident;
        $(
            $(#[$fmeta:meta])*
            $kind:ident $(< $cap:literal $(, label_bytes: $label_bytes:literal)? >)? $field:ident
                $([ $($extra:tt)* ])? = $help:literal ;
        )*
    ) => {
        $(#[$smeta])*
        $svis struct $Group {
            $(
                $(#[$fmeta])*
                pub $field: $crate::__embeprom_ty!(
                    @ty $kind $(, $cap $(, $label_bytes)?)? ; $($($extra)*)?),
            )*
        }

        impl $Group {
            /// Create a new, empty metrics group.
            pub const fn new() -> Self {
                Self {
                    $(
                        $field: $crate::__embeprom_init!(
                            @init $kind $(, $cap $(, $label_bytes)?)? ; $($($extra)*)?),
                    )*
                }
            }
        }

        impl ::core::default::Default for $Group {
            fn default() -> Self {
                Self::new()
            }
        }

        const _: () = {
            $(
                assert!(
                    $crate::valid_metric_name(::core::stringify!($field)),
                    "embeprom: invalid metric name"
                );
                $crate::__embeprom_validate_labels!(@validate $kind $(, $cap)? ; $($($extra)*)?);
            )*
            $(
                assert!($crate::valid_metric_name($ns), "embeprom: invalid namespace");
            )?
        };

        impl $crate::MetricGroup for $Group {
            fn group_name(&self) -> &'static str {
                $crate::__embeprom_ns!($($ns)?)
            }

            fn len(&self) -> usize {
                $crate::__embeprom_count!($($field),*)
            }

            fn get(&self, index: usize) -> ::core::option::Option<$crate::MetricDesc<'_>> {
                const NAMESPACE: &str = $crate::__embeprom_ns!($($ns)?);
                let mut i = index;
                $(
                    if i == 0 {
                        return ::core::option::Option::Some($crate::MetricDesc {
                            namespace: NAMESPACE,
                            name: ::core::stringify!($field),
                            help: $help,
                            metric: $crate::__embeprom_ref!(
                                @ref $kind ; self.$field ; $($($extra)*)?),
                        });
                    }
                    i -= 1;
                )*
                let _ = i;
                ::core::option::Option::None
            }
        }

        $svis static $STATIC: $Group = $Group::new();

        $svis fn $accessor() -> &'static $Group {
            $crate::__embeprom_register_mode!([$($registration)*] &$STATIC);
            &$STATIC
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_register_mode {
    ([global] $group:expr) => {
        static ONCE: $crate::OnceRegister = $crate::OnceRegister::new();
        ONCE.ensure($group);
    };
    ([registry $registry:path] $group:expr) => {
        static ONCE: $crate::OnceRegister = $crate::OnceRegister::new();
        ONCE.ensure_in(&$registry, $group);
    };
    ([manual] $group:expr) => {
        let _ = $group;
    };
}

/// Register several metric groups with the global registry in one call. See
/// [`crate::register`].
#[macro_export]
macro_rules! register_all {
    ($($g:path),+ $(,)?) => {
        $( $crate::register(&$g); )+
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_ty {
    (@ty counter ;) => { $crate::Counter };
    (@ty gauge ;) => { $crate::Gauge };
    (@ty gauge_f64 ;) => { $crate::GaugeF64 };
    (@ty counter_vec, $cap:literal, $label_bytes:literal ; $($l:literal),+) => {
        $crate::CounterVec<$cap, { $crate::__embeprom_count!($($l),+) }, $label_bytes>
    };
    (@ty counter_vec, $cap:literal ; $($l:literal),+) => {
        $crate::CounterVec<$cap, { $crate::__embeprom_count!($($l),+) }>
    };
    (@ty gauge_vec, $cap:literal, $label_bytes:literal ; $($l:literal),+) => {
        $crate::GaugeVec<$cap, { $crate::__embeprom_count!($($l),+) }, $label_bytes>
    };
    (@ty gauge_vec, $cap:literal ; $($l:literal),+) => {
        $crate::GaugeVec<$cap, { $crate::__embeprom_count!($($l),+) }>
    };
    (@ty int_histogram ; buckets: $($b:literal),+) => {
        $crate::IntHistogram<{ $crate::__embeprom_count!($($b),+) }>
    };
    (@ty histogram ; buckets: $($b:literal),+) => {
        $crate::Histogram<{ $crate::__embeprom_count!($($b),+) }>
    };
    (@ty int_histogram_vec, $cap:literal, $label_bytes:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::IntHistogramVec<
            $cap,
            { $crate::__embeprom_count!($($b),+) },
            { $crate::__embeprom_count!($($l),+) },
            $label_bytes,
        >
    };
    (@ty int_histogram_vec, $cap:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::IntHistogramVec<
            $cap,
            { $crate::__embeprom_count!($($b),+) },
            { $crate::__embeprom_count!($($l),+) },
        >
    };
    (@ty histogram_vec, $cap:literal, $label_bytes:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::HistogramVec<
            $cap,
            { $crate::__embeprom_count!($($b),+) },
            { $crate::__embeprom_count!($($l),+) },
            $label_bytes,
        >
    };
    (@ty histogram_vec, $cap:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::HistogramVec<
            $cap,
            { $crate::__embeprom_count!($($b),+) },
            { $crate::__embeprom_count!($($l),+) },
        >
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_init {
    (@init counter ;) => { $crate::Counter::new() };
    (@init gauge ;) => { $crate::Gauge::new(0) };
    (@init gauge_f64 ;) => { $crate::GaugeF64::new(0.0) };
    (@init counter_vec, $cap:literal, $label_bytes:literal ; $($l:literal),+) => {
        $crate::CounterVec::new(&[$($l),+])
    };
    (@init counter_vec, $cap:literal ; $($l:literal),+) => {
        $crate::CounterVec::new(&[$($l),+])
    };
    (@init gauge_vec, $cap:literal, $label_bytes:literal ; $($l:literal),+) => {
        $crate::GaugeVec::new(&[$($l),+])
    };
    (@init gauge_vec, $cap:literal ; $($l:literal),+) => {
        $crate::GaugeVec::new(&[$($l),+])
    };
    (@init int_histogram ; buckets: $($b:literal),+) => {
        $crate::IntHistogram::new(&[$($b),+])
    };
    (@init histogram ; buckets: $($b:literal),+) => {
        $crate::Histogram::new(&[$($b),+])
    };
    (@init int_histogram_vec, $cap:literal, $label_bytes:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::IntHistogramVec::new(&[$($l),+], &[$($b),+])
    };
    (@init int_histogram_vec, $cap:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::IntHistogramVec::new(&[$($l),+], &[$($b),+])
    };
    (@init histogram_vec, $cap:literal, $label_bytes:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::HistogramVec::new(&[$($l),+], &[$($b),+])
    };
    (@init histogram_vec, $cap:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::HistogramVec::new(&[$($l),+], &[$($b),+])
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_ref {
    (@ref counter ; $e:expr ;) => {
        $crate::MetricRef::Counter(&$e)
    };
    (@ref gauge ; $e:expr ;) => {
        $crate::MetricRef::Gauge($crate::Value::I64($e.get()))
    };
    (@ref gauge_f64 ; $e:expr ;) => {
        $crate::MetricRef::Gauge($crate::Value::F64($e.get()))
    };
    (@ref counter_vec ; $e:expr ; $($l:literal),+) => {
        $crate::MetricRef::CounterVec(&$e)
    };
    (@ref gauge_vec ; $e:expr ; $($l:literal),+) => {
        $crate::MetricRef::GaugeVec(&$e)
    };
    (@ref int_histogram ; $e:expr ; buckets: $($b:literal),+) => {
        $crate::MetricRef::Histogram { h: &$e }
    };
    (@ref histogram ; $e:expr ; buckets: $($b:literal),+) => {
        $crate::MetricRef::Histogram { h: &$e }
    };
    (@ref int_histogram_vec ; $e:expr ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::MetricRef::HistogramVec { h: &$e }
    };
    (@ref histogram_vec ; $e:expr ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::MetricRef::HistogramVec { h: &$e }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_validate_labels {
    (@validate counter ;) => { {} };
    (@validate gauge ;) => { {} };
    (@validate gauge_f64 ;) => { {} };
    (@validate int_histogram ; buckets: $($b:literal),+) => { {} };
    (@validate histogram ; buckets: $($b:literal),+) => { {} };
    (@validate counter_vec, $cap:literal ; $($l:literal),+) => {
        { $( assert!($crate::valid_label_name($l), "embeprom: invalid label name"); )+ }
    };
    (@validate gauge_vec, $cap:literal ; $($l:literal),+) => {
        { $( assert!($crate::valid_label_name($l), "embeprom: invalid label name"); )+ }
    };
    (@validate int_histogram_vec, $cap:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        { $( assert!($crate::valid_label_name($l), "embeprom: invalid label name"); )+ }
    };
    (@validate histogram_vec, $cap:literal ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        { $( assert!($crate::valid_label_name($l), "embeprom: invalid label name"); )+ }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_count {
    ($($x:tt),*) => {
        <[()]>::len(&[$($crate::__embeprom_unit!($x)),*])
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_unit {
    ($x:tt) => {
        ()
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_ns {
    () => {
        ""
    };
    ($ns:literal) => {
        $ns
    };
}

#[cfg(test)]
mod tests {
    use crate::MetricGroup;

    crate::metrics! {
        /// Wi-Fi driver metrics.
        pub struct WifiMetrics;
        namespace = "wifi";
        static METRICS;
        fn metrics;

        counter        packets_sent = "Total Wi-Fi frames transmitted.";
        gauge          rssi_dbm = "Last measured RSSI in dBm.";
        counter_vec<4> disconnects_total["reason"] = "Disconnects, by reason.";
        int_histogram  tx_latency_us[buckets: 100, 500, 1000, 5000]
            = "TX completion latency in microseconds.";
        int_histogram_vec<4> peer_latency_us["peer"; buckets: 10, 50]
            = "Per-peer latency.";
    }

    #[test]
    fn generates_a_real_struct_with_named_fields() {
        let m = WifiMetrics::new();
        m.packets_sent.inc();
        m.rssi_dbm.set(-40);
        m.disconnects_total.inc(&["timeout"]);
        m.tx_latency_us.observe(42);
        m.peer_latency_us.observe(&["ap-1"], 5);

        assert_eq!(m.packets_sent.get(), 1);
        assert_eq!(m.rssi_dbm.get(), -40);
        assert_eq!(m.disconnects_total.with(&["timeout"]).get(), 1);
        assert_eq!(m.tx_latency_us.count(), 1);
    }

    #[test]
    fn accessor_and_static_are_generated() {
        metrics().packets_sent.inc_by(3);
        assert_eq!(metrics().packets_sent.get(), 3);
        assert_eq!(metrics().group_name(), "wifi");
    }

    #[test]
    fn implements_metric_group_with_correct_arity_and_names() {
        metrics().packets_sent.inc_by(5);
        metrics().rssi_dbm.set(-67);
        metrics().disconnects_total.inc(&["beacon_timeout"]);
        metrics().tx_latency_us.observe(100);
        metrics().peer_latency_us.observe(&["ap-1"], 10);

        let group: &dyn MetricGroup = metrics();
        assert_eq!(group.len(), 5);
        assert_eq!(group.get(0).unwrap().name, "packets_sent");
        assert_eq!(group.get(0).unwrap().namespace, "wifi");
        assert_eq!(group.get(4).unwrap().name, "peer_latency_us");
        assert!(group.get(5).is_none());
    }

    crate::metrics! {
        pub struct RegistrationMetrics;
        namespace = "registration_test";
        static REGISTRATION_METRICS;
        fn registration_metrics;

        counter requests = "Total requests.";
    }

    #[test]
    fn accessor_self_registers_on_first_call() {
        registration_metrics().requests.inc();
        assert_eq!(
            crate::snapshot()
                .iter()
                .filter(|g| core::ptr::addr_eq(**g, &REGISTRATION_METRICS))
                .count(),
            1
        );

        // Further calls (and an explicit `register`, which dedups by pointer
        // identity) don't add a second entry.
        registration_metrics().requests.inc();
        crate::register(&REGISTRATION_METRICS);
        assert_eq!(
            crate::snapshot()
                .iter()
                .filter(|g| core::ptr::addr_eq(**g, &REGISTRATION_METRICS))
                .count(),
            1
        );
    }

    crate::metrics! {
        pub struct NoNamespaceMetrics;
        static NO_NS_METRICS;
        fn no_ns_metrics;

        counter requests = "Total requests.";
    }

    #[test]
    fn namespace_is_optional() {
        assert_eq!(no_ns_metrics().group_name(), "");
        let group: &dyn MetricGroup = no_ns_metrics();
        assert_eq!(group.get(0).unwrap().namespace, "");
        assert_eq!(group.get(0).unwrap().name, "requests");
    }

    #[cfg(feature = "float")]
    crate::metrics! {
        pub struct FloatMetrics;
        static FLOAT_METRICS;
        fn float_metrics;

        gauge_f64 cpu_temp_c = "CPU temperature in Celsius.";
        histogram request_latency_s[buckets: 0.01, 0.1, 1.0] = "Request latency.";
        gauge_vec<4> queue_depth["queue"] = "Queue depth.";
        histogram_vec<4> peer_rtt_s["peer"; buckets: 0.05, 0.5] = "Per-peer RTT.";
    }

    #[cfg(feature = "float")]
    #[test]
    fn float_kinds_work() {
        let m = float_metrics();
        m.cpu_temp_c.set(42.5);
        m.request_latency_s.observe(0.05);
        m.queue_depth.set(&["ingress"], 3);
        m.peer_rtt_s.observe(&["ap-1"], 0.2);

        assert_eq!(m.cpu_temp_c.get(), 42.5);
        assert_eq!(m.request_latency_s.count(), 1);
        assert_eq!(m.queue_depth.with(&["ingress"]).get(), 3);

        let group: &dyn MetricGroup = m;
        assert_eq!(group.len(), 4);
    }

    crate::metrics! {
        struct LiteralBucketMetrics;
        static LITERAL_BUCKET_METRICS;
        fn literal_bucket_metrics;

        int_histogram payload_bytes[buckets: 1_000u64, 0x7d0u64]
            = "Payload size in bytes.";
    }

    #[test]
    fn histogram_bounds_render_values_instead_of_rust_literal_spelling() {
        let registry = crate::Registry::<1>::new();
        registry.register(literal_bucket_metrics());
        let mut out = heapless::String::<512>::new();
        crate::Renderer::<1>::from_registry(&registry)
            .render_to(&mut out)
            .unwrap();

        assert!(out.contains("payload_bytes_bucket{le=\"1000\"}"));
        assert!(out.contains("payload_bytes_bucket{le=\"2000\"}"));
        assert!(!out.contains("1_000u64"));
        assert!(!out.contains("0x7d0u64"));
    }

    static NAMED_REGISTRY: crate::Registry<1> = crate::Registry::new();

    crate::metrics! {
        registry = NAMED_REGISTRY;

        struct NamedRegistryMetrics;
        static NAMED_REGISTRY_METRICS;
        fn named_registry_metrics;

        counter requests = "Total requests.";
        counter_vec<2, label_bytes: 18> requests_by_reason["reason"]
            = "Requests by reason.";
    }

    #[test]
    fn accessor_can_lazily_register_with_a_named_registry() {
        named_registry_metrics().requests.inc();
        named_registry_metrics().requests.inc();
        named_registry_metrics()
            .requests_by_reason
            .inc(&["auth_fail"]);

        assert_eq!(NAMED_REGISTRY.len(), 1);
        assert_eq!(named_registry_metrics().requests.get(), 2);
        let _: &crate::CounterVec<2, 1, 18> = &named_registry_metrics().requests_by_reason;
        assert_eq!(
            named_registry_metrics()
                .requests_by_reason
                .with(&["auth_fail"])
                .get(),
            1
        );
    }

    static MANUAL_REGISTRY_A: crate::Registry<1> = crate::Registry::new();
    static MANUAL_REGISTRY_B: crate::Registry<1> = crate::Registry::new();

    crate::metrics! {
        registration = manual;

        struct ManualMetrics;
        static MANUAL_METRICS;
        fn manual_metrics;

        counter requests = "Total requests.";
    }

    #[test]
    fn manual_group_can_be_registered_with_multiple_registries() {
        manual_metrics().requests.inc();
        assert!(MANUAL_REGISTRY_A.is_empty());
        assert!(MANUAL_REGISTRY_B.is_empty());

        MANUAL_REGISTRY_A.register(&MANUAL_METRICS);
        MANUAL_REGISTRY_B.register(&MANUAL_METRICS);
        assert_eq!(MANUAL_REGISTRY_A.len(), 1);
        assert_eq!(MANUAL_REGISTRY_B.len(), 1);
        assert_eq!(manual_metrics().requests.get(), 1);
    }
}
