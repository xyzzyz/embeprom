//! Prometheus text-exposition-format escaping and name validation.

use core::fmt::{self, Write};

/// Escape a label value per the exposition format: `\` -> `\\`, `"` -> `\"`, LF -> `\n`.
pub fn write_escaped_label_value(out: &mut dyn Write, s: &str) -> fmt::Result {
    for c in s.chars() {
        match c {
            '\\' => out.write_str("\\\\")?,
            '"' => out.write_str("\\\"")?,
            '\n' => out.write_str("\\n")?,
            c => out.write_char(c)?,
        }
    }
    Ok(())
}

/// Escape HELP text per the exposition format: `\` -> `\\`, LF -> `\n`. Quotes are
/// not escaped in HELP text.
pub fn write_escaped_help(out: &mut dyn Write, s: &str) -> fmt::Result {
    for c in s.chars() {
        match c {
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            c => out.write_char(c)?,
        }
    }
    Ok(())
}

/// Whether `s` is a valid Prometheus metric name: `[a-zA-Z_:][a-zA-Z0-9_:]*`.
/// An empty string (no namespace) is considered valid.
pub const fn valid_metric_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() {
        return true;
    }
    if !is_name_start(b[0]) {
        return false;
    }
    let mut i = 1;
    while i < b.len() {
        if !is_name_continue(b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether `s` is a valid Prometheus label name: `[a-zA-Z_][a-zA-Z0-9_]*`, and not
/// prefixed with `__` (reserved for internal use).
pub const fn valid_label_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() {
        return false;
    }
    if !is_label_start(b[0]) {
        return false;
    }
    let mut i = 1;
    while i < b.len() {
        if !is_label_continue(b[i]) {
            return false;
        }
        i += 1;
    }
    !(b.len() >= 2 && b[0] == b'_' && b[1] == b'_')
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
    }

    #[test]
    fn escapes_help_text_but_not_quotes() {
        assert_eq!(escaped_help("plain"), "plain");
        assert_eq!(escaped_help("back\\slash"), "back\\\\slash");
        assert_eq!(escaped_help("has\"quote"), "has\"quote");
        assert_eq!(escaped_help("multi\nline"), "multi\\nline");
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
