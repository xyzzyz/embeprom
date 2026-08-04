//! Compile-time capacity knobs, selected via Cargo features.
//!
//! Per-metric const-generic overrides (e.g. `CounterVec<4, 1, 96>`) are always
//! available and are the recommended way to deviate from these defaults for a
//! single metric; these constants only set the crate-wide default.

/// Default byte budget for a metric's *rendered* label block (e.g. `k1="v1",k2="v2"`,
/// no surrounding braces) — not a per-value length. Overridable per-metric via the
/// vec types' third const-generic parameter. Capacity features are additive:
/// if several are enabled, the largest requested capacity wins.
#[cfg(not(any(feature = "label-value-64", feature = "label-value-128")))]
pub const LABEL_VALUE_LEN: usize = 48;
#[cfg(all(feature = "label-value-64", not(feature = "label-value-128")))]
pub const LABEL_VALUE_LEN: usize = 64;
#[cfg(feature = "label-value-128")]
pub const LABEL_VALUE_LEN: usize = 128;

/// Default maximum number of metric groups the global registry can hold.
/// Capacity features are additive: if several are enabled, the largest
/// requested capacity wins.
#[cfg(not(any(
    feature = "max-groups-32",
    feature = "max-groups-64",
    feature = "max-groups-128",
    feature = "max-groups-256",
    feature = "max-groups-512"
)))]
pub const MAX_GROUPS: usize = 16;
#[cfg(all(
    feature = "max-groups-32",
    not(any(
        feature = "max-groups-64",
        feature = "max-groups-128",
        feature = "max-groups-256",
        feature = "max-groups-512"
    ))
))]
pub const MAX_GROUPS: usize = 32;
#[cfg(all(
    feature = "max-groups-64",
    not(any(
        feature = "max-groups-128",
        feature = "max-groups-256",
        feature = "max-groups-512"
    ))
))]
pub const MAX_GROUPS: usize = 64;
#[cfg(all(
    feature = "max-groups-128",
    not(any(feature = "max-groups-256", feature = "max-groups-512"))
))]
pub const MAX_GROUPS: usize = 128;
#[cfg(all(feature = "max-groups-256", not(feature = "max-groups-512")))]
pub const MAX_GROUPS: usize = 256;
#[cfg(feature = "max-groups-512")]
pub const MAX_GROUPS: usize = 512;

/// Default maximum length in bytes of a single rendered output line. A line
/// exceeding this returns [`crate::RenderError::LineTooLong`]. Override it for
/// one renderer with [`crate::Renderer::with_line_capacity`].
pub const MAX_LINE: usize = 256;

/// Default number of finite histogram buckets that a renderer can snapshot
/// when `consistent-histograms` is enabled. This scratch capacity is reused
/// for each scalar histogram or histogram-vector series and can be overridden
/// with [`crate::Renderer::with_capacities`].
pub const MAX_HISTOGRAM_BUCKETS: usize = 16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn largest_enabled_label_capacity_wins() {
        #[cfg(feature = "label-value-128")]
        assert_eq!(LABEL_VALUE_LEN, 128);
        #[cfg(all(feature = "label-value-64", not(feature = "label-value-128")))]
        assert_eq!(LABEL_VALUE_LEN, 64);
        #[cfg(not(any(feature = "label-value-64", feature = "label-value-128")))]
        assert_eq!(LABEL_VALUE_LEN, 48);
    }

    #[test]
    fn largest_enabled_group_capacity_wins() {
        #[cfg(feature = "max-groups-512")]
        assert_eq!(MAX_GROUPS, 512);
        #[cfg(all(feature = "max-groups-256", not(feature = "max-groups-512")))]
        assert_eq!(MAX_GROUPS, 256);
        #[cfg(all(
            feature = "max-groups-128",
            not(any(feature = "max-groups-256", feature = "max-groups-512"))
        ))]
        assert_eq!(MAX_GROUPS, 128);
        #[cfg(all(
            feature = "max-groups-64",
            not(any(
                feature = "max-groups-128",
                feature = "max-groups-256",
                feature = "max-groups-512"
            ))
        ))]
        assert_eq!(MAX_GROUPS, 64);
        #[cfg(all(
            feature = "max-groups-32",
            not(any(
                feature = "max-groups-64",
                feature = "max-groups-128",
                feature = "max-groups-256",
                feature = "max-groups-512"
            ))
        ))]
        assert_eq!(MAX_GROUPS, 32);
        #[cfg(not(any(
            feature = "max-groups-32",
            feature = "max-groups-64",
            feature = "max-groups-128",
            feature = "max-groups-256",
            feature = "max-groups-512"
        )))]
        assert_eq!(MAX_GROUPS, 16);
    }
}
