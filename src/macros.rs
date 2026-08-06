//! The `metrics!` declaration macro and its internal dispatch helpers.
//!
//! `metrics!` expands to a real `Metrics` struct (one field per
//! declared metric, so IDE autocomplete/find-usages work), a `const fn new()`,
//! an `impl MetricGroup` for dynamic dispatch from the registry/renderer, a
//! `METRICS` static holding the group, and a `get()` accessor function. Invoke
//! it in a dedicated module so those fixed names have their own scope.
//!
//! The expansion has three stages:
//!
//! 1. `metrics!` and `__embeprom_metrics_options!` normalize registration and
//!    the optional namespace.
//! 2. `__embeprom_metrics_parse!` is a token-tree muncher that translates the
//!    user-facing, Rust-like declarations into one uniform internal list.
//! 3. `__embeprom_metrics_generate!` emits the group. Small dispatch macros
//!    choose each field's type, initializer, and erased `MetricRef` from that
//!    internal list.
//!
//! The final split by output position is required by `macro_rules!`: one
//! nested macro cannot expand to a struct field, constructor expression, and
//! match-like branch at once.

/// Declare a module-scoped metrics group: a `Metrics` struct with one field
/// per metric, renderable via [`crate::Renderer`]. The generated `get()`
/// accessor self-registers the group with the global registry on first call (see
/// [`crate::OnceRegister`]) — no separate registration step is needed, but
/// [`crate::register`] remains available for eager registration.
///
/// # Example
///
/// ```
/// pub mod metrics {
///     embeprom::metrics! {
///         namespace = "wifi";
///
///         /// Total Wi-Fi frames transmitted.
///         packets_sent: Counter,
///         /// Last measured RSSI in dBm.
///         rssi_dbm: Gauge,
///         /// Disconnects, by reason.
///         #[labels("reason")]
///         disconnects_total: CounterVec<4>,
///         /// TX completion latency in microseconds.
///         #[buckets(100, 500, 1000, 5000)]
///         tx_latency_us: IntHistogram,
///     }
/// }
///
/// // Self-registers on this first call; no explicit registration needed.
/// metrics::get().packets_sent.inc();
/// metrics::get().disconnects_total.inc(&["beacon_timeout"]);
/// ```
///
/// The first line of each field's Rust documentation is also used as its
/// Prometheus help string. Further `///` lines remain part of the Rust
/// documentation. Put documentation for the group as a whole on its enclosing
/// module. Because the generated item names are fixed, invoke this macro at
/// most once per module. A group must declare at least one metric:
///
/// ```compile_fail
/// mod metrics {
///     embeprom::metrics! {}
/// }
/// ```
///
/// Label metadata is checked while the generated static is constructed:
///
/// ```compile_fail
/// mod metrics {
///     embeprom::metrics! {
///         /// Requests by an invalid label name.
///         #[labels("not-valid")]
///         requests: CounterVec<1>,
///     }
/// }
/// ```
///
/// Supported types are [`crate::Counter`], [`crate::Gauge`], `GaugeF64`
/// (feature `float`), `CounterVec<N>`, `GaugeVec<N>`,
/// [`crate::IntHistogram`], `Histogram` (feature `float`),
/// `IntHistogramVec<N>`, and `HistogramVec<N>` (feature `float`). Vec fields
/// declare their label names with `#[labels("name", ...)]`; histogram fields
/// declare finite bucket bounds with `#[buckets(10, 50, ...)]`. A vec can set
/// its rendered label-block allocation independently of the crate-wide default
/// with `#[label_bytes(24)]`.
///
/// By default, the generated accessor lazily registers with the active global
/// registry. Put `registry = PATH;` before the optional namespace and metric
/// declarations to lazily register with a named [`crate::Registry`] instead,
/// or put `registration = manual;` there to disable accessor-driven
/// registration.
#[macro_export]
macro_rules! metrics {
    (registry = $registry:path; $($rest:tt)*) => {
        $crate::__embeprom_metrics_options!([registry $registry] $($rest)*);
    };
    (registration = manual; $($rest:tt)*) => {
        $crate::__embeprom_metrics_options!([manual] $($rest)*);
    };
    ($($rest:tt)*) => {
        $crate::__embeprom_metrics_options!([global] $($rest)*);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_metrics_options {
    ([$($registration:tt)*] namespace = $ns:literal; $($rest:tt)*) => {
        $crate::__embeprom_metrics_parse!([$($registration)*] [$ns] [] $($rest)*);
    };
    ([$($registration:tt)*] $($rest:tt)*) => {
        $crate::__embeprom_metrics_parse!([$($registration)*] [""] [] $($rest)*);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_metrics_parse {
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]) => {
        $crate::__embeprom_metrics_generate!(
            [$($registration)*]
            [$ns]
            $($metrics)*
        );
    };

    // Scalar metrics.
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        $field:ident: Counter, $($rest:tt)*) => {
        $crate::__embeprom_metrics_parse!(
            [$($registration)*] [$ns]
            [$($metrics)* #[doc = $help] $(#[doc = $more_docs])* counter $field = $help;]
            $($rest)*
        );
    };
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        $field:ident: Gauge, $($rest:tt)*) => {
        $crate::__embeprom_metrics_parse!(
            [$($registration)*] [$ns]
            [$($metrics)* #[doc = $help] $(#[doc = $more_docs])* gauge $field = $help;]
            $($rest)*
        );
    };
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        $field:ident: GaugeF64, $($rest:tt)*) => {
        $crate::__embeprom_if_float! {
            $crate::__embeprom_metrics_parse!(
                [$($registration)*] [$ns]
                [$($metrics)* #[doc = $help] $(#[doc = $more_docs])* gauge_f64 $field = $help;]
                $($rest)*
            );
        }
    };

    // Counter and gauge vectors.
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        #[labels($($label:literal),+)]
        $(#[label_bytes($label_bytes:literal)])?
        $field:ident: CounterVec<$cap:literal>, $($rest:tt)*) => {
        $crate::__embeprom_metrics_parse!(
            [$($registration)*] [$ns]
            [$($metrics)* #[doc = $help] $(#[doc = $more_docs])*
                counter_vec<$cap $(, label_bytes: $label_bytes)?> $field[$($label),+] = $help;]
            $($rest)*
        );
    };
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        #[labels($($label:literal),+)]
        $(#[label_bytes($label_bytes:literal)])?
        $field:ident: GaugeVec<$cap:literal>, $($rest:tt)*) => {
        $crate::__embeprom_metrics_parse!(
            [$($registration)*] [$ns]
            [$($metrics)* #[doc = $help] $(#[doc = $more_docs])*
                gauge_vec<$cap $(, label_bytes: $label_bytes)?> $field[$($label),+] = $help;]
            $($rest)*
        );
    };

    // Scalar histograms.
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        #[buckets($($bucket:literal),+)]
        $field:ident: IntHistogram, $($rest:tt)*) => {
        $crate::__embeprom_metrics_parse!(
            [$($registration)*] [$ns]
            [$($metrics)* #[doc = $help] $(#[doc = $more_docs])*
                int_histogram $field[buckets: $($bucket),+] = $help;]
            $($rest)*
        );
    };
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        #[buckets($($bucket:literal),+)]
        $field:ident: Histogram, $($rest:tt)*) => {
        $crate::__embeprom_if_float! {
            $crate::__embeprom_metrics_parse!(
                [$($registration)*] [$ns]
                [$($metrics)* #[doc = $help] $(#[doc = $more_docs])*
                    histogram $field[buckets: $($bucket),+] = $help;]
                $($rest)*
            );
        }
    };

    // Histogram vectors.
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        #[labels($($label:literal),+)]
        #[buckets($($bucket:literal),+)]
        $(#[label_bytes($label_bytes:literal)])?
        $field:ident: IntHistogramVec<$cap:literal>, $($rest:tt)*) => {
        $crate::__embeprom_metrics_parse!(
            [$($registration)*] [$ns]
            [$($metrics)* #[doc = $help] $(#[doc = $more_docs])*
                int_histogram_vec<$cap $(, label_bytes: $label_bytes)?>
                $field[$($label),+; buckets: $($bucket),+] = $help;]
            $($rest)*
        );
    };
    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*]
        #[doc = $help:literal] $(#[doc = $more_docs:literal])*
        #[labels($($label:literal),+)]
        #[buckets($($bucket:literal),+)]
        $(#[label_bytes($label_bytes:literal)])?
        $field:ident: HistogramVec<$cap:literal>, $($rest:tt)*) => {
        $crate::__embeprom_if_float! {
            $crate::__embeprom_metrics_parse!(
                [$($registration)*] [$ns]
                [$($metrics)* #[doc = $help] $(#[doc = $more_docs])*
                    histogram_vec<$cap $(, label_bytes: $label_bytes)?>
                    $field[$($label),+; buckets: $($bucket),+] = $help;]
                $($rest)*
            );
        }
    };

    ([$($registration:tt)*] [$ns:literal] [$($metrics:tt)*] $($invalid:tt)+) => {
        compile_error!(concat!(
            "embeprom: invalid metrics! declaration near `",
            stringify!($($invalid)+),
            "`; expected `/// Help text` followed by `name: MetricType,`"
        ));
    };
}

#[cfg(feature = "float")]
#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_if_float {
    ($($tokens:tt)*) => {
        $($tokens)*
    };
}

#[cfg(not(feature = "float"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_if_float {
    ($($tokens:tt)*) => {
        compile_error!(
            "embeprom: floating-point metric kinds require enabling the `float` feature"
        );
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_metrics_generate {
    (
        [$($registration:tt)*]
        [$ns:literal]
    ) => {
        compile_error!("embeprom: a metrics! group must declare at least one metric");
    };
    (
        [$($registration:tt)*]
        [$ns:literal]
        $(
            $(#[$fmeta:meta])*
            $kind:ident $(< $cap:literal $(, label_bytes: $label_bytes:literal)? >)? $field:ident
                $([ $($extra:tt)* ])? = $help:literal ;
        )+
    ) => {
        /// The metrics declared in this module.
        pub struct Metrics {
            $(
                $(#[$fmeta])*
                pub $field: $crate::__embeprom_ty!(
                    @ty $kind $(, $cap $(, $label_bytes)?)? ; $($($extra)*)?),
            )+
        }

        impl Metrics {
            /// Create a new, empty metrics group.
            pub const fn new() -> Self {
                Self {
                    $(
                        $field: $crate::__embeprom_init!(
                            @init $kind $(, $cap $(, $label_bytes)?)? ; $($($extra)*)?),
                    )+
                }
            }
        }

        impl ::core::default::Default for Metrics {
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
            )+
            assert!($crate::valid_metric_name($ns), "embeprom: invalid namespace");
        };

        impl $crate::MetricGroup for Metrics {
            fn group_name(&self) -> &'static str {
                $ns
            }

            fn len(&self) -> usize {
                $crate::__embeprom_count!($($field),*)
            }

            fn get(&self, index: usize) -> ::core::option::Option<$crate::MetricDesc<'_>> {
                let mut i = index;
                $(
                    if i == 0 {
                        return ::core::option::Option::Some($crate::MetricDesc {
                            namespace: $ns,
                            name: ::core::stringify!($field),
                            help: $help.strip_prefix(' ').unwrap_or($help),
                            metric: $crate::__embeprom_ref!(
                                @ref $kind ; self.$field ; $($($extra)*)?),
                        });
                    }
                    i -= 1;
                )+
                let _ = i;
                ::core::option::Option::None
            }
        }

        /// The static metrics instance.
        pub static METRICS: Metrics = Metrics::new();

        /// Return the static metrics instance, registering it on first use.
        pub fn get() -> &'static Metrics {
            $crate::__embeprom_register_mode!([$($registration)*] &METRICS);
            &METRICS
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
    ([manual] $_group:expr) => {};
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
    (@init counter_vec, $_cap:literal $(, $_label_bytes:literal)? ; $($l:literal),+) => {
        $crate::CounterVec::new(&[$($l),+])
    };
    (@init gauge_vec, $_cap:literal $(, $_label_bytes:literal)? ; $($l:literal),+) => {
        $crate::GaugeVec::new(&[$($l),+])
    };
    (@init int_histogram ; buckets: $($b:literal),+) => {
        $crate::IntHistogram::new(&[$($b),+])
    };
    (@init histogram ; buckets: $($b:literal),+) => {
        $crate::Histogram::new(&[$($b),+])
    };
    (@init int_histogram_vec, $_cap:literal $(, $_label_bytes:literal)? ; $($l:literal),+ ; buckets: $($b:literal),+) => {
        $crate::IntHistogramVec::new(&[$($l),+], &[$($b),+])
    };
    (@init histogram_vec, $_cap:literal $(, $_label_bytes:literal)? ; $($l:literal),+ ; buckets: $($b:literal),+) => {
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
    (@ref counter_vec ; $e:expr ; $($_label:literal),+) => {
        $crate::MetricRef::CounterVec(&$e)
    };
    (@ref gauge_vec ; $e:expr ; $($_label:literal),+) => {
        $crate::MetricRef::GaugeVec(&$e)
    };
    (@ref int_histogram ; $e:expr ; buckets: $($_bucket:literal),+) => {
        $crate::MetricRef::Histogram { h: &$e }
    };
    (@ref histogram ; $e:expr ; buckets: $($_bucket:literal),+) => {
        $crate::MetricRef::Histogram { h: &$e }
    };
    (@ref int_histogram_vec ; $e:expr ; $($_label:literal),+ ; buckets: $($_bucket:literal),+) => {
        $crate::MetricRef::HistogramVec { h: &$e }
    };
    (@ref histogram_vec ; $e:expr ; $($_label:literal),+ ; buckets: $($_bucket:literal),+) => {
        $crate::MetricRef::HistogramVec { h: &$e }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embeprom_count {
    ($($x:tt),*) => {
        [$(::core::stringify!($x)),*].len()
    };
}

#[cfg(test)]
mod tests {
    use crate::MetricGroup;

    /// Wi-Fi driver metrics.
    mod wifi_metrics {
        crate::metrics! {
            namespace = "wifi";

            /// Total Wi-Fi frames transmitted.
            packets_sent: Counter,
            /// Last measured RSSI in dBm.
            rssi_dbm: Gauge,
            /// Disconnects, by reason.
            #[labels("reason")]
            disconnects_total: CounterVec<4>,
            /// TX completion latency in microseconds.
            #[buckets(100, 500, 1000, 5000)]
            tx_latency_us: IntHistogram,
            /// Per-peer latency.
            #[labels("peer")]
            #[buckets(10, 50)]
            peer_latency_us: IntHistogramVec<4>,
        }
    }

    mod accessor_metrics {
        crate::metrics! {
            namespace = "accessor_test";

            /// Accessor calls.
            calls: Counter,
        }
    }

    #[test]
    fn generates_a_real_struct_with_named_fields() {
        let m = wifi_metrics::Metrics::new();
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
        accessor_metrics::get().calls.inc_by(3);
        assert_eq!(accessor_metrics::get().calls.get(), 3);
        assert_eq!(accessor_metrics::get().group_name(), "accessor_test");
        assert!(core::ptr::eq(
            accessor_metrics::get(),
            &raw const accessor_metrics::METRICS
        ));
    }

    #[test]
    fn implements_metric_group_with_correct_arity_and_names() {
        wifi_metrics::get().packets_sent.inc_by(5);
        wifi_metrics::get().rssi_dbm.set(-67);
        wifi_metrics::get()
            .disconnects_total
            .inc(&["beacon_timeout"]);
        wifi_metrics::get().tx_latency_us.observe(100);
        wifi_metrics::get().peer_latency_us.observe(&["ap-1"], 10);

        let group: &dyn MetricGroup = wifi_metrics::get();
        assert_eq!(group.len(), 5);
        assert_eq!(group.get(0).unwrap().name, "packets_sent");
        assert_eq!(group.get(0).unwrap().namespace, "wifi");
        assert_eq!(
            group.get(0).unwrap().help,
            "Total Wi-Fi frames transmitted."
        );
        assert_eq!(group.get(4).unwrap().name, "peer_latency_us");
        assert!(group.get(5).is_none());
    }

    mod registration_metrics {
        crate::metrics! {
            namespace = "registration_test";

            /// Total requests.
            requests: Counter,
        }
    }

    #[test]
    fn accessor_self_registers_on_first_call() {
        registration_metrics::get().requests.inc();
        assert_eq!(
            crate::snapshot()
                .iter()
                .filter(|g| { core::ptr::addr_eq(**g, &raw const registration_metrics::METRICS) })
                .count(),
            1
        );

        // Further calls (and an explicit `register`, which dedups by static
        // identity) don't add a second entry.
        registration_metrics::get().requests.inc();
        crate::register(&registration_metrics::METRICS);
        assert_eq!(
            crate::snapshot()
                .iter()
                .filter(|g| { core::ptr::addr_eq(**g, &raw const registration_metrics::METRICS) })
                .count(),
            1
        );
    }

    mod no_namespace_metrics {
        crate::metrics! {
            /// Total requests.
            requests: Counter,
        }
    }

    #[test]
    fn namespace_is_optional() {
        assert_eq!(no_namespace_metrics::get().group_name(), "");
        let group: &dyn MetricGroup = no_namespace_metrics::get();
        assert_eq!(group.get(0).unwrap().namespace, "");
        assert_eq!(group.get(0).unwrap().name, "requests");
    }

    mod multiline_docs_metrics {
        crate::metrics! {
            registration = manual;

            /// First-line help.
            ///
            /// Additional Rust documentation.
            requests: Counter,
        }
    }

    #[test]
    fn first_doc_line_is_the_prometheus_help() {
        let group: &dyn MetricGroup = multiline_docs_metrics::get();
        assert_eq!(group.get(0).unwrap().help, "First-line help.");
    }

    mod custom_label_bytes_metrics {
        crate::metrics! {
            registration = manual;

            /// Queue depth.
            #[labels("queue")]
            #[label_bytes(18)]
            queue_depth: GaugeVec<2>,
            /// Per-peer latency.
            #[labels("peer")]
            #[buckets(10, 50)]
            #[label_bytes(18)]
            peer_latency: IntHistogramVec<2>,
        }
    }

    #[test]
    fn custom_label_bytes_apply_to_gauge_and_integer_histogram_vectors() {
        let metrics = custom_label_bytes_metrics::get();
        let _: &crate::GaugeVec<2, 1, 18> = &metrics.queue_depth;
        let _: &crate::IntHistogramVec<2, 2, 1, 18> = &metrics.peer_latency;
    }

    #[cfg(feature = "float")]
    mod float_metrics {
        crate::metrics! {
            /// CPU temperature in Celsius.
            cpu_temp_c: GaugeF64,
            /// Request latency.
            #[buckets(0.01, 0.1, 1.0)]
            request_latency_s: Histogram,
            /// Queue depth.
            #[labels("queue")]
            queue_depth: GaugeVec<4>,
            /// Per-peer RTT.
            #[labels("peer")]
            #[buckets(0.05, 0.5)]
            #[label_bytes(18)]
            peer_rtt_s: HistogramVec<4>,
        }
    }

    #[cfg(feature = "float")]
    #[test]
    fn float_kinds_work() {
        let m = float_metrics::get();
        m.cpu_temp_c.set(42.5);
        m.request_latency_s.observe(0.05);
        m.queue_depth.set(&["ingress"], 3);
        m.peer_rtt_s.observe(&["ap-1"], 0.2);

        assert_eq!(m.cpu_temp_c.get().to_bits(), 42.5_f64.to_bits());
        assert_eq!(m.request_latency_s.count(), 1);
        assert_eq!(m.queue_depth.with(&["ingress"]).get(), 3);
        let _: &crate::HistogramVec<4, 2, 1, 18> = &m.peer_rtt_s;

        let group: &dyn MetricGroup = m;
        assert_eq!(group.len(), 4);
    }

    mod literal_bucket_metrics {
        crate::metrics! {
            /// Payload size in bytes.
            #[buckets(1_000u64, 0x7d0u64)]
            payload_bytes: IntHistogram,
        }
    }

    #[test]
    fn histogram_bounds_render_values_instead_of_rust_literal_spelling() {
        let registry = crate::Registry::<1>::new();
        registry.register(literal_bucket_metrics::get());
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

    mod named_registry_metrics {
        crate::metrics! {
            registry = super::NAMED_REGISTRY;

            /// Total requests.
            requests: Counter,
            /// Requests by reason.
            #[labels("reason")]
            #[label_bytes(18)]
            requests_by_reason: CounterVec<2>,
        }
    }

    #[test]
    fn accessor_can_lazily_register_with_a_named_registry() {
        named_registry_metrics::get().requests.inc();
        named_registry_metrics::get().requests.inc();
        named_registry_metrics::get()
            .requests_by_reason
            .inc(&["auth_fail"]);

        assert_eq!(NAMED_REGISTRY.len(), 1);
        assert_eq!(named_registry_metrics::get().requests.get(), 2);
        let _: &crate::CounterVec<2, 1, 18> = &named_registry_metrics::get().requests_by_reason;
        assert_eq!(
            named_registry_metrics::get()
                .requests_by_reason
                .with(&["auth_fail"])
                .get(),
            1
        );
    }

    static MANUAL_REGISTRY_A: crate::Registry<1> = crate::Registry::new();
    static MANUAL_REGISTRY_B: crate::Registry<1> = crate::Registry::new();

    mod manual_metrics {
        crate::metrics! {
            registration = manual;

            /// Total requests.
            requests: Counter,
        }
    }

    #[test]
    fn manual_group_can_be_registered_with_multiple_registries() {
        manual_metrics::get().requests.inc();
        assert!(MANUAL_REGISTRY_A.is_empty());
        assert!(MANUAL_REGISTRY_B.is_empty());

        MANUAL_REGISTRY_A.register(&manual_metrics::METRICS);
        MANUAL_REGISTRY_B.register(&manual_metrics::METRICS);
        assert_eq!(MANUAL_REGISTRY_A.len(), 1);
        assert_eq!(MANUAL_REGISTRY_B.len(), 1);
        assert_eq!(manual_metrics::get().requests.get(), 1);
    }
}
