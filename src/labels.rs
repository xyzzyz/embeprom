//! Building and storing rendered, escaped label blocks for labeled metrics.

use core::fmt::{self, Write};

use crate::escape::write_escaped_label_value;

/// A pre-rendered, pre-escaped label block (e.g. `reason="timeout"`, no
/// surrounding braces), stored as the key for one series of a labeled
/// metric. `V` is the byte budget for the whole block, not one value.
pub type LabelBlock<const V: usize> = heapless::String<V>;

/// The rendered label block exceeded its fixed byte capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelBlockTooLong;

impl From<fmt::Error> for LabelBlockTooLong {
    fn from(_: fmt::Error) -> Self {
        Self
    }
}

/// Render `names[i]="esc(values[i])"` pairs, comma-separated, into a
/// [`LabelBlock`]. Escaping happens once here, not on every render.
///
/// Returns [`LabelBlockTooLong`] if the rendered block would exceed `V` bytes.
pub fn build_block<const K: usize, const V: usize>(
    names: &'static [&'static str; K],
    values: &[&str; K],
) -> Result<LabelBlock<V>, LabelBlockTooLong> {
    let mut out = LabelBlock::<V>::new();
    for (i, (name, value)) in names.iter().zip(values.iter()).enumerate() {
        if i > 0 {
            out.write_char(',')?;
        }
        out.write_str(name)?;
        out.write_char('=')?;
        out.write_char('"')?;
        write_escaped_label_value(&mut out, value)?;
        out.write_char('"')?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    static NAMES: [&str; 1] = ["reason"];
    static NAMES2: [&str; 2] = ["peer", "code"];

    #[test]
    fn builds_a_single_label_block() {
        let block = build_block::<1, 32>(&NAMES, &["beacon_timeout"]).unwrap();
        assert_eq!(block.as_str(), "reason=\"beacon_timeout\"");
    }

    #[test]
    fn builds_a_multi_label_block() {
        let block = build_block::<2, 32>(&NAMES2, &["ap-1", "200"]).unwrap();
        assert_eq!(block.as_str(), "peer=\"ap-1\",code=\"200\"");
    }

    #[test]
    fn escapes_values_while_building() {
        let block = build_block::<1, 48>(&NAMES, &["has\"quote\\and\nnewline"]).unwrap();
        assert_eq!(block.as_str(), "reason=\"has\\\"quote\\\\and\\nnewline\"");
    }

    #[test]
    fn rejects_a_block_that_does_not_fit() {
        assert!(build_block::<1, 8>(&NAMES, &["this value is way too long"]).is_err());
    }
}
