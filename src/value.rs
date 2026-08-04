//! A type-erased metric sample value, used where dynamic dispatch over
//! [`crate::MetricGroup`] needs to carry a scalar reading (e.g. a `Gauge`'s
//! current value) without naming a concrete type.

use core::fmt::{self, Write};

/// A single Prometheus sample value.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    #[cfg(feature = "float")]
    F64(f64),
}

impl Value {
    /// Write this value per the exposition format: `f64::Display` prints `inf`
    /// for infinities, which Prometheus does not accept — this normalizes to
    /// `+Inf` / `-Inf` / `NaN`. Integers are written as plain decimal.
    ///
    /// # Errors
    ///
    /// Returns [`fmt::Error`] if `out` rejects the formatted value.
    pub fn write_prom(&self, out: &mut dyn Write) -> fmt::Result {
        match self {
            Value::U64(v) => write!(out, "{v}"),
            Value::I64(v) => write!(out, "{v}"),
            #[cfg(feature = "float")]
            Value::F64(v) => {
                if v.is_nan() {
                    out.write_str("NaN")
                } else if *v == f64::INFINITY {
                    out.write_str("+Inf")
                } else if *v == f64::NEG_INFINITY {
                    out.write_str("-Inf")
                } else {
                    write!(out, "{v}")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(v: Value) -> heapless::String<32> {
        let mut out = heapless::String::<32>::new();
        v.write_prom(&mut out).unwrap();
        out
    }

    #[test]
    fn integers_render_plainly() {
        assert_eq!(rendered(Value::U64(1843)), "1843");
        assert_eq!(rendered(Value::I64(-67)), "-67");
    }

    #[cfg(feature = "float")]
    #[test]
    fn floats_render_prometheus_correctly() {
        assert_eq!(rendered(Value::F64(1.5)), "1.5");
        assert_eq!(rendered(Value::F64(f64::INFINITY)), "+Inf");
        assert_eq!(rendered(Value::F64(f64::NEG_INFINITY)), "-Inf");
        assert_eq!(rendered(Value::F64(f64::NAN)), "NaN");
    }
}
