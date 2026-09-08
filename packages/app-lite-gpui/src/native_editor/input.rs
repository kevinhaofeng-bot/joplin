//! Input-range adapters shared by the native editor.
//!
//! GPUI follows the AppKit text-input contract and therefore reports ranges
//! in UTF-16 code units.  The native document model deliberately stores UTF-8
//! byte offsets, so the conversion stays in this small module instead of
//! leaking platform offsets into the transaction layer.

use std::ops::Range;

/// Convert a platform UTF-16 offset to a UTF-8 byte offset.
///
/// This follows the donor Block implementation: an offset that lands inside a
/// surrogate pair is rounded to the end of that scalar value, which keeps the
/// resulting range a valid UTF-8 boundary.
pub(crate) fn utf16_to_utf8_in(text: &str, offset: usize) -> usize {
    let mut utf8_offset = 0;
    let mut utf16_count = 0;

    for ch in text.chars() {
        if utf16_count >= offset {
            break;
        }
        utf16_count += ch.len_utf16();
        utf8_offset += ch.len_utf8();
    }

    utf8_offset
}

/// Convert a UTF-8 byte offset to the UTF-16 code-unit offset expected by
/// GPUI's input bridge.
pub(crate) fn utf8_to_utf16_in(text: &str, offset: usize) -> usize {
    let mut utf16_offset = 0;
    let mut utf8_count = 0;

    for ch in text.chars() {
        if utf8_count >= offset {
            break;
        }
        utf8_count += ch.len_utf8();
        utf16_offset += ch.len_utf16();
    }

    utf16_offset
}

/// Convert a UTF-16 range to a UTF-8 byte range using the donor's endpoint
/// conversion semantics.
pub(crate) fn utf16_range_to_utf8_in(text: &str, range_utf16: &Range<usize>) -> Range<usize> {
    utf16_to_utf8_in(text, range_utf16.start)..utf16_to_utf8_in(text, range_utf16.end)
}

/// Convert a UTF-8 byte range to a UTF-16 range.
pub(crate) fn utf8_range_to_utf16_in(text: &str, range: &Range<usize>) -> Range<usize> {
    utf8_to_utf16_in(text, range.start)..utf8_to_utf16_in(text, range.end)
}

#[cfg(test)]
mod tests {
    use super::{
        utf8_range_to_utf16_in, utf8_to_utf16_in, utf16_range_to_utf8_in, utf16_to_utf8_in,
    };

    #[test]
    fn utf16_offsets_round_trip_cjk_and_surrogate_pairs() {
        let text = "前😀后";
        assert_eq!(utf16_to_utf8_in(text, 0), 0);
        assert_eq!(utf16_to_utf8_in(text, 1), "前".len());
        assert_eq!(utf16_to_utf8_in(text, 3), "前😀".len());
        assert_eq!(utf16_to_utf8_in(text, 4), text.len());
        assert_eq!(utf8_to_utf16_in(text, "前😀".len()), 3);
        assert_eq!(utf8_to_utf16_in(text, text.len()), 4);
    }

    #[test]
    fn range_conversion_preserves_utf16_selection_endpoints() {
        let text = "甲😀乙";
        let utf16 = 1..4;
        let utf8 = utf16_range_to_utf8_in(text, &utf16);
        assert_eq!(&text[utf8.clone()], "😀乙");
        assert_eq!(utf8_range_to_utf16_in(text, &utf8), utf16);
    }

    #[test]
    fn conversion_clamps_offsets_past_text_end_to_valid_boundaries() {
        let text = "前😀";
        assert_eq!(utf16_to_utf8_in(text, usize::MAX), text.len());
        assert_eq!(utf8_to_utf16_in(text, usize::MAX), 3);
    }
}
