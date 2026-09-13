use crate::bitstream::BitPlanes;

/// Character class bitmasks computed from bit planes.
///
/// Each `u64` has one bit per input byte (of the 64-byte block).
/// Bit `i` is set if byte `i` belongs to the character class.
#[allow(dead_code)] // Individual fields verified in tests; reader uses composites
pub struct CharClassMasks {
    // Individual character classes
    pub lt: u64,        // '<' 0x3C
    pub gt: u64,        // '>' 0x3E
    pub amp: u64,       // '&' 0x26
    pub dquote: u64,    // '"' 0x22
    pub squote: u64,    // '\'' 0x27
    pub eq: u64,        // '=' 0x3D
    pub slash: u64,     // '/' 0x2F
    pub qmark: u64,     // '?' 0x3F
    pub bang: u64,      // '!' 0x21
    pub dash: u64,      // '-' 0x2D
    pub lbracket: u64,  // '[' 0x5B
    pub rbracket: u64,  // ']' 0x5D
    pub semicolon: u64, // ';' 0x3B
    pub hash: u64,      // '#' 0x23
    pub colon: u64,     // ':' 0x3A
    pub whitespace: u64, // SP(0x20) TAB(0x09) LF(0x0A) CR(0x0D)

    // Composite masks (precomputed for common state machine needs)
    /// Anything that can terminate a name: whitespace | '>' | '/' | '=' | '?'
    pub name_end: u64,
    /// Anything interesting in character content: '<' | '&' | ']'
    pub content_delim: u64,
    /// Anything interesting in a double-quoted attribute value: '"' | '&' | '<'
    pub attr_dq_delim: u64,
    /// Anything interesting in a single-quoted attribute value: '\'' | '&' | '<'
    pub attr_sq_delim: u64,
}

/// Classify a 64-byte block by computing character class bitmasks from bit planes.
///
/// Uses boolean algebra with shared sub-expressions to minimise operations.
#[inline]
pub fn classify(bp: &BitPlanes) -> CharClassMasks {
    let p = &bp.planes;

    // Negations (reused many times)
    let not_p7 = !p[7];
    let not_p6 = !p[6];
    let not_p4 = !p[4];
    let not_p3 = !p[3];
    let not_p2 = !p[2];
    let not_p1 = !p[1];
    let not_p0 = !p[0];

    // High nibble groups (upper 4 bits)
    // 0x0_ : !p7 & !p6 & !p5 & !p4
    let hi_0 = not_p7 & not_p6 & !p[5] & not_p4;
    // 0x2_ : !p7 & !p6 &  p5 & !p4
    let hi_2 = not_p7 & not_p6 & p[5] & not_p4;
    // 0x3_ : !p7 & !p6 &  p5 &  p4
    let hi_3 = not_p7 & not_p6 & p[5] & p[4];
    // 0x5_ : !p7 &  p6 & !p5 &  p4
    let hi_5 = not_p7 & p[6] & !p[5] & p[4];

    // Shared sub-expressions within high-nibble groups
    // hi_3 & p3 & p2: shared by '<'(0x3C) '>'(0x3E) '='(0x3D) '?'(0x3F)
    let hi_3_p3_p2 = hi_3 & p[3] & p[2];
    // hi_2 & !p3 & p2 & p1: shared by '&'(0x26) '\''(0x27)
    let hi_2_np3_p2_p1 = hi_2 & not_p3 & p[2] & p[1];
    // hi_2 & !p3 & !p2: shared by '"'(0x22) '!'(0x21) '#'(0x23)
    let hi_2_np3_np2 = hi_2 & not_p3 & not_p2;
    // hi_2 & p3 & p2: shared by '/'(0x2F) '-'(0x2D)
    let hi_2_p3_p2 = hi_2 & p[3] & p[2];
    // hi_5 & p3: shared by '['(0x5B) ']'(0x5D)
    let hi_5_p3 = hi_5 & p[3];
    // hi_3 & p3 & !p2: shared by ';'(0x3B) ':'(0x3A)
    let hi_3_p3_np2 = hi_3 & p[3] & not_p2;

    // Individual characters
    // '<' = 0x3C = 0011_1100
    let lt = hi_3_p3_p2 & not_p1 & not_p0;
    // '>' = 0x3E = 0011_1110
    let gt = hi_3_p3_p2 & p[1] & not_p0;
    // '=' = 0x3D = 0011_1101
    let eq = hi_3_p3_p2 & not_p1 & p[0];
    // '?' = 0x3F = 0011_1111
    let qmark = hi_3_p3_p2 & p[1] & p[0];

    // '&' = 0x26 = 0010_0110
    let amp = hi_2_np3_p2_p1 & not_p0;
    // '\'' = 0x27 = 0010_0111
    let squote = hi_2_np3_p2_p1 & p[0];

    // '"' = 0x22 = 0010_0010
    let dquote = hi_2_np3_np2 & p[1] & not_p0;
    // '!' = 0x21 = 0010_0001
    let bang = hi_2_np3_np2 & not_p1 & p[0];
    // '#' = 0x23 = 0010_0011
    let hash = hi_2_np3_np2 & p[1] & p[0];

    // '/' = 0x2F = 0010_1111
    let slash = hi_2_p3_p2 & p[1] & p[0];
    // '-' = 0x2D = 0010_1101
    let dash = hi_2_p3_p2 & not_p1 & p[0];

    // '[' = 0x5B = 0101_1011
    let lbracket = hi_5_p3 & not_p2 & p[1] & p[0];
    // ']' = 0x5D = 0101_1101
    let rbracket = hi_5_p3 & p[2] & not_p1 & p[0];

    // ';' = 0x3B = 0011_1011
    let semicolon = hi_3_p3_np2 & p[1] & p[0];
    // ':' = 0x3A = 0011_1010
    let colon = hi_3_p3_np2 & p[1] & not_p0;

    // Whitespace: SP(0x20) | TAB(0x09) | LF(0x0A) | CR(0x0D)
    // SP  = 0x20 = 0010_0000 = hi_2 & !p3 & !p2 & !p1 & !p0
    let sp = hi_2 & not_p3 & not_p2 & not_p1 & not_p0;
    // TAB = 0x09 = 0000_1001 = hi_0 & p3 & !p2 & !p1 & p0
    let tab = hi_0 & p[3] & not_p2 & not_p1 & p[0];
    // LF  = 0x0A = 0000_1010 = hi_0 & p3 & !p2 & p1 & !p0
    let lf = hi_0 & p[3] & not_p2 & p[1] & not_p0;
    // CR  = 0x0D = 0000_1101 = hi_0 & p3 & p2 & !p1 & p0
    let cr = hi_0 & p[3] & p[2] & not_p1 & p[0];
    let whitespace = sp | tab | lf | cr;

    // Composite masks
    let name_end = whitespace | gt | slash | eq | qmark;
    let content_delim = lt | amp | rbracket;
    let attr_dq_delim = dquote | amp | lt;
    let attr_sq_delim = squote | amp | lt;

    CharClassMasks {
        lt,
        gt,
        amp,
        dquote,
        squote,
        eq,
        slash,
        qmark,
        bang,
        dash,
        lbracket,
        rbracket,
        semicolon,
        hash,
        colon,
        whitespace,
        name_end,
        content_delim,
        attr_dq_delim,
        attr_sq_delim,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::scalar;

    /// Helper: create a 64-byte block from a string (padded with spaces).
    fn make_block(s: &[u8]) -> [u8; 64] {
        let mut block = [b' '; 64];
        let len = s.len().min(64);
        block[..len].copy_from_slice(&s[..len]);
        block
    }

    /// Helper: transpose and classify a block.
    fn classify_block(block: &[u8; 64]) -> CharClassMasks {
        let bp = scalar::transpose_64(block);
        classify(&bp)
    }

    #[test]
    fn detect_angle_brackets() {
        let block = make_block(b"<hello>world</hello>");
        let m = classify_block(&block);
        // '<' at positions 0 and 12
        assert_ne!(m.lt & (1 << 0), 0);
        assert_ne!(m.lt & (1 << 12), 0);
        assert_eq!(m.lt & (1 << 1), 0); // 'h' is not '<'
        // '>' at positions 6 and 19
        assert_ne!(m.gt & (1 << 6), 0);
        assert_ne!(m.gt & (1 << 19), 0);
    }

    #[test]
    fn detect_ampersand() {
        // a & a m p ; b
        // 0 1 2 3 4 5 6
        let block = make_block(b"a&amp;b");
        let m = classify_block(&block);
        assert_ne!(m.amp & (1 << 1), 0); // '&' at position 1
        assert_eq!(m.amp & (1 << 5), 0); // ';' is not '&'
        assert_ne!(m.semicolon & (1 << 5), 0); // ';' at position 5
    }

    #[test]
    fn detect_quotes() {
        let block = make_block(b"a=\"hello\" b='world'");
        let m = classify_block(&block);
        // '=' at position 1
        assert_ne!(m.eq & (1 << 1), 0);
        // '"' at positions 2 and 8
        assert_ne!(m.dquote & (1 << 2), 0);
        assert_ne!(m.dquote & (1 << 8), 0);
        // '\'' at positions 12 and 18
        assert_ne!(m.squote & (1 << 12), 0);
        assert_ne!(m.squote & (1 << 18), 0);
    }

    #[test]
    fn detect_whitespace() {
        let block = make_block(b"a b\tc\nd\re");
        let m = classify_block(&block);
        assert_ne!(m.whitespace & (1 << 1), 0); // space
        assert_ne!(m.whitespace & (1 << 3), 0); // tab
        assert_ne!(m.whitespace & (1 << 5), 0); // newline
        assert_ne!(m.whitespace & (1 << 7), 0); // carriage return
        assert_eq!(m.whitespace & (1 << 0), 0); // 'a'
        assert_eq!(m.whitespace & (1 << 2), 0); // 'b'
    }

    #[test]
    fn detect_slash_and_question_mark() {
        // < b r / > < ? x m l ? >
        // 0 1 2 3 4 5 6 7 8 9 10 11
        let block = make_block(b"<br/><?xml?>");
        let m = classify_block(&block);
        // '/' at position 3
        assert_ne!(m.slash & (1 << 3), 0);
        // '?' at positions 6 and 10
        assert_ne!(m.qmark & (1 << 6), 0);
        assert_ne!(m.qmark & (1 << 10), 0);
    }

    #[test]
    fn detect_comment_chars() {
        let block = make_block(b"<!-- comment -->");
        let m = classify_block(&block);
        // '<' at 0
        assert_ne!(m.lt & (1 << 0), 0);
        // '!' at 1
        assert_ne!(m.bang & (1 << 1), 0);
        // '-' at 2, 3, 13, 14
        assert_ne!(m.dash & (1 << 2), 0);
        assert_ne!(m.dash & (1 << 3), 0);
        assert_ne!(m.dash & (1 << 13), 0);
        assert_ne!(m.dash & (1 << 14), 0);
        // '>' at 15
        assert_ne!(m.gt & (1 << 15), 0);
    }

    #[test]
    fn detect_cdata_brackets() {
        let block = make_block(b"<![CDATA[text]]>");
        let m = classify_block(&block);
        // '[' at 2 and 8
        assert_ne!(m.lbracket & (1 << 2), 0);
        assert_ne!(m.lbracket & (1 << 8), 0);
        // ']' at 13 and 14
        assert_ne!(m.rbracket & (1 << 13), 0);
        assert_ne!(m.rbracket & (1 << 14), 0);
    }

    #[test]
    fn detect_hash() {
        // & # 6 0 ; & # x 3 C ;
        // 0 1 2 3 4 5 6 7 8 9 10
        let block = make_block(b"&#60;&#x3C;");
        let m = classify_block(&block);
        // '#' at positions 1 and 6
        assert_ne!(m.hash & (1 << 1), 0);
        assert_ne!(m.hash & (1 << 6), 0);
    }

    #[test]
    fn composite_name_end() {
        let block = make_block(b"foo bar>baz/qux=val?end");
        let m = classify_block(&block);
        // name_end should have: space(3), '>'(7), '/'(11), '='(15), '?'(19)
        assert_ne!(m.name_end & (1 << 3), 0);
        assert_ne!(m.name_end & (1 << 7), 0);
        assert_ne!(m.name_end & (1 << 11), 0);
        assert_ne!(m.name_end & (1 << 15), 0);
        assert_ne!(m.name_end & (1 << 19), 0);
        // 'f' at 0 should not be in name_end
        assert_eq!(m.name_end & (1 << 0), 0);
    }

    #[test]
    fn composite_content_delim() {
        let block = make_block(b"text<more&ref]end");
        let m = classify_block(&block);
        // '<' at 4, '&' at 9, ']' at 13
        assert_ne!(m.content_delim & (1 << 4), 0);
        assert_ne!(m.content_delim & (1 << 9), 0);
        assert_ne!(m.content_delim & (1 << 13), 0);
    }

    #[test]
    fn exhaustive_single_characters() {
        // Test every character we classify, one at a time
        let cases: &[(u8, &str)] = &[
            (b'<', "lt"),
            (b'>', "gt"),
            (b'&', "amp"),
            (b'"', "dquote"),
            (b'\'', "squote"),
            (b'=', "eq"),
            (b'/', "slash"),
            (b'?', "qmark"),
            (b'!', "bang"),
            (b'-', "dash"),
            (b'[', "lbracket"),
            (b']', "rbracket"),
            (b';', "semicolon"),
            (b'#', "hash"),
            (b':', "colon"),
            (b' ', "whitespace"),
            (b'\t', "whitespace"),
            (b'\n', "whitespace"),
            (b'\r', "whitespace"),
        ];

        for &(byte, field_name) in cases {
            let mut block = [0u8; 64];
            block[0] = byte;
            let m = classify_block(&block);

            let mask = match field_name {
                "lt" => m.lt,
                "gt" => m.gt,
                "amp" => m.amp,
                "dquote" => m.dquote,
                "squote" => m.squote,
                "eq" => m.eq,
                "slash" => m.slash,
                "qmark" => m.qmark,
                "bang" => m.bang,
                "dash" => m.dash,
                "lbracket" => m.lbracket,
                "rbracket" => m.rbracket,
                "semicolon" => m.semicolon,
                "hash" => m.hash,
                "colon" => m.colon,
                "whitespace" => m.whitespace,
                _ => unreachable!(),
            };

            assert_ne!(
                mask & 1,
                0,
                "byte 0x{byte:02X} ({}) should set {field_name} at position 0",
                byte as char,
            );
        }
    }

    #[test]
    fn no_false_positives() {
        // Test that a regular ASCII letter doesn't trigger any character class
        let mut block = [b'A'; 64];
        block[0] = b'A';
        let m = classify_block(&block);

        assert_eq!(m.lt & 1, 0);
        assert_eq!(m.gt & 1, 0);
        assert_eq!(m.amp & 1, 0);
        assert_eq!(m.dquote & 1, 0);
        assert_eq!(m.squote & 1, 0);
        assert_eq!(m.eq & 1, 0);
        assert_eq!(m.slash & 1, 0);
        assert_eq!(m.qmark & 1, 0);
        assert_eq!(m.bang & 1, 0);
        assert_eq!(m.dash & 1, 0);
        assert_eq!(m.lbracket & 1, 0);
        assert_eq!(m.rbracket & 1, 0);
        assert_eq!(m.semicolon & 1, 0);
        assert_eq!(m.hash & 1, 0);
        assert_eq!(m.colon & 1, 0);
        assert_eq!(m.whitespace & 1, 0);
    }
}
