//! Prometheus text-exposition-format escaping and legacy name validation.
//!
//! The escaping rules come from the official
//! [Prometheus text exposition format][text-format]. The recommended legacy
//! metric and label name character sets, and the reserved `__` label prefix,
//! come from the [Prometheus data model][data-model].
//!
//! [text-format]: https://prometheus.io/docs/instrumenting/exposition_formats/#prometheus-text-format
//! [data-model]: https://prometheus.io/docs/concepts/data_model/#metric-names-and-labels

use core::fmt::{self, Write};

/// Escape a label value per the exposition format: `\` -> `\\`, `"` -> `\"`, LF -> `\n`.
pub fn write_escaped_label_value<W: Write + ?Sized>(out: &mut W, s: &str) -> fmt::Result {
    write_escaped::<true, _>(out, s)
}

/// Escape HELP text per the exposition format: `\` -> `\\`, LF -> `\n`. Quotes are
/// not escaped in HELP text.
pub fn write_escaped_help<W: Write + ?Sized>(out: &mut W, s: &str) -> fmt::Result {
    write_escaped::<false, _>(out, s)
}

/// Copy ordinary text in runs, only breaking the input around bytes that need
/// escaping. All escaped bytes are ASCII and therefore occur at UTF-8 code
/// point boundaries, so the slices below are always valid strings.
fn write_escaped<const ESCAPE_QUOTES: bool, W: Write + ?Sized>(
    out: &mut W,
    s: &str,
) -> fmt::Result {
    let mut run_start = 0;
    for (i, byte) in s.bytes().enumerate() {
        let escape = match byte {
            b'\\' => Some("\\\\"),
            b'\n' => Some("\\n"),
            b'"' if ESCAPE_QUOTES => Some("\\\""),
            _ => None,
        };
        if let Some(escape) = escape {
            if run_start < i {
                out.write_str(&s[run_start..i])?;
            }
            out.write_str(escape)?;
            run_start = i + 1;
        }
    }
    if run_start < s.len() {
        out.write_str(&s[run_start..])?;
    }
    Ok(())
}

/// Whether `s` is a valid Prometheus metric name: `[a-zA-Z_:][a-zA-Z0-9_:]*`.
/// An empty string (no namespace) is considered valid.
pub const fn valid_metric_name(s: &str) -> bool {
    let Some((first, mut rest)) = s.as_bytes().split_first() else {
        return true;
    };
    if !is_name_start(*first) {
        return false;
    }
    while let Some((next, tail)) = rest.split_first() {
        if !is_name_continue(*next) {
            return false;
        }
        rest = tail;
    }
    true
}

/// Whether `s` is a valid Prometheus label name: `[a-zA-Z_][a-zA-Z0-9_]*`, and not
/// prefixed with `__` (reserved for internal use).
pub const fn valid_label_name(s: &str) -> bool {
    let bytes = s.as_bytes();
    let Some((first, mut rest)) = bytes.split_first() else {
        return false;
    };
    if !is_label_start(*first) {
        return false;
    }
    while let Some((next, tail)) = rest.split_first() {
        if !is_label_continue(*next) {
            return false;
        }
        rest = tail;
    }
    !matches!(bytes, [b'_', b'_', ..])
}

const fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b':'
}

const fn is_name_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b':'
}

const fn is_label_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

const fn is_label_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escaped_value(s: &str) -> heapless::String<64> {
        let mut out = heapless::String::<64>::new();
        write_escaped_label_value(&mut out, s).unwrap();
        out
    }

    fn escaped_help(s: &str) -> heapless::String<64> {
        let mut out = heapless::String::<64>::new();
        write_escaped_help(&mut out, s).unwrap();
        out
    }

    #[test]
    fn escapes_label_values() {
        assert_eq!(escaped_value("plain"), "plain");
        assert_eq!(escaped_value("back\\slash"), "back\\\\slash");
        assert_eq!(escaped_value("has\"quote"), "has\\\"quote");
        assert_eq!(escaped_value("multi\nline"), "multi\\nline");
        assert_eq!(escaped_value("\\\"\n"), "\\\\\\\"\\n");
        assert_eq!(escaped_value("café ☃ 日本語"), "café ☃ 日本語");
    }

    #[test]
    fn escapes_help_text_but_not_quotes() {
        assert_eq!(escaped_help("plain"), "plain");
        assert_eq!(escaped_help("back\\slash"), "back\\\\slash");
        assert_eq!(escaped_help("has\"quote"), "has\"quote");
        assert_eq!(escaped_help("multi\nline"), "multi\\nline");
        assert_eq!(escaped_help("café ☃ 日本語"), "café ☃ 日本語");
    }

    #[test]
    fn metric_names() {
        assert!(valid_metric_name(""));
        assert!(valid_metric_name("wifi"));
        assert!(valid_metric_name("wifi_packets_sent"));
        assert!(valid_metric_name("_leading_underscore"));
        assert!(valid_metric_name("with:colon"));
        assert!(valid_metric_name("Name123"));
        assert!(!valid_metric_name("1starts_with_digit"));
        assert!(!valid_metric_name("has-dash"));
        assert!(!valid_metric_name("has space"));
    }

    #[test]
    fn label_names() {
        assert!(valid_label_name("reason"));
        assert!(valid_label_name("_ok"));
        assert!(!valid_label_name(""));
        assert!(!valid_label_name("1starts_with_digit"));
        assert!(!valid_label_name("has-dash"));
        assert!(!valid_label_name("has:colon"));
        assert!(!valid_label_name("__reserved"));
    }
}
