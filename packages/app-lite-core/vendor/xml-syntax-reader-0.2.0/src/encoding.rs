use crate::types::{is_xml_whitespace, DeclaredEncoding, Encoding};

/// Result of probing the encoding of an XML document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    /// Detected encoding.
    pub encoding: Encoding,
    /// Number of BOM bytes consumed (0 if no BOM).
    pub bom_length: usize,
}

/// Probe the encoding of an XML document from its initial bytes.
///
/// Inspects up to the first ~128 bytes for:
/// 1. Byte Order Mark (BOM): UTF-8, UTF-16 LE/BE, UTF-32 LE/BE
/// 2. XML declaration `<?xml ... encoding="..."?>`
///
/// The main parser assumes UTF-8. Callers that need to support other
/// encodings should use this function to detect the encoding and
/// transcode before feeding data to `Reader::parse()`.
///
/// Returns `Encoding::Unknown` if the input is too short to determine encoding.
pub fn probe_encoding(data: &[u8]) -> ProbeResult {
    if data.len() < 2 {
        return ProbeResult {
            encoding: Encoding::Unknown,
            bom_length: 0,
        };
    }

    // Check for BOM
    if data.len() >= 4 {
        // UTF-32 LE: FF FE 00 00
        if data[0] == 0xFF && data[1] == 0xFE && data[2] == 0x00 && data[3] == 0x00 {
            return ProbeResult {
                encoding: Encoding::Utf32Le,
                bom_length: 4,
            };
        }
        // UTF-32 BE: 00 00 FE FF
        if data[0] == 0x00 && data[1] == 0x00 && data[2] == 0xFE && data[3] == 0xFF {
            return ProbeResult {
                encoding: Encoding::Utf32Be,
                bom_length: 4,
            };
        }
    }

    if data.len() >= 3 {
        // UTF-8 BOM: EF BB BF
        if data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF {
            return ProbeResult {
                encoding: Encoding::Utf8,
                bom_length: 3,
            };
        }
    }

    // UTF-16 LE: FF FE (must check after UTF-32 LE)
    if data[0] == 0xFF && data[1] == 0xFE {
        return ProbeResult {
            encoding: Encoding::Utf16Le,
            bom_length: 2,
        };
    }
    // UTF-16 BE: FE FF
    if data[0] == 0xFE && data[1] == 0xFF {
        return ProbeResult {
            encoding: Encoding::Utf16Be,
            bom_length: 2,
        };
    }

    // No BOM - check for XML declaration and encoding attribute.
    // Heuristic: if the first bytes look like "<?xml" in some encoding,
    // try to extract the encoding declaration.

    // Check for UTF-16 without BOM by looking at null byte patterns
    if data.len() >= 4 {
        // 00 3C 00 3F → UTF-16 BE without BOM (< ?)
        if data[0] == 0x00 && data[1] == 0x3C && data[2] == 0x00 && data[3] == 0x3F {
            return ProbeResult {
                encoding: Encoding::Utf16Be,
                bom_length: 0,
            };
        }
        // 3C 00 3F 00 → UTF-16 LE without BOM (< ?)
        if data[0] == 0x3C && data[1] == 0x00 && data[2] == 0x3F && data[3] == 0x00 {
            return ProbeResult {
                encoding: Encoding::Utf16Le,
                bom_length: 0,
            };
        }
        // 00 00 00 3C → UTF-32 BE without BOM
        if data[0] == 0x00 && data[1] == 0x00 && data[2] == 0x00 && data[3] == 0x3C {
            return ProbeResult {
                encoding: Encoding::Utf32Be,
                bom_length: 0,
            };
        }
        // 3C 00 00 00 → UTF-32 LE without BOM
        if data[0] == 0x3C && data[1] == 0x00 && data[2] == 0x00 && data[3] == 0x00 {
            return ProbeResult {
                encoding: Encoding::Utf32Le,
                bom_length: 0,
            };
        }
    }

    // Looks like ASCII-compatible (UTF-8 or single-byte). Try to read encoding from XML decl.
    if let Some(enc) = extract_encoding_from_decl(data) {
        return ProbeResult {
            encoding: Encoding::Declared(enc),
            bom_length: 0,
        };
    }

    // Default: assume UTF-8 if it starts with `<` or looks like ASCII
    if data[0] == b'<' || data[0].is_ascii() {
        return ProbeResult {
            encoding: Encoding::Utf8,
            bom_length: 0,
        };
    }

    ProbeResult {
        encoding: Encoding::Unknown,
        bom_length: 0,
    }
}

/// Try to extract an `encoding="..."` value from an XML declaration in ASCII-compatible data.
///
/// Scans for `<?xml` followed by `encoding` attribute. Returns `None` if not found
/// or if the data is too short to contain a complete declaration.
fn extract_encoding_from_decl(data: &[u8]) -> Option<DeclaredEncoding> {
    // Need at least "<?xml encoding='x'?>" = 22 bytes
    if data.len() < 22 {
        return None;
    }

    // Must start with "<?xml" (case-sensitive per XML spec)
    if !data.starts_with(b"<?xml") {
        return None;
    }

    // Byte after "<?xml" must be whitespace
    if data.len() <= 5 || !is_xml_whitespace(data[5]) {
        return None;
    }

    // Scan for "encoding" within the first 256 bytes or until "?>"
    let limit = data.len().min(256);
    let search = &data[6..limit];

    // Find "encoding"
    let enc_pos = find_subsequence(search, b"encoding")?;
    let after_enc = enc_pos + 8; // skip "encoding"

    if after_enc >= search.len() {
        return None;
    }

    // Skip whitespace and '='
    let mut pos = after_enc;
    while pos < search.len() && is_xml_whitespace(search[pos]) {
        pos += 1;
    }
    if pos >= search.len() || search[pos] != b'=' {
        return None;
    }
    pos += 1; // skip '='
    while pos < search.len() && is_xml_whitespace(search[pos]) {
        pos += 1;
    }

    if pos >= search.len() {
        return None;
    }

    // Read quoted value
    let quote = search[pos];
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    pos += 1; // skip opening quote

    let value_start = pos;
    while pos < search.len() && search[pos] != quote {
        pos += 1;
    }
    if pos >= search.len() {
        return None;
    }

    let value = &search[value_start..pos];
    DeclaredEncoding::new(value)
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_bom() {
        let data = b"\xEF\xBB\xBF<?xml version=\"1.0\"?>";
        let result = probe_encoding(data);
        assert_eq!(result.encoding, Encoding::Utf8);
        assert_eq!(result.bom_length, 3);
    }

    #[test]
    fn utf16_le_bom() {
        let data = b"\xFF\xFE<\x00?\x00x\x00m\x00l\x00";
        let result = probe_encoding(data);
        assert_eq!(result.encoding, Encoding::Utf16Le);
        assert_eq!(result.bom_length, 2);
    }

    #[test]
    fn utf16_be_bom() {
        let data = b"\xFE\xFF\x00<\x00?\x00x\x00m\x00l";
        let result = probe_encoding(data);
        assert_eq!(result.encoding, Encoding::Utf16Be);
        assert_eq!(result.bom_length, 2);
    }

    #[test]
    fn utf32_le_bom() {
        let data = b"\xFF\xFE\x00\x00<\x00\x00\x00";
        let result = probe_encoding(data);
        assert_eq!(result.encoding, Encoding::Utf32Le);
        assert_eq!(result.bom_length, 4);
    }

    #[test]
    fn utf32_be_bom() {
        let data = b"\x00\x00\xFE\xFF\x00\x00\x00<";
        let result = probe_encoding(data);
        assert_eq!(result.encoding, Encoding::Utf32Be);
        assert_eq!(result.bom_length, 4);
    }

    #[test]
    fn utf16_be_no_bom() {
        let data = b"\x00<\x00?\x00x\x00m\x00l";
        let result = probe_encoding(data);
        assert_eq!(result.encoding, Encoding::Utf16Be);
        assert_eq!(result.bom_length, 0);
    }

    #[test]
    fn utf16_le_no_bom() {
        let data = b"<\x00?\x00x\x00m\x00l\x00";
        let result = probe_encoding(data);
        assert_eq!(result.encoding, Encoding::Utf16Le);
        assert_eq!(result.bom_length, 0);
    }

    #[test]
    fn encoding_declaration() {
        let data = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>";
        let result = probe_encoding(data);
        assert_eq!(result.bom_length, 0);
        match result.encoding {
            Encoding::Declared(enc) => {
                assert_eq!(enc.as_str(), Some("ISO-8859-1"));
            }
            other => panic!("expected Declared, got {other:?}"),
        }
    }

    #[test]
    fn encoding_declaration_single_quotes() {
        let data = b"<?xml version='1.0' encoding='Shift_JIS'?>";
        let result = probe_encoding(data);
        match result.encoding {
            Encoding::Declared(enc) => {
                assert_eq!(enc.as_str(), Some("Shift_JIS"));
            }
            other => panic!("expected Declared, got {other:?}"),
        }
    }

    #[test]
    fn no_encoding_declaration() {
        let data = b"<?xml version=\"1.0\"?><root/>";
        let result = probe_encoding(data);
        // No encoding attribute → assume UTF-8 (starts with '<')
        assert_eq!(result.encoding, Encoding::Utf8);
        assert_eq!(result.bom_length, 0);
    }

    #[test]
    fn plain_utf8_document() {
        let data = b"<root>hello</root>";
        let result = probe_encoding(data);
        assert_eq!(result.encoding, Encoding::Utf8);
        assert_eq!(result.bom_length, 0);
    }

    #[test]
    fn empty_input() {
        let result = probe_encoding(b"");
        assert_eq!(result.encoding, Encoding::Unknown);
    }

    #[test]
    fn single_byte() {
        let result = probe_encoding(b"<");
        assert_eq!(result.encoding, Encoding::Unknown);
    }

    #[test]
    fn encoding_with_spaces_around_eq() {
        let data = b"<?xml version = \"1.0\" encoding = \"windows-1252\" ?>";
        let result = probe_encoding(data);
        match result.encoding {
            Encoding::Declared(enc) => {
                assert_eq!(enc.as_str(), Some("windows-1252"));
            }
            other => panic!("expected Declared, got {other:?}"),
        }
    }
}
