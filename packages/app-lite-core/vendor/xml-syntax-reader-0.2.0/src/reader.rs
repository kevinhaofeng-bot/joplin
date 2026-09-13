use crate::bitstream::{self, TransposeFn};
use crate::classify::{self, CharClassMasks};
use crate::state::{DoctypeSubState, ParserState, QuoteStyle};
#[cfg(feature = "dtd")]
use crate::state::{DtdDeclContext, DtdDeclKind, DtdPhase};
use crate::types::{is_xml_whitespace, Error, ErrorKind, ParseError, QName, Span};
#[cfg(feature = "dtd")]
use crate::types::EntityKind;
use crate::visitor::Visitor;

/// Maximum allowed length (in bytes) for XML names: element names, attribute
/// names, PI targets, DOCTYPE names, and entity reference names.
const MAX_NAME_LENGTH: usize = 1_000;

/// Maximum length for the value between `&#` and `;` in a character reference.
/// The longest valid reference is `&#x10FFFF;` or `&#1114111;` (7 bytes).
const MAX_CHAR_REF_LENGTH: usize = 7;

/// Streaming XML syntax reader.
///
/// The reader borrows the caller's buffer on each `parse()` call, processes
/// as many bytes as possible, and returns the number of bytes consumed.
/// Unconsumed bytes (if any) must be shifted to the front of the buffer
/// by the caller before the next call.
pub struct Reader {
    state: ParserState,
    transpose: TransposeFn,

    /// Buffer position where the current incomplete markup token started.
    /// `None` when in Content state with no pending markup.
    markup_start: Option<usize>,

    /// Buffer position where the current text run started.
    text_start: Option<usize>,

    /// Buffer position where we should resume scanning on the next call.
    /// After each call to `parse()`, this is adjusted by subtracting consumed.
    resume_pos: usize,

    /// Consecutive `]` characters seen at the end of the most recent content scan.
    /// Used to detect `]]>` spanning block or buffer boundaries.
    content_bracket_count: u8,

    /// Buffer position where the current content body run started.
    /// Used for comment, CDATA, PI, and DOCTYPE content chunking.
    content_start: Option<usize>,

    /// Absolute stream offset of the opening markup delimiter (e.g. `<` of `<!--`).
    /// Preserved for EOF error reporting after markup_start is cleared.
    markup_stream_offset: Option<u64>,

    /// Whether any markup has completed (set in `finish_markup` / `finish_content_body`).
    /// Used to detect if `<?xml ...?>` is the first construct or appears later.
    had_markup: bool,

    /// Whether we're currently inside an XML declaration (`<?xml ...?>`).
    in_xml_decl: bool,

    /// Buffers content bytes when inside an XML declaration (fixed-size inline).
    xml_decl_buf: [u8; 256],

    /// Number of valid bytes in `xml_decl_buf`.
    xml_decl_buf_len: usize,

    /// Absolute stream offset of the `<?` that started the XML declaration.
    xml_decl_span_start: u64,

    /// DTD internal subset tokenizer phase.
    #[cfg(feature = "dtd")]
    dtd_phase: DtdPhase,
}

impl Reader {
    pub fn new() -> Self {
        Self {
            state: ParserState::Content,
            transpose: bitstream::select_transpose(),
            markup_start: None,
            text_start: None,
            resume_pos: 0,
            content_bracket_count: 0,
            content_start: None,
            markup_stream_offset: None,
            had_markup: false,
            in_xml_decl: false,
            xml_decl_buf: [0; 256],
            xml_decl_buf_len: 0,
            xml_decl_span_start: 0,
            #[cfg(feature = "dtd")]
            dtd_phase: DtdPhase::Idle,
        }
    }

    /// Reset the reader to initial state for parsing a new document.
    pub fn reset(&mut self) {
        self.finish_content_body();
        self.resume_pos = 0;
        self.content_bracket_count = 0;
        self.had_markup = false;
        self.in_xml_decl = false;
        self.xml_decl_buf_len = 0;
        #[cfg(feature = "dtd")]
        { self.dtd_phase = DtdPhase::Idle; }
    }

    /// Transition back to Content state after completing a markup token.
    #[inline(always)]
    fn finish_markup(&mut self) {
        self.state = ParserState::Content;
        self.markup_start = None;
        self.text_start = None;
        self.markup_stream_offset = None;
        self.had_markup = true;
    }

    /// Transition back to Content state after completing a content body
    /// (comment, CDATA, PI, or DOCTYPE).
    #[inline(always)]
    fn finish_content_body(&mut self) {
        self.finish_markup();
        self.content_start = None;
    }

    /// Try to process markup inline (byte-by-byte) starting at `delim_pos`,
    /// then peek ahead for consecutive tags. Called from the pre-transpose
    /// fast-path when the delimiter is in a future block (text was skipped).
    ///
    /// Returns `Ok(Some((resume, block)))` with new resume_pos and block_offset
    /// on success. Returns `Ok(None)` to fall through to SIMD.
    ///
    /// Outlined (`inline(never)`) to keep the hot `parse()` loop small for
    /// instruction-cache efficiency on dense-tag workloads.
    #[inline(never)]
    fn try_inline_with_peek<V: Visitor>(
        &mut self,
        buf: &[u8],
        delim_pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<Option<(usize, usize)>, ParseError<V::Error>> {
        let b = buf[delim_pos];
        let first_pos = if b == b'<' {
            self.try_inline_tag(buf, delim_pos, stream_offset, visitor)?
        } else if b == b'&' {
            self.try_inline_ref(buf, delim_pos, stream_offset, visitor)?
        } else {
            // b']' - fall through to SIMD for ]]> detection
            None
        };
        let Some(mut pos) = first_pos else {
            return Ok(None);
        };
        // Peek-ahead mini-loop: scan a short window for more tags.
        'peek: loop {
            // An inline tag consumes the previous text_start. Mark the first
            // byte after it before peeking, or a short text run followed by
            // another inline tag is silently omitted from characters().
            self.text_start = Some(pos);
            let limit = (pos + 16).min(buf.len());
            let mut text_scan = pos;
            while text_scan < limit {
                let ch = buf[text_scan];
                if ch == b'<' {
                    if let Some(next) = self.try_inline_tag(
                        buf, text_scan, stream_offset, visitor,
                    )? {
                        pos = next;
                        continue 'peek;
                    }
                    self.text_start = Some(pos);
                    return Ok(Some((text_scan, text_scan / 64 * 64)));
                } else if ch == b'&' {
                    if let Some(next) = self.try_inline_ref(
                        buf, text_scan, stream_offset, visitor,
                    )? {
                        pos = next;
                        continue 'peek;
                    }
                    self.text_start = Some(pos);
                    return Ok(Some((text_scan, text_scan / 64 * 64)));
                } else if ch == b']' {
                    self.text_start = Some(pos);
                    return Ok(Some((text_scan, text_scan / 64 * 64)));
                }
                text_scan += 1;
            }
            // No delimiter in window - go back to memchr3
            self.text_start = Some(pos);
            return Ok(Some((limit, limit / 64 * 64)));
        }
    }

    /// Try to inline-process a tag at `lt_pos`. Handles `<name>`, `<name/>`,
    /// and `</name>`. Falls through for attributes, whitespace after name,
    /// `<!`, `<?`, or buffer boundary.
    #[inline]
    fn try_inline_tag<V: Visitor>(
        &mut self,
        buf: &[u8],
        lt_pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<Option<usize>, ParseError<V::Error>> {
        let after = lt_pos + 1;
        if after >= buf.len() {
            return Ok(None);
        }
        let b = buf[after];
        if b == b'/' {
            self.try_inline_end_tag(buf, lt_pos, stream_offset, visitor)
        } else if is_name_start_byte(b) {
            self.try_inline_start_tag(buf, lt_pos, stream_offset, visitor)
        } else {
            Ok(None)
        }
    }

    /// Inline end tag: `</name>`. Name must be immediately followed by `>`.
    #[inline]
    fn try_inline_end_tag<V: Visitor>(
        &mut self,
        buf: &[u8],
        lt_pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<Option<usize>, ParseError<V::Error>> {
        let name_start = lt_pos + 2;
        if name_start >= buf.len() {
            return Ok(None);
        }
        if !is_name_start_byte(buf[name_start]) {
            return Ok(None);
        }
        let mut i = name_start + 1;
        while i < buf.len() && is_name_byte(buf[i]) {
            i += 1;
        }
        if i >= buf.len() {
            return Ok(None);
        }
        let name_end = i;
        if name_end - name_start > MAX_NAME_LENGTH {
            return Ok(None);
        }
        if buf[name_end] != b'>' {
            return Ok(None);
        }
        self.flush_text_before(buf, lt_pos, stream_offset, visitor)?;
        let name = &buf[name_start..name_end];
        let span = Span::new(
            stream_offset + name_start as u64,
            stream_offset + name_end as u64,
        );
        visitor.end_tag(make_qname(name, span)).map_err(ParseError::Visitor)?;
        Ok(Some(name_end + 1))
    }

    /// Inline start tag: `<name>` or `<name/>`. No attributes or whitespace.
    #[inline]
    fn try_inline_start_tag<V: Visitor>(
        &mut self,
        buf: &[u8],
        lt_pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<Option<usize>, ParseError<V::Error>> {
        let name_start = lt_pos + 1;
        let mut i = name_start + 1;
        while i < buf.len() && is_name_byte(buf[i]) {
            i += 1;
        }
        if i >= buf.len() {
            return Ok(None);
        }
        let name_end = i;
        if name_end - name_start > MAX_NAME_LENGTH {
            return Ok(None);
        }
        let byte = buf[name_end];
        if byte == b'>' {
            self.flush_text_before(buf, lt_pos, stream_offset, visitor)?;
            let name = &buf[name_start..name_end];
            let name_span = Span::new(
                stream_offset + name_start as u64,
                stream_offset + name_end as u64,
            );
            visitor.start_tag_open(make_qname(name, name_span)).map_err(ParseError::Visitor)?;
            let close_span = Span::new(
                stream_offset + name_end as u64,
                stream_offset + name_end as u64 + 1,
            );
            visitor.start_tag_close(close_span).map_err(ParseError::Visitor)?;
            Ok(Some(name_end + 1))
        } else if byte == b'/' {
            let gt_pos = name_end + 1;
            if gt_pos >= buf.len() || buf[gt_pos] != b'>' {
                return Ok(None);
            }
            self.flush_text_before(buf, lt_pos, stream_offset, visitor)?;
            let name = &buf[name_start..name_end];
            let name_span = Span::new(
                stream_offset + name_start as u64,
                stream_offset + name_end as u64,
            );
            visitor.start_tag_open(make_qname(name, name_span)).map_err(ParseError::Visitor)?;
            let close_span = Span::new(
                stream_offset + name_end as u64,
                stream_offset + gt_pos as u64 + 1,
            );
            visitor.empty_element_end(close_span).map_err(ParseError::Visitor)?;
            Ok(Some(gt_pos + 1))
        } else {
            Ok(None)
        }
    }

    /// Inline entity reference: `&name;`. Falls through for char refs (`&#`).
    #[inline]
    fn try_inline_ref<V: Visitor>(
        &mut self,
        buf: &[u8],
        amp_pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<Option<usize>, ParseError<V::Error>> {
        let name_start = amp_pos + 1;
        if name_start >= buf.len() {
            return Ok(None);
        }
        if buf[name_start] == b'#' || !is_name_start_byte(buf[name_start]) {
            return Ok(None);
        }
        let mut i = name_start + 1;
        while i < buf.len() && is_name_byte(buf[i]) {
            i += 1;
        }
        if i >= buf.len() || buf[i] != b';' {
            return Ok(None);
        }
        let name_end = i;
        if name_end - name_start > MAX_NAME_LENGTH {
            return Ok(None);
        }
        self.flush_text_before(buf, amp_pos, stream_offset, visitor)?;
        let name = &buf[name_start..name_end];
        let span = Span::new(
            stream_offset + name_start as u64,
            stream_offset + name_end as u64,
        );
        visitor.entity_ref(name, span).map_err(ParseError::Visitor)?;
        Ok(Some(name_end + 1))
    }

    /// Flush any pending text run before `end_pos` (a markup delimiter).
    #[inline]
    fn flush_text_before<V: Visitor>(
        &mut self,
        buf: &[u8],
        end_pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<(), ParseError<V::Error>> {
        if let Some(text_start) = self.text_start.take() {
            if text_start < end_pos {
                let span = Span::new(
                    stream_offset + text_start as u64,
                    stream_offset + end_pos as u64,
                );
                visitor
                    .characters(&buf[text_start..end_pos], span)
                    .map_err(ParseError::Visitor)?;
            }
        }
        Ok(())
    }

    /// Handle `/` in a start tag: check for `>` to complete `/>`, transition
    /// to `StartTagGotSlash` if at buffer end, or error on an unexpected byte.
    ///
    /// Returns the new block-relative position.
    #[inline(always)]
    fn handle_empty_element_slash<V: Visitor>(
        &mut self,
        buf: &[u8],
        abs: usize,
        block_rel_pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let gt_pos = abs + 1;
        if gt_pos < buf.len() {
            if buf[gt_pos] == b'>' {
                let span = Span::new(
                    stream_offset + abs as u64,
                    stream_offset + gt_pos as u64 + 1,
                );
                visitor
                    .empty_element_end(span)
                    .map_err(ParseError::Visitor)?;
                self.finish_markup();
                Ok(block_rel_pos + 2)
            } else {
                Err(ParseError::Xml(Error {
                    kind: ErrorKind::ExpectedClose(buf[gt_pos]),
                    offset: stream_offset + gt_pos as u64,
                }))
            }
        } else {
            self.state = ParserState::StartTagGotSlash;
            Ok(block_rel_pos + 1)
        }
    }

    /// Parse a complete, in-memory document in a single call.
    ///
    /// This is a convenience wrapper around [`parse()`](Self::parse) for when
    /// the entire input is available in a single buffer. It calls `parse()` once
    /// with `stream_offset = 0` and `is_final = true`.
    pub fn parse_slice<V: Visitor>(
        &mut self,
        buf: &[u8],
        visitor: &mut V,
    ) -> Result<u64, ParseError<V::Error>> {
        self.parse(buf, 0, true, visitor)
    }

    /// Parse as much of `buf` as possible.
    ///
    /// - `buf`: the input bytes to parse
    /// - `stream_offset`: absolute byte offset of `buf[0]` in the overall stream
    /// - `is_final`: `true` if this is the last chunk (no more data coming)
    /// - `visitor`: receives fine-grained parsing events
    ///
    /// Returns `Ok(consumed)` where `consumed <= buf.len()`, indicating how many
    /// bytes were fully processed. The caller must shift `buf[consumed..]` to the
    /// front of the buffer, read more data, and call `parse()` again.
    ///
    /// When `is_final` is false and the buffer ends with an incomplete multi-byte
    /// UTF-8 sequence, the returned `consumed` count excludes the incomplete bytes
    /// so that `characters()` callbacks never split a multi-byte character across
    /// calls. Callers can therefore trust that `std::str::from_utf8()` on any
    /// `&[u8]` slice delivered to `characters()` will not fail due to a buffer
    /// boundary split - only due to genuinely invalid UTF-8 in the source data.
    /// If the trailing bytes are provably invalid UTF-8 (continuation bytes with
    /// no leading byte), `parse()` returns `ErrorKind::InvalidUtf8`.
    pub fn parse<V: Visitor>(
        &mut self,
        buf: &[u8],
        stream_offset: u64,
        is_final: bool,
        visitor: &mut V,
    ) -> Result<u64, ParseError<V::Error>> {
        if buf.is_empty() {
            if is_final && self.state != ParserState::Content {
                return Err(ParseError::Xml(Error {
                    kind: ErrorKind::UnexpectedEof,
                    offset: stream_offset,
                }));
            }
            return Ok(0);
        }

        // Process blocks starting from the block that contains resume_pos
        let first_block = (self.resume_pos / 64) * 64;
        let mut block_offset = first_block;

        while block_offset < buf.len() {
            // Pre-transpose content fast-path: when in Content state with no
            // bracket carry-over, use memchr3 to locate the next delimiter
            // BEFORE paying the transpose + classify cost. This skips SIMD
            // processing entirely for pure-text blocks.
            if matches!(self.state, ParserState::Content) && self.content_bracket_count == 0 {
                let scan_start = if block_offset <= self.resume_pos {
                    self.resume_pos
                } else {
                    block_offset
                };
                if scan_start < buf.len() {
                    if let Some(rel) = memchr::memchr3(b'<', b'&', b']', &buf[scan_start..]) {
                        let delim_pos = scan_start + rel;
                        let delim_block = delim_pos / 64 * 64;
                        if delim_block > block_offset {
                            // Delimiter is in a future block - we skipped text.
                            if self.text_start.is_none() {
                                self.text_start = Some(scan_start);
                            }
                            // Try inline processing to avoid SIMD overhead.
                            if let Some((resume, block)) = self.try_inline_with_peek(
                                buf, delim_pos, stream_offset, visitor,
                            )? {
                                self.resume_pos = resume;
                                block_offset = block;
                                continue;
                            }
                            // Inline failed - skip ahead to delim block (original behavior)
                            block_offset = delim_block;
                            self.resume_pos = block_offset;
                            continue;
                        }
                        // Delimiter is in the current block - fall through
                    } else {
                        // No delimiter in rest of buffer
                        if self.text_start.is_none() {
                            self.text_start = Some(scan_start);
                        }
                        self.resume_pos = buf.len();
                        block_offset = buf.len();
                        continue;
                    }
                }
            }

            let (bp, block_len) = bitstream::transpose_block(self.transpose, buf, block_offset);
            let masks = classify::classify(&bp);

            let start_pos = if block_offset <= self.resume_pos {
                self.resume_pos - block_offset
            } else {
                0
            };

            let final_buf_pos =
                self.process_block(buf, block_offset, block_len, start_pos, &masks, stream_offset, visitor)?;

            self.resume_pos = final_buf_pos;
            block_offset += block_len;
        }

        // Determine consumption boundary
        let mut consumed = if let Some(start) = self.markup_start {
            if is_final {
                return Err(ParseError::Xml(Error {
                    kind: ErrorKind::UnexpectedEof,
                    offset: stream_offset + start as u64,
                }));
            }
            start
        } else if is_final {
            if let Some(offset) = self.markup_stream_offset {
                return Err(ParseError::Xml(Error {
                    kind: ErrorKind::UnexpectedEof,
                    offset,
                }));
            }
            buf.len()
        } else {
            match utf8_boundary_rewind(buf) {
                Ok(rewind) => buf.len() - rewind,
                Err(offset) => {
                    return Err(ParseError::Xml(Error {
                        kind: ErrorKind::InvalidUtf8,
                        offset: stream_offset + offset as u64,
                    }));
                }
            }
        };

        // Exclude trailing delimiter candidate bytes from consumption so they
        // remain in the buffer for rescanning. This prevents incorrectly
        // flushing bytes that may be part of a closing delimiter (e.g. the
        // dashes in `-->`) as content.
        if !is_final {
            let exclude = match &self.state {
                ParserState::CommentContent { dash_count } => *dash_count as usize,
                ParserState::CdataContent { bracket_count } => *bracket_count as usize,
                ParserState::PIContent { saw_qmark: true } => 1,
                #[cfg(feature = "dtd")]
                ParserState::DtdInternalSubset => match &self.dtd_phase {
                    DtdPhase::Comment { dash_count } => *dash_count as usize,
                    DtdPhase::PIContent { saw_qmark: true } => 1,
                    _ => 0,
                },
                _ => 0,
            };
            if exclude > 0 {
                consumed = consumed.saturating_sub(exclude);
                // Reset delimiter counter - bytes will be rescanned
                match &mut self.state {
                    ParserState::CommentContent { dash_count } => *dash_count = 0,
                    ParserState::CdataContent { bracket_count } => *bracket_count = 0,
                    ParserState::PIContent { saw_qmark } => *saw_qmark = false,
                    #[cfg(feature = "dtd")]
                    ParserState::DtdInternalSubset => match &mut self.dtd_phase {
                        DtdPhase::Comment { dash_count } => *dash_count = 0,
                        DtdPhase::PIContent { saw_qmark } => *saw_qmark = false,
                        _ => {}
                    },
                    _ => {}
                }
                // Force resume_pos to rescan excluded bytes
                self.resume_pos = consumed;
            }
        }

        // Flush any pending text run up to the consumption point
        if let Some(text_start) = self.text_start {
            if text_start < consumed {
                let text = &buf[text_start..consumed];
                if !text.is_empty() {
                    let span = Span::new(
                        stream_offset + text_start as u64,
                        stream_offset + consumed as u64,
                    );
                    visitor
                        .characters(text, span)
                        .map_err(ParseError::Visitor)?;
                }
            }
            if consumed >= buf.len() {
                self.text_start = None;
            } else {
                self.text_start = Some(text_start.saturating_sub(consumed));
            }
        }

        // Flush any pending content body run up to the consumption point
        if let Some(cs) = self.content_start {
            if cs < consumed {
                let content = &buf[cs..consumed];
                if !content.is_empty() {
                    let span = Span::new(
                        stream_offset + cs as u64,
                        stream_offset + consumed as u64,
                    );
                    match &self.state {
                        ParserState::CommentContent { .. } => {
                            visitor.comment_content(content, span).map_err(ParseError::Visitor)?;
                        }
                        ParserState::CdataContent { .. } => {
                            visitor.cdata_content(content, span).map_err(ParseError::Visitor)?;
                        }
                        ParserState::PIContent { .. } => {
                            self.emit_pi_content(content, span, visitor)?;
                        }
                        ParserState::DoctypeContent { .. } => {
                            visitor.doctype_content(content, span).map_err(ParseError::Visitor)?;
                        }
                        ParserState::AttrValue { .. } => {
                            visitor.attribute_value(content, span).map_err(ParseError::Visitor)?;
                        }
                        #[cfg(feature = "dtd")]
                        ParserState::DtdInternalSubset => {
                            self.flush_dtd_content(content, span, visitor)?;
                        }
                        _ => {}
                    }
                }
            }
            // Content resumes at byte 0 of the next buffer (still inside the construct)
            self.content_start = Some(cs.saturating_sub(consumed));
        }

        // Adjust all buffer-relative positions for the buffer shift
        if consumed > 0 {
            self.markup_start = self.markup_start.map(|s| s - consumed);
            self.resume_pos = self.resume_pos.saturating_sub(consumed);
            self.state.adjust_positions(consumed);
            #[cfg(feature = "dtd")]
            self.dtd_phase.adjust_positions(consumed);
        }

        Ok(consumed as u64)
    }

    /// Process a single 64-byte block (or partial block).
    /// Returns the buffer-absolute position where processing stopped.
    fn process_block<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        start_pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let mut pos = start_pos; // position within the block

        while pos < block_len {
            match self.state {
                ParserState::Content => {
                    pos = self.scan_content(
                        buf, block_offset, block_len, pos, masks, stream_offset, visitor,
                    )?;
                }

                ParserState::AfterLt => {
                    let abs = block_offset + pos;
                    let byte = buf[abs];
                    match byte {
                        b'/' => {
                            self.state = ParserState::EndTagName {
                                name_start: abs + 1,
                            };
                            pos += 1;
                        }
                        b'?' => {
                            self.state = ParserState::PITarget {
                                name_start: abs + 1,
                            };
                            pos += 1;
                        }
                        b'!' => {
                            self.state = ParserState::AfterLtBang;
                            pos += 1;
                        }
                        _ if is_name_start_byte(byte) => {
                            self.state = ParserState::StartTagName { name_start: abs };
                        }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedName(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }

                ParserState::StartTagName { name_start } => {
                    let Some((next, abs)) =
                        find_name_end(masks.name_end, pos, block_offset, block_len)
                    else {
                        check_name_length(
                            block_offset + block_len,
                            name_start,
                            stream_offset,
                        )?;
                        pos = block_len;
                        continue;
                    };
                    let name = validate_name(buf, name_start, abs, stream_offset)?;
                    let name_span = Span::new(
                        stream_offset + name_start as u64,
                        stream_offset + abs as u64,
                    );
                    visitor
                        .start_tag_open(make_qname(name, name_span))
                        .map_err(ParseError::Visitor)?;
                    self.markup_stream_offset = Some(stream_offset + self.markup_start.unwrap() as u64);
                    self.markup_start = None;

                    let byte = buf[abs];
                    match byte {
                        b'>' => {
                            let span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.start_tag_close(span).map_err(ParseError::Visitor)?;
                            self.finish_markup();
                            pos = next + 1;
                        }
                        b'/' => {
                            pos = self.handle_empty_element_slash(
                                buf, abs, next, stream_offset, visitor,
                            )?;
                        }
                        _ => {
                            self.state = ParserState::StartTagPostName;
                            pos = next;
                        }
                    }
                }

                ParserState::StartTagPostName => {
                    let Some((next, abs)) =
                        find_non_whitespace(masks.whitespace, pos, block_offset, block_len)
                    else {
                        pos = block_len;
                        continue;
                    };
                    let byte = buf[abs];
                    match byte {
                        b'>' => {
                            let span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.start_tag_close(span).map_err(ParseError::Visitor)?;
                            self.finish_markup();
                            pos = next + 1;
                        }
                        b'/' => {
                            pos = self.handle_empty_element_slash(
                                buf, abs, next, stream_offset, visitor,
                            )?;
                        }
                        _ if is_name_start_byte(byte) => {
                            self.markup_start = Some(abs);
                            self.state = ParserState::AttrName { name_start: abs };
                            pos = next;
                        }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedName(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }

                ParserState::StartTagGotSlash => {
                    let abs = block_offset + pos;
                    let byte = buf[abs];
                    if byte == b'>' {
                        let close_span = Span::new(
                            stream_offset + abs as u64 - 1,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor
                            .empty_element_end(close_span)
                            .map_err(ParseError::Visitor)?;
                        self.finish_markup();
                        pos += 1;
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedClose(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }

                ParserState::AttrName { name_start } => {
                    let Some((next, abs)) =
                        find_name_end(masks.name_end, pos, block_offset, block_len)
                    else {
                        check_name_length(
                            block_offset + block_len,
                            name_start,
                            stream_offset,
                        )?;
                        pos = block_len;
                        continue;
                    };
                    let name = validate_name(buf, name_start, abs, stream_offset)?;
                    let name_span = Span::new(
                        stream_offset + name_start as u64,
                        stream_offset + abs as u64,
                    );
                    visitor
                        .attribute_name(make_qname(name, name_span))
                        .map_err(ParseError::Visitor)?;
                    self.markup_start = None;

                    let byte = buf[abs];
                    if byte == b'=' {
                        self.state = ParserState::BeforeAttrValue;
                        pos = next + 1;
                    } else {
                        self.state = ParserState::AfterAttrName;
                        pos = next;
                    }
                }

                ParserState::AfterAttrName => {
                    let Some((next, abs)) =
                        find_non_whitespace(masks.whitespace, pos, block_offset, block_len)
                    else {
                        pos = block_len;
                        continue;
                    };
                    let byte = buf[abs];
                    if byte == b'=' {
                        self.state = ParserState::BeforeAttrValue;
                        pos = next + 1;
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedEquals(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }

                ParserState::BeforeAttrValue => {
                    let abs = block_offset + pos;
                    let byte = buf[abs];
                    if byte == b'"' {
                        self.state = ParserState::AttrValue { quote: QuoteStyle::Double };
                        self.content_start = Some(abs + 1);
                        pos += 1;
                    } else if byte == b'\'' {
                        self.state = ParserState::AttrValue { quote: QuoteStyle::Single };
                        self.content_start = Some(abs + 1);
                        pos += 1;
                    } else if is_xml_whitespace(byte) {
                        pos += 1;
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedQuote(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }

                ParserState::AttrValue { quote } => {
                    let content_start = self.content_start.unwrap();
                    let delim_mask = match quote {
                        QuoteStyle::Double => masks.attr_dq_delim,
                        QuoteStyle::Single => masks.attr_sq_delim,
                    };
                    let delim_byte = match quote {
                        QuoteStyle::Double => b'"',
                        QuoteStyle::Single => b'\'',
                    };
                    let Some((next, abs)) =
                        find_name_end(delim_mask, pos, block_offset, block_len)
                    else {
                        pos = block_len;
                        continue;
                    };
                    let byte = buf[abs];
                    if byte == delim_byte {
                        if content_start < abs {
                            let value = &buf[content_start..abs];
                            let span = Span::new(
                                stream_offset + content_start as u64,
                                stream_offset + abs as u64,
                            );
                            visitor
                                .attribute_value(value, span)
                                .map_err(ParseError::Visitor)?;
                        }
                        let quote_span = Span::new(
                            stream_offset + abs as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor
                            .attribute_end(quote_span)
                            .map_err(ParseError::Visitor)?;
                        self.content_start = None;
                        self.state = ParserState::StartTagPostName;
                        pos = next + 1;
                    } else if byte == b'<' {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedQuote(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    } else {
                        // '&' - flush preceding text, enter entity ref state
                        if content_start < abs {
                            let value = &buf[content_start..abs];
                            let span = Span::new(
                                stream_offset + content_start as u64,
                                stream_offset + abs as u64,
                            );
                            visitor
                                .attribute_value(value, span)
                                .map_err(ParseError::Visitor)?;
                        }
                        self.markup_start = Some(abs);
                        self.content_start = None;
                        self.state = ParserState::AttrEntityRef {
                            name_start: abs + 1,
                            quote,
                        };
                        pos = next + 1;
                    }
                }

                ParserState::EndTagName { name_start } => {
                    let Some((next, abs)) =
                        find_name_end(masks.name_end, pos, block_offset, block_len)
                    else {
                        check_name_length(
                            block_offset + block_len,
                            name_start,
                            stream_offset,
                        )?;
                        pos = block_len;
                        continue;
                    };
                    let name = validate_name(buf, name_start, abs, stream_offset)?;
                    let name_span = Span::new(
                        stream_offset + name_start as u64,
                        stream_offset + abs as u64,
                    );
                    visitor
                        .end_tag(make_qname(name, name_span))
                        .map_err(ParseError::Visitor)?;

                    let byte = buf[abs];
                    if byte == b'>' {
                        self.finish_markup();
                        pos = next + 1;
                    } else {
                        self.state = ParserState::EndTagPostName;
                        pos = next;
                    }
                }

                ParserState::EndTagPostName => {
                    let Some((next, abs)) =
                        find_non_whitespace(masks.whitespace, pos, block_offset, block_len)
                    else {
                        pos = block_len;
                        continue;
                    };
                    let byte = buf[abs];
                    if byte == b'>' {
                        self.finish_markup();
                        pos = next + 1;
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedClose(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }

                // --- Phase 2 states ---

                ParserState::AfterLtBang => {
                    let abs = block_offset + pos;
                    let byte = buf[abs];
                    match byte {
                        b'-' => {
                            self.state = ParserState::AfterLtBangDash;
                            pos += 1;
                        }
                        b'[' => {
                            self.state = ParserState::AfterLtBangBracket { matched: 0 };
                            pos += 1;
                        }
                        b'D' => {
                            self.state = ParserState::AfterLtBangD { matched: 0 };
                            pos += 1;
                        }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedKeyword(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }

                ParserState::AfterLtBangDash => {
                    let abs = block_offset + pos;
                    let byte = buf[abs];
                    if byte == b'-' {
                        // '<!--' complete
                        let start_span = Span::new(
                            stream_offset + self.markup_start.unwrap() as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor
                            .comment_start(start_span)
                            .map_err(ParseError::Visitor)?;
                        self.markup_stream_offset = Some(stream_offset + self.markup_start.unwrap() as u64);
                        self.markup_start = None;
                        self.content_start = Some(abs + 1);
                        self.state = ParserState::CommentContent {
                            dash_count: 0,
                        };
                        pos += 1;
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedKeyword(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }

                ParserState::CommentContent { dash_count } => {
                    pos = self.scan_comment_content(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, dash_count, visitor,
                    )?;
                }

                ParserState::AfterLtBangBracket { matched } => {
                    let abs = block_offset + pos;
                    let byte = buf[abs];
                    const CDATA_CHARS: &[u8] = b"CDATA[";
                    if byte == CDATA_CHARS[matched as usize] {
                        let new_matched = matched + 1;
                        if new_matched as usize == CDATA_CHARS.len() {
                            // '<![CDATA[' complete
                            let start_span = Span::new(
                                stream_offset + self.markup_start.unwrap() as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor
                                .cdata_start(start_span)
                                .map_err(ParseError::Visitor)?;
                            self.markup_stream_offset = Some(stream_offset + self.markup_start.unwrap() as u64);
                            self.markup_start = None;
                            self.content_start = Some(abs + 1);
                            self.state = ParserState::CdataContent {
                                bracket_count: 0,
                            };
                        } else {
                            self.state = ParserState::AfterLtBangBracket { matched: new_matched };
                        }
                        pos += 1;
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedKeyword(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }

                ParserState::CdataContent { bracket_count } => {
                    pos = self.scan_cdata_content(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, bracket_count, visitor,
                    )?;
                }

                ParserState::AfterLtBangD { matched } => {
                    let abs = block_offset + pos;
                    let byte = buf[abs];
                    const DOCTYPE_CHARS: &[u8] = b"OCTYPE";
                    if byte == DOCTYPE_CHARS[matched as usize] {
                        let new_matched = matched + 1;
                        if new_matched as usize == DOCTYPE_CHARS.len() {
                            // Use usize::MAX as sentinel: "need to skip whitespace first"
                            self.state = ParserState::DoctypeName { name_start: usize::MAX };
                        } else {
                            self.state = ParserState::AfterLtBangD { matched: new_matched };
                        }
                        pos += 1;
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedKeyword(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }

                ParserState::DoctypeName { name_start } => {
                    pos = self.scan_doctype_name(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, name_start, visitor,
                    )?;
                }

                ParserState::DoctypeContent { depth, sub } => {
                    pos = self.scan_doctype_content(
                        buf, block_offset, block_len, pos,
                        stream_offset, depth, sub, visitor,
                    )?;
                }

                ParserState::PITarget { name_start } => {
                    pos = self.scan_pi_target(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, name_start, visitor,
                    )?;
                }

                ParserState::PIContent { saw_qmark } => {
                    pos = self.scan_pi_content(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, saw_qmark, visitor,
                    )?;
                }

                ParserState::EntityRef { name_start } => {
                    pos = self.scan_entity_ref(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, name_start, visitor,
                    )?;
                }

                ParserState::CharRef { value_start } => {
                    pos = self.scan_char_ref(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, value_start, visitor,
                    )?;
                }

                ParserState::AttrEntityRef { name_start, quote } => {
                    pos = self.scan_attr_entity_ref(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, name_start, quote, visitor,
                    )?;
                }

                ParserState::AttrCharRef { value_start, quote } => {
                    pos = self.scan_attr_char_ref(
                        buf, block_offset, block_len, pos, masks,
                        stream_offset, value_start, quote, visitor,
                    )?;
                }

                #[cfg(feature = "dtd")]
                ParserState::DtdInternalSubset => {
                    pos = self.scan_dtd_internal_subset(
                        buf, block_offset, block_len, pos,
                        stream_offset, visitor,
                    )?;
                }

                #[cfg(feature = "dtd")]
                ParserState::DoctypeAfterSubset => {
                    pos = self.scan_doctype_after_subset(
                        buf, block_offset, block_len, pos,
                        stream_offset, visitor,
                    )?;
                }
            }
        }

        Ok(block_offset + pos)
    }

    /// Scan content (text between markup). Returns the new block-relative position.
    fn scan_content<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        mut pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        if self.text_start.is_none() {
            self.text_start = Some(block_offset + pos);
        }

        // Handle carry-over bracket count from previous block/buffer.
        if self.content_bracket_count > 0 {
            let abs = block_offset + pos;
            if abs < buf.len() {
                let mut scan = abs;
                let mut brackets = self.content_bracket_count;
                while scan < buf.len() {
                    let ch = buf[scan];
                    if ch == b']' {
                        brackets = brackets.saturating_add(1);
                        scan += 1;
                    } else if ch == b'>' && brackets >= 2 {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::CdataEndInContent,
                            offset: stream_offset + scan as u64 - 2,
                        }));
                    } else {
                        self.content_bracket_count = 0;
                        break;
                    }
                }
                if scan >= buf.len() {
                    self.content_bracket_count = brackets.min(2);
                }
                let consumed_in_block = scan - block_offset;
                if consumed_in_block >= block_len {
                    return Ok(block_len);
                }
                pos = consumed_in_block;
            }
        }

        loop {
            if pos >= block_len {
                return Ok(block_len);
            }

            let shifted = masks.content_delim >> pos;
            if shifted == 0 {
                return Ok(block_len);
            }

            let next = shifted.trailing_zeros() as usize;
            if pos + next >= block_len {
                return Ok(block_len);
            }

            let abs = block_offset + pos + next;
            let byte = buf[abs];

            match byte {
                b'<' => {
                    self.content_bracket_count = 0;
                    if let Some(text_start) = self.text_start.take() {
                        if text_start < abs {
                            let span = Span::new(
                                stream_offset + text_start as u64,
                                stream_offset + abs as u64,
                            );
                            visitor
                                .characters(&buf[text_start..abs], span)
                                .map_err(ParseError::Visitor)?;
                        }
                    }
                    self.markup_start = Some(abs);

                    // Peek at the byte after '<' to skip the AfterLt dispatch.
                    let after = abs + 1;
                    if after < buf.len() {
                        let b = buf[after];
                        match b {
                            b'/' => {
                                self.state = ParserState::EndTagName {
                                    name_start: after + 1,
                                };
                                return Ok(pos + next + 2);
                            }
                            b'?' => {
                                self.state = ParserState::PITarget {
                                    name_start: after + 1,
                                };
                                return Ok(pos + next + 2);
                            }
                            b'!' => {
                                self.state = ParserState::AfterLtBang;
                                return Ok(pos + next + 2);
                            }
                            _ if is_name_start_byte(b) => {
                                self.state =
                                    ParserState::StartTagName { name_start: after };
                                return Ok(pos + next + 1);
                            }
                            _ => {
                                return Err(ParseError::Xml(Error {
                                    kind: ErrorKind::ExpectedName(b),
                                    offset: stream_offset + after as u64,
                                }));
                            }
                        }
                    } else {
                        // '<' at buffer end - fall back to AfterLt state
                        self.state = ParserState::AfterLt;
                        return Ok(pos + next + 1);
                    }
                }
                b'&' => {
                    self.content_bracket_count = 0;
                    if let Some(text_start) = self.text_start.take() {
                        if text_start < abs {
                            let span = Span::new(
                                stream_offset + text_start as u64,
                                stream_offset + abs as u64,
                            );
                            visitor
                                .characters(&buf[text_start..abs], span)
                                .map_err(ParseError::Visitor)?;
                        }
                    }
                    self.state = ParserState::EntityRef {
                        name_start: abs + 1,
                    };
                    self.markup_start = Some(abs);
                    return Ok(pos + next + 1);
                }
                b']' => {
                    // Track consecutive ']' to detect illegal ]]> in content.
                    // We need to look ahead for more ']' and a final '>'.
                    let mut scan = abs + 1;
                    let mut brackets: u8 = self.content_bracket_count + 1;
                    while scan < buf.len() {
                        let ch = buf[scan];
                        if ch == b']' {
                            brackets = brackets.saturating_add(1);
                            scan += 1;
                        } else if ch == b'>' && brackets >= 2 {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::CdataEndInContent,
                                offset: stream_offset + scan as u64 - 2,
                            }));
                        } else {
                            // Not ]]>, reset and resume normal scanning
                            self.content_bracket_count = 0;
                            break;
                        }
                    }
                    if scan >= buf.len() {
                        // Brackets at end of available data - remember count
                        self.content_bracket_count = brackets.min(2);
                    }
                    // Advance past all the bytes we just examined
                    let consumed_in_block = scan - block_offset;
                    if consumed_in_block >= block_len {
                        return Ok(block_len);
                    }
                    pos = consumed_in_block;
                }
                _ => unreachable!(),
            }
        }
    }

    // ====================================================================
    // Phase 2 scanner methods
    // ====================================================================

    /// Scan comment content, looking for `-->`.
    ///
    /// Uses byte-at-a-time scanning when dash_count > 0 (carry-over from
    /// previous block boundary), and bitmask scanning otherwise.
    fn scan_comment_content<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        mut pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        mut dash_count: u8,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let content_start = self.content_start.unwrap();
        loop {
            // Handle carry-over dashes from previous block boundary
            while dash_count > 0 && pos < block_len {
                let abs = block_offset + pos;
                let byte = buf[abs];
                if byte == b'>' && dash_count >= 2 {
                    // '-->' found
                    let content_end = abs - dash_count as usize;
                    if content_end > content_start {
                        let span = Span::new(
                            stream_offset + content_start as u64,
                            stream_offset + content_end as u64,
                        );
                        visitor
                            .comment_content(&buf[content_start..content_end], span)
                            .map_err(ParseError::Visitor)?;
                    }
                    let end_span = Span::new(
                        stream_offset + content_end as u64,
                        stream_offset + abs as u64 + 1,
                    );
                    visitor
                        .comment_end(end_span)
                        .map_err(ParseError::Visitor)?;
                    self.finish_content_body();
                    return Ok(pos + 1);
                } else if dash_count >= 2 {
                    // '--' not followed by '>' - covers '--x', '---', '-- ', etc.
                    return Err(ParseError::Xml(Error {
                        kind: ErrorKind::DoubleDashInComment,
                        offset: stream_offset + abs as u64 - 2,
                    }));
                } else if byte == b'-' {
                    dash_count += 1;
                    pos += 1;
                } else {
                    // Single '-' not followed by '-' - reset and resume bitmask scanning
                    dash_count = 0;
                    pos += 1;
                    break;
                }
            }

            if pos >= block_len {
                self.state = ParserState::CommentContent { dash_count };
                return Ok(block_len);
            }

            if dash_count > 0 {
                // Still in dash mode but ran out of block
                self.state = ParserState::CommentContent { dash_count };
                return Ok(block_len);
            }

            // Bitmask scan for next dash
            let shifted = masks.dash >> pos;
            if shifted == 0 {
                self.state = ParserState::CommentContent { dash_count: 0 };
                return Ok(block_len);
            }

            let next = shifted.trailing_zeros() as usize;
            if pos + next >= block_len {
                self.state = ParserState::CommentContent { dash_count: 0 };
                return Ok(block_len);
            }

            // Found a dash - start counting
            pos = pos + next;
            dash_count = 1;
            pos += 1;
            // Loop back to handle consecutive dashes via byte-at-a-time
        }
    }

    /// Scan CDATA content, looking for `]]>`.
    ///
    /// Same pattern as comment scanning: byte-at-a-time when bracket_count > 0,
    /// bitmask scanning otherwise.
    fn scan_cdata_content<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        mut pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        mut bracket_count: u8,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let content_start = self.content_start.unwrap();
        loop {
            // Handle carry-over brackets from previous block boundary
            while bracket_count > 0 && pos < block_len {
                let abs = block_offset + pos;
                let byte = buf[abs];
                if byte == b']' {
                    bracket_count = (bracket_count + 1).min(2);
                    pos += 1;
                } else if byte == b'>' && bracket_count >= 2 {
                    // ']]>' found
                    let content_end = abs - bracket_count as usize;
                    if content_end > content_start {
                        let span = Span::new(
                            stream_offset + content_start as u64,
                            stream_offset + content_end as u64,
                        );
                        visitor
                            .cdata_content(&buf[content_start..content_end], span)
                            .map_err(ParseError::Visitor)?;
                    }
                    let end_span = Span::new(
                        stream_offset + content_end as u64,
                        stream_offset + abs as u64 + 1,
                    );
                    visitor
                        .cdata_end(end_span)
                        .map_err(ParseError::Visitor)?;
                    self.finish_content_body();
                    return Ok(pos + 1);
                } else {
                    bracket_count = 0;
                    pos += 1;
                    break;
                }
            }

            if pos >= block_len {
                self.state = ParserState::CdataContent { bracket_count };
                return Ok(block_len);
            }

            if bracket_count > 0 {
                self.state = ParserState::CdataContent { bracket_count };
                return Ok(block_len);
            }

            // Bitmask scan for next ']'
            let shifted = masks.rbracket >> pos;
            if shifted == 0 {
                self.state = ParserState::CdataContent { bracket_count: 0 };
                return Ok(block_len);
            }

            let next = shifted.trailing_zeros() as usize;
            if pos + next >= block_len {
                self.state = ParserState::CdataContent { bracket_count: 0 };
                return Ok(block_len);
            }

            // Found a ']' - start counting
            pos = pos + next;
            bracket_count = 1;
            pos += 1;
        }
    }

    /// Scan DOCTYPE name (after `<!DOCTYPE`).
    /// Skips leading whitespace, reads the name, then transitions to DoctypeContent.
    ///
    /// `name_start` uses `usize::MAX` as sentinel meaning "haven't found name start yet".
    fn scan_doctype_name<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        name_start: usize,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        // Phase 1: require whitespace after DOCTYPE keyword, then find the name start.
        // name_start == usize::MAX means we just matched "DOCTYPE" and need at least
        // one whitespace char. name_start == usize::MAX - 1 means we saw whitespace
        // and are skipping to the name.
        if name_start >= usize::MAX - 1 {
            if name_start == usize::MAX {
                // Must see at least one whitespace character after DOCTYPE
                let abs = block_offset + pos;
                if abs >= buf.len() {
                    return Ok(block_len);
                }
                let byte = buf[abs];
                if !is_xml_whitespace(byte) {
                    return Err(ParseError::Xml(Error {
                        kind: ErrorKind::DoctypeMissingWhitespace,
                        offset: stream_offset + abs as u64,
                    }));
                }
                // Got whitespace - transition to "skipping whitespace" sentinel
                self.state = ParserState::DoctypeName { name_start: usize::MAX - 1 };
                return Ok(pos + 1);
            }

            // name_start == usize::MAX - 1: skip remaining whitespace to find name
            let non_ws = !masks.whitespace >> pos;
            if non_ws == 0 {
                return Ok(block_len);
            }
            let next = non_ws.trailing_zeros() as usize;
            if pos + next >= block_len {
                return Ok(block_len);
            }
            let new_abs = block_offset + pos + next;
            let byte = buf[new_abs];
            if byte == b'>' || !is_name_start_byte(byte) {
                // DOCTYPE with no name or invalid name start
                return Err(ParseError::Xml(Error {
                    kind: ErrorKind::DoctypeMissingName,
                    offset: stream_offset + new_abs as u64,
                }));
            }
            // Found name start - update state and continue scanning in same call
            self.state = ParserState::DoctypeName { name_start: new_abs };
            // Scan for name end from this position
            let shifted2 = masks.name_end >> (pos + next);
            if shifted2 == 0 {
                return Ok(block_len);
            }
            let next2 = shifted2.trailing_zeros() as usize;
            if pos + next + next2 >= block_len {
                return Ok(block_len);
            }
            let end_abs = block_offset + pos + next + next2;
            return self.finish_doctype_name(buf, pos + next + next2, end_abs, new_abs, stream_offset, visitor);
        }

        // Phase 2: scan for name end
        let Some((next, end_abs)) =
            find_name_end(masks.name_end, pos, block_offset, block_len)
        else {
            check_name_length(block_offset + block_len, name_start, stream_offset)?;
            return Ok(block_len);
        };
        self.finish_doctype_name(buf, next, end_abs, name_start, stream_offset, visitor)
    }

    /// Emit doctype_start and transition to DoctypeContent or close.
    fn finish_doctype_name<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_rel_pos: usize,
        end_abs: usize,
        name_start: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let name = &buf[name_start..end_abs];
        if name.len() > MAX_NAME_LENGTH {
            return Err(ParseError::Xml(Error {
                kind: ErrorKind::NameTooLong,
                offset: stream_offset + name_start as u64,
            }));
        }
        let name_span = Span::new(
            stream_offset + name_start as u64,
            stream_offset + end_abs as u64,
        );
        visitor
            .doctype_start(name, name_span)
            .map_err(ParseError::Visitor)?;

        let byte = buf[end_abs];
        if byte == b'>' {
            let end_span = Span::new(
                stream_offset + end_abs as u64,
                stream_offset + end_abs as u64 + 1,
            );
            visitor
                .doctype_end(end_span)
                .map_err(ParseError::Visitor)?;
            self.finish_markup();
            Ok(block_rel_pos + 1)
        } else {
            // Whitespace or '[' - transition to content scanning
            self.markup_stream_offset = Some(stream_offset + self.markup_start.unwrap() as u64);
            self.markup_start = None;
            self.content_start = Some(end_abs + 1);
            self.state = ParserState::DoctypeContent {
                depth: 0,
                sub: DoctypeSubState::Normal,
            };
            Ok(block_rel_pos + 1)
        }
    }

    /// Scan DOCTYPE content, looking for `>` with balanced bracket depth.
    ///
    /// Tracks comment (`<!-- -->`), PI (`<? ?>`), and quoted string (`"`, `'`)
    /// contexts so that `[`, `]`, and `>` inside those constructs are not
    /// misinterpreted as structural delimiters.
    fn scan_doctype_content<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        mut pos: usize,
        stream_offset: u64,
        mut depth: u32,
        mut sub: DoctypeSubState,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let content_start = self.content_start.unwrap();

        while pos < block_len {
            let abs = block_offset + pos;
            let byte = buf[abs];

            match sub {
                DoctypeSubState::Normal => match byte {
                    #[cfg(feature = "dtd")]
                    b'[' if depth == 0 => {
                        // Enter DTD internal subset tokenizer
                        if abs > content_start {
                            let span = Span::new(
                                stream_offset + content_start as u64,
                                stream_offset + abs as u64,
                            );
                            visitor
                                .doctype_content(&buf[content_start..abs], span)
                                .map_err(ParseError::Visitor)?;
                        }
                        let bracket_span = Span::new(
                            stream_offset + abs as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor
                            .doctype_internal_subset_start(bracket_span)
                            .map_err(ParseError::Visitor)?;
                        self.content_start = None;
                        self.dtd_phase = DtdPhase::Idle;
                        self.state = ParserState::DtdInternalSubset;
                        return Ok(pos + 1);
                    }
                    b'[' => {
                        if depth > 0 {
                            // `[` inside the internal subset (at top level) is not
                            // valid well-formed XML.  Conditional sections
                            // (`<![INCLUDE[…]]>`) appear only in external DTD
                            // subsets, never in internal subsets.
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedKeyword(b'['),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                        depth = 1;
                    }
                    b']' if depth > 0 => {
                        depth = 0;
                    }
                    b'>' => {
                        if depth == 0 {
                            // End of DOCTYPE
                            if abs > content_start {
                                let span = Span::new(
                                    stream_offset + content_start as u64,
                                    stream_offset + abs as u64,
                                );
                                visitor
                                    .doctype_content(&buf[content_start..abs], span)
                                    .map_err(ParseError::Visitor)?;
                            }
                            let end_span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor
                                .doctype_end(end_span)
                                .map_err(ParseError::Visitor)?;
                            self.finish_content_body();
                            return Ok(pos + 1);
                        }
                    }
                    b'<' => sub = DoctypeSubState::AfterLt,
                    b'"' => sub = DoctypeSubState::DoubleQuoted,
                    b'\'' => sub = DoctypeSubState::SingleQuoted,
                    _ => {}
                },

                DoctypeSubState::AfterLt => match byte {
                    b'!' => sub = DoctypeSubState::AfterLtBang,
                    b'?' => sub = DoctypeSubState::PI { saw_qmark: false },
                    _ => { sub = DoctypeSubState::Normal; continue; }
                },

                DoctypeSubState::AfterLtBang => match byte {
                    b'-' => sub = DoctypeSubState::AfterLtBangDash,
                    _ => { sub = DoctypeSubState::Normal; continue; }
                },

                DoctypeSubState::AfterLtBangDash => match byte {
                    b'-' => sub = DoctypeSubState::Comment { dash_count: 0 },
                    _ => { sub = DoctypeSubState::Normal; continue; }
                },

                DoctypeSubState::Comment { ref mut dash_count } => match byte {
                    b'-' => *dash_count = dash_count.saturating_add(1),
                    b'>' if *dash_count >= 2 => sub = DoctypeSubState::Normal,
                    _ => *dash_count = 0,
                },

                DoctypeSubState::PI { ref mut saw_qmark } => match byte {
                    b'?' => *saw_qmark = true,
                    b'>' if *saw_qmark => sub = DoctypeSubState::Normal,
                    _ => *saw_qmark = false,
                },

                DoctypeSubState::DoubleQuoted => {
                    if byte == b'"' { sub = DoctypeSubState::Normal; }
                }

                DoctypeSubState::SingleQuoted => {
                    if byte == b'\'' { sub = DoctypeSubState::Normal; }
                }
            }

            pos += 1;
        }

        self.state = ParserState::DoctypeContent { depth, sub };
        Ok(block_len)
    }

    /// Maximum size for XML declaration content buffer.
    const XML_DECL_BUF_LIMIT: usize = 256;

    /// Emit PI content, or buffer it if we're inside an XML declaration.
    fn emit_pi_content<V: Visitor>(
        &mut self,
        content: &[u8],
        span: Span,
        visitor: &mut V,
    ) -> Result<(), ParseError<V::Error>> {
        if self.in_xml_decl {
            let new_len = self.xml_decl_buf_len + content.len();
            if new_len > Self::XML_DECL_BUF_LIMIT {
                return Err(ParseError::Xml(Error {
                    kind: ErrorKind::MalformedXmlDeclaration,
                    offset: span.start,
                }));
            }
            self.xml_decl_buf[self.xml_decl_buf_len..new_len].copy_from_slice(content);
            self.xml_decl_buf_len = new_len;
            Ok(())
        } else {
            visitor.pi_content(content, span).map_err(ParseError::Visitor)
        }
    }

    /// Emit PI end, or parse the buffered XML declaration and emit xml_declaration.
    fn emit_pi_end<V: Visitor>(
        &mut self,
        end_span: Span,
        visitor: &mut V,
    ) -> Result<(), ParseError<V::Error>> {
        if self.in_xml_decl {
            self.in_xml_decl = false;
            let decl_span = Span::new(self.xml_decl_span_start, end_span.end);
            let len = self.xml_decl_buf_len;
            self.xml_decl_buf_len = 0;
            let (version, encoding, standalone) =
                parse_xml_decl(&self.xml_decl_buf[..len], self.xml_decl_span_start)?;
            visitor
                .xml_declaration(version, encoding, standalone, decl_span)
                .map_err(ParseError::Visitor)
        } else {
            visitor.pi_end(end_span).map_err(ParseError::Visitor)
        }
    }

    /// Scan PI target name after `<?`.
    fn scan_pi_target<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        name_start: usize,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let Some((next, abs)) =
            find_name_end(masks.name_end, pos, block_offset, block_len)
        else {
            check_name_length(block_offset + block_len, name_start, stream_offset)?;
            return Ok(block_len);
        };

        let name = validate_name(buf, name_start, abs, stream_offset)?;
        let name_span = Span::new(
            stream_offset + name_start as u64,
            stream_offset + abs as u64,
        );

        // Check if this is an XML declaration or a reserved PI target
        let is_xml_target = name.eq_ignore_ascii_case(b"xml");
        if is_xml_target {
            if self.had_markup {
                // <?xml ...?> after document start is an error
                return Err(ParseError::Xml(Error {
                    kind: ErrorKind::ReservedPITarget,
                    offset: stream_offset + name_start as u64,
                }));
            }
            // Enter XML declaration mode
            self.in_xml_decl = true;
            self.xml_decl_buf_len = 0;
            self.xml_decl_span_start = stream_offset + self.markup_start.unwrap() as u64;
        } else {
            visitor
                .pi_start(name, name_span)
                .map_err(ParseError::Visitor)?;
        }

        let byte = buf[abs];
        if byte == b'?' {
            // Check for '?>' immediately
            let gt_pos = abs + 1;
            if gt_pos < buf.len() && buf[gt_pos] == b'>' {
                let end_span = Span::new(
                    stream_offset + abs as u64,
                    stream_offset + gt_pos as u64 + 1,
                );
                self.emit_pi_end(end_span, visitor)?;
                self.finish_markup();
                Ok(next + 2)
            } else {
                self.markup_stream_offset = Some(stream_offset + self.markup_start.unwrap() as u64);
                self.markup_start = None;
                self.content_start = Some(abs + 1);
                self.state = ParserState::PIContent {
                    saw_qmark: true,
                };
                Ok(next + 1)
            }
        } else {
            // Whitespace separates target from content
            self.markup_stream_offset = Some(stream_offset + self.markup_start.unwrap() as u64);
            self.markup_start = None;
            self.content_start = Some(abs + 1);
            self.state = ParserState::PIContent {
                saw_qmark: false,
            };
            Ok(next + 1)
        }
    }

    /// Scan PI content, looking for `?>`.
    fn scan_pi_content<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        mut pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        mut saw_qmark: bool,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let content_start = self.content_start.unwrap();
        loop {
            // If we had a '?' from a previous boundary, check if '>' follows
            if saw_qmark {
                if pos >= block_len {
                    self.state = ParserState::PIContent { saw_qmark: true };
                    return Ok(block_len);
                }
                let abs = block_offset + pos;
                let byte = buf[abs];
                if byte == b'>' {
                    // '?>' found
                    let content_end = abs - 1;
                    if content_end > content_start {
                        let span = Span::new(
                            stream_offset + content_start as u64,
                            stream_offset + content_end as u64,
                        );
                        self.emit_pi_content(&buf[content_start..content_end], span, visitor)?;
                    }
                    let end_span = Span::new(
                        stream_offset + abs as u64 - 1,
                        stream_offset + abs as u64 + 1,
                    );
                    self.emit_pi_end(end_span, visitor)?;
                    self.finish_content_body();
                    return Ok(pos + 1);
                }
                saw_qmark = false;
                if byte == b'?' {
                    saw_qmark = true;
                    pos += 1;
                    continue;
                }
                pos += 1;
                continue;
            }

            if pos >= block_len {
                self.state = ParserState::PIContent { saw_qmark: false };
                return Ok(block_len);
            }

            let shifted = masks.qmark >> pos;
            if shifted == 0 {
                self.state = ParserState::PIContent {
                    saw_qmark: false,
                };
                return Ok(block_len);
            }

            let next = shifted.trailing_zeros() as usize;
            if pos + next >= block_len {
                self.state = ParserState::PIContent {
                    saw_qmark: false,
                };
                return Ok(block_len);
            }

            let qmark_abs = block_offset + pos + next;
            let gt_pos = qmark_abs + 1;
            if gt_pos < buf.len() {
                if buf[gt_pos] == b'>' {
                    // '?>' found
                    let content_end = qmark_abs;
                    if content_end > content_start {
                        let span = Span::new(
                            stream_offset + content_start as u64,
                            stream_offset + content_end as u64,
                        );
                        self.emit_pi_content(&buf[content_start..content_end], span, visitor)?;
                    }
                    let end_span = Span::new(
                        stream_offset + qmark_abs as u64,
                        stream_offset + gt_pos as u64 + 1,
                    );
                    self.emit_pi_end(end_span, visitor)?;
                    self.finish_content_body();
                    return Ok(pos + next + 2);
                }
                // '?' not followed by '>' - continue
                pos = pos + next + 1;
            } else {
                // '?' at buffer end - save state
                self.state = ParserState::PIContent {
                    saw_qmark: true,
                };
                return Ok(pos + next + 1);
            }
        }
    }

    /// Scan entity reference after `&` in content.
    fn scan_entity_ref<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        name_start: usize,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        // Check if this is a character reference (&#)
        let abs = block_offset + pos;
        if abs == name_start && abs < buf.len() && buf[abs] == b'#' {
            self.state = ParserState::CharRef {
                value_start: abs + 1,
            };
            return Ok(pos + 1);
        }

        let Some((name, span, next_pos)) = find_and_validate_entity_name(
            buf, block_offset, block_len, pos, masks.semicolon,
            stream_offset, name_start,
        )? else {
            return Ok(block_len);
        };

        visitor
            .entity_ref(name, span)
            .map_err(ParseError::Visitor)?;

        self.finish_markup();
        Ok(next_pos)
    }

    /// Scan character reference after `&#` or `&#x` in content.
    fn scan_char_ref<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        value_start: usize,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let Some((value, span, next_pos)) = find_and_validate_char_ref(
            buf, block_offset, block_len, pos, masks.semicolon,
            stream_offset, value_start,
        )? else {
            return Ok(block_len);
        };

        visitor
            .char_ref(value, span)
            .map_err(ParseError::Visitor)?;

        self.finish_markup();
        Ok(next_pos)
    }

    /// Scan entity reference after `&` in an attribute value.
    fn scan_attr_entity_ref<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        name_start: usize,
        quote: QuoteStyle,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        // Check if this is a character reference (&#)
        let abs = block_offset + pos;
        if abs == name_start && abs < buf.len() && buf[abs] == b'#' {
            self.state = ParserState::AttrCharRef {
                value_start: abs + 1,
                quote,
            };
            return Ok(pos + 1);
        }

        let Some((name, span, next_pos)) = find_and_validate_entity_name(
            buf, block_offset, block_len, pos, masks.semicolon,
            stream_offset, name_start,
        )? else {
            return Ok(block_len);
        };

        visitor
            .attribute_entity_ref(name, span)
            .map_err(ParseError::Visitor)?;

        self.markup_start = None;
        self.content_start = Some(block_offset + next_pos);
        self.state = ParserState::AttrValue { quote };
        Ok(next_pos)
    }

    /// Scan character reference after `&#` in an attribute value.
    fn scan_attr_char_ref<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        pos: usize,
        masks: &CharClassMasks,
        stream_offset: u64,
        value_start: usize,
        quote: QuoteStyle,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        let Some((value, span, next_pos)) = find_and_validate_char_ref(
            buf, block_offset, block_len, pos, masks.semicolon,
            stream_offset, value_start,
        )? else {
            return Ok(block_len);
        };

        visitor
            .attribute_char_ref(value, span)
            .map_err(ParseError::Visitor)?;

        self.markup_start = None;
        self.content_start = Some(block_offset + next_pos);
        self.state = ParserState::AttrValue { quote };
        Ok(next_pos)
    }

    // ======================================================================
    // DTD Internal Subset Tokenizer (feature = "dtd")
    // ======================================================================

    /// Maximum length for system/public identifier literals.
    #[cfg(feature = "dtd")]
    const MAX_LITERAL_LENGTH: usize = 8_192;

    /// Maximum parenthesis nesting depth in content models and enumerated types.
    #[cfg(feature = "dtd")]
    const MAX_PAREN_DEPTH: u32 = 256;

    /// Scan the DTD internal subset. Dispatches on `self.dtd_phase`.
    #[cfg(feature = "dtd")]
    fn scan_dtd_internal_subset<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        mut pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        while pos < block_len {
            let abs = block_offset + pos;
            let byte = buf[abs];

            match self.dtd_phase {
                DtdPhase::Idle => match byte {
                    b']' => {
                        let span = Span::new(
                            stream_offset + abs as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor
                            .doctype_internal_subset_end(span)
                            .map_err(ParseError::Visitor)?;
                        self.state = ParserState::DoctypeAfterSubset;
                        return Ok(pos + 1);
                    }
                    b'<' => {
                        self.markup_start = Some(abs);
                        self.dtd_phase = DtdPhase::AfterLt;
                    }
                    b'%' => {
                        self.markup_start = Some(abs);
                        self.dtd_phase = DtdPhase::PeRefName { name_start: abs + 1 };
                    }
                    b if is_xml_whitespace(b) => { /* skip */ }
                    _ => {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedName(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                },

                DtdPhase::AfterLt => match byte {
                    b'!' => self.dtd_phase = DtdPhase::AfterLtBang,
                    b'?' => {
                        // markup_start already set in Idle when we saw '<'
                        self.dtd_phase = DtdPhase::PITarget { name_start: abs + 1 };
                    }
                    _ => {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedKeyword(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                },

                DtdPhase::AfterLtBang => {
                    let lt_offset = self.markup_start.map(|s| stream_offset + s as u64)
                        .unwrap_or(stream_offset + abs as u64);
                    match byte {
                    b'-' => self.dtd_phase = DtdPhase::AfterLtBangDash,
                    b'E' => {
                        // Could be ELEMENT or ENTITY
                        // markup_start already set in Idle when we saw '<'
                        self.dtd_phase = DtdPhase::MatchKeyword {
                            kind: DtdDeclKind::Element, // tentative, refined at byte 2
                            matched: 1, // 'E' matched
                        };
                    }
                    b'A' => {
                        // markup_start already set in Idle
                        self.dtd_phase = DtdPhase::MatchKeyword {
                            kind: DtdDeclKind::Attlist,
                            matched: 1,
                        };
                    }
                    b'N' => {
                        // markup_start already set in Idle
                        self.dtd_phase = DtdPhase::MatchKeyword {
                            kind: DtdDeclKind::Notation,
                            matched: 1,
                        };
                    }
                    b'[' => {
                        // Conditional section — opaque scan (not yet supported, error for now)
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdInvalidMarkup,
                            offset: lt_offset,
                        }));
                    }
                    _ => {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdInvalidMarkup,
                            offset: lt_offset,
                        }));
                    }
                    }
                },

                DtdPhase::AfterLtBangDash => {
                    let lt_offset = self.markup_start.map(|s| stream_offset + s as u64)
                        .unwrap_or(stream_offset + abs as u64);
                    match byte {
                        b'-' => {
                            // Enter comment — clear markup_start so content body
                            // streaming works correctly at buffer boundaries.
                            let span = Span::new(
                                lt_offset,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.comment_start(span).map_err(ParseError::Visitor)?;
                            self.markup_stream_offset = Some(lt_offset);
                            self.markup_start = None;
                            self.content_start = Some(abs + 1);
                            self.dtd_phase = DtdPhase::Comment { dash_count: 0 };
                        }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::DtdInvalidMarkup,
                                offset: lt_offset,
                            }));
                        }
                    }
                }

                DtdPhase::Comment { ref mut dash_count } => match byte {
                    b'-' => *dash_count = dash_count.saturating_add(1),
                    b'>' if *dash_count >= 2 => {
                        // End of comment
                        let content_start = self.content_start.unwrap();
                        let content_end = abs - 2; // exclude "-->"
                        if content_end > content_start {
                            let span = Span::new(
                                stream_offset + content_start as u64,
                                stream_offset + content_end as u64,
                            );
                            visitor.comment_content(&buf[content_start..content_end], span)
                                .map_err(ParseError::Visitor)?;
                        }
                        let end_span = Span::new(
                            stream_offset + (abs - 2) as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.comment_end(end_span).map_err(ParseError::Visitor)?;
                        self.content_start = None;
                        self.markup_stream_offset = None;
                        self.dtd_phase = DtdPhase::Idle;
                    }
                    _ => {
                        if *dash_count >= 2 {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::DoubleDashInComment,
                                offset: stream_offset + (abs - 2) as u64,
                            }));
                        }
                        *dash_count = 0;
                    }
                },

                DtdPhase::PITarget { name_start } => {
                    if is_xml_whitespace(byte) || byte == b'?' {
                        // End of PI target name
                        if abs == name_start {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedName(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                        let name = &buf[name_start..abs];
                        if name.len() > MAX_NAME_LENGTH {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::NameTooLong,
                                offset: stream_offset + name_start as u64,
                            }));
                        }
                        let span = Span::new(
                            stream_offset + self.markup_start.unwrap() as u64,
                            stream_offset + abs as u64,
                        );
                        visitor.pi_start(name, span).map_err(ParseError::Visitor)?;
                        // Clear markup_start for content body streaming.
                        self.markup_stream_offset = Some(stream_offset + self.markup_start.unwrap() as u64);
                        self.markup_start = None;
                        if byte == b'?' {
                            self.dtd_phase = DtdPhase::PIContent { saw_qmark: true };
                        } else {
                            self.dtd_phase = DtdPhase::PIContent { saw_qmark: false };
                        }
                        self.content_start = Some(abs + 1);
                    } else if !is_name_byte(byte) {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedName(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                },

                DtdPhase::PIContent { ref mut saw_qmark } => match byte {
                    b'?' => *saw_qmark = true,
                    b'>' if *saw_qmark => {
                        // End of PI
                        let content_start = self.content_start.unwrap();
                        let content_end = abs - 1; // exclude "?>"
                        if content_end > content_start {
                            let span = Span::new(
                                stream_offset + content_start as u64,
                                stream_offset + content_end as u64,
                            );
                            visitor.pi_content(&buf[content_start..content_end], span)
                                .map_err(ParseError::Visitor)?;
                        }
                        let end_span = Span::new(
                            stream_offset + (abs - 1) as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.pi_end(end_span).map_err(ParseError::Visitor)?;
                        self.content_start = None;
                        self.markup_stream_offset = None;
                        self.dtd_phase = DtdPhase::Idle;
                    }
                    _ => *saw_qmark = false,
                },

                DtdPhase::MatchKeyword { kind, matched } => {
                    let target = match kind {
                        DtdDeclKind::Element => b"ELEMENT" as &[u8],
                        DtdDeclKind::Attlist => b"ATTLIST",
                        DtdDeclKind::Entity => b"ENTITY",
                        DtdDeclKind::Notation => b"NOTATION",
                    };

                    // At matched == 1 for 'E', we need to disambiguate ELEMENT vs ENTITY
                    if kind == DtdDeclKind::Element && matched == 1 {
                        if byte == b'L' {
                            // Still ELEMENT
                            self.dtd_phase = DtdPhase::MatchKeyword { kind: DtdDeclKind::Element, matched: 2 };
                        } else if byte == b'N' {
                            // Switch to ENTITY
                            self.dtd_phase = DtdPhase::MatchKeyword { kind: DtdDeclKind::Entity, matched: 2 };
                        } else {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::DtdInvalidMarkup,
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    } else if (matched as usize) < target.len() {
                        if byte == target[matched as usize] {
                            let new_matched = matched + 1;
                            if (new_matched as usize) == target.len() {
                                // Full keyword matched — require whitespace next
                                match kind {
                                    DtdDeclKind::Element => self.dtd_phase = DtdPhase::ElementRequireWs,
                                    DtdDeclKind::Attlist => self.dtd_phase = DtdPhase::AttlistRequireWs,
                                    DtdDeclKind::Entity => self.dtd_phase = DtdPhase::EntityRequireWs,
                                    DtdDeclKind::Notation => self.dtd_phase = DtdPhase::NotationRequireWs,
                                }
                            } else {
                                self.dtd_phase = DtdPhase::MatchKeyword { kind, matched: new_matched };
                            }
                        } else {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::DtdInvalidMarkup,
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                },

                // --- ELEMENT declaration ---
                DtdPhase::ElementRequireWs => {
                    if !is_xml_whitespace(byte) {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingWhitespace,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                    self.dtd_phase = DtdPhase::ElementBeforeName;
                }
                DtdPhase::ElementBeforeName => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if is_name_start_byte(byte) {
                        self.dtd_phase = DtdPhase::ElementName { name_start: abs };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingName,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::ElementName { name_start } => {
                    if !is_name_byte(byte) {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + name_start as u64,
                            stream_offset + abs as u64,
                        );
                        visitor.element_decl_start(name, span).map_err(ParseError::Visitor)?;
                        self.dtd_phase = DtdPhase::ElementAfterName;
                        continue; // reprocess this byte
                    }
                }
                DtdPhase::ElementAfterName => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'(' {
                        // Content model
                        self.content_start = Some(abs);
                        self.dtd_phase = DtdPhase::ElementContentModel { paren_depth: 1 };
                    } else if byte == b'E' || byte == b'A' {
                        // EMPTY or ANY
                        self.dtd_phase = DtdPhase::ElementContentSpecKeyword { matched: 1 };
                        // We store the start of the keyword for the span
                        self.content_start = Some(abs);
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedKeyword(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::ElementContentSpecKeyword { matched } => {
                    // We're matching EMPTY or ANY
                    let content_start = self.content_start.unwrap();
                    let first = buf[content_start];
                    let target: &[u8] = if first == b'E' { b"EMPTY" } else { b"ANY" };
                    if (matched as usize) < target.len() {
                        if byte == target[matched as usize] {
                            let new_matched = matched + 1;
                            if (new_matched as usize) == target.len() {
                                let span = Span::new(
                                    stream_offset + content_start as u64,
                                    stream_offset + abs as u64 + 1,
                                );
                                if first == b'E' {
                                    visitor.element_decl_empty(span).map_err(ParseError::Visitor)?;
                                } else {
                                    visitor.element_decl_any(span).map_err(ParseError::Visitor)?;
                                }
                                self.content_start = None;
                                self.dtd_phase = DtdPhase::ElementAfterContentSpec;
                            } else {
                                self.dtd_phase = DtdPhase::ElementContentSpecKeyword { matched: new_matched };
                            }
                        } else {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedKeyword(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }
                DtdPhase::ElementContentModel { ref mut paren_depth } => {
                    match byte {
                        b'(' => {
                            *paren_depth += 1;
                            if *paren_depth > Self::MAX_PAREN_DEPTH {
                                return Err(ParseError::Xml(Error {
                                    kind: ErrorKind::DtdParensTooDeep,
                                    offset: stream_offset + abs as u64,
                                }));
                            }
                        }
                        b')' => {
                            *paren_depth -= 1;
                            if *paren_depth == 0 {
                                // Check for trailing multiplicity character
                                // We'll emit content_spec including everything up to and
                                // including the closing `)` + optional `?`/`*`/`+` in the
                                // next state.
                                // But first, peek ahead for `?`/`*`/`+`
                                let content_start = self.content_start.unwrap();
                                if abs + 1 < buf.len() {
                                    let next_byte = buf[abs + 1];
                                    let end = if matches!(next_byte, b'?' | b'*' | b'+') {
                                        abs + 2
                                    } else {
                                        abs + 1
                                    };
                                    let span = Span::new(
                                        stream_offset + content_start as u64,
                                        stream_offset + end as u64,
                                    );
                                    visitor.element_decl_content_spec(
                                        &buf[content_start..end], span,
                                    ).map_err(ParseError::Visitor)?;
                                    self.content_start = None;
                                    self.dtd_phase = DtdPhase::ElementAfterContentSpec;
                                    pos = end - block_offset;
                                    continue;
                                } else {
                                    // Buffer boundary — flush what we have, handle multiplicity later
                                    let span = Span::new(
                                        stream_offset + content_start as u64,
                                        stream_offset + abs as u64 + 1,
                                    );
                                    visitor.element_decl_content_spec(
                                        &buf[content_start..abs + 1], span,
                                    ).map_err(ParseError::Visitor)?;
                                    self.content_start = None;
                                    self.dtd_phase = DtdPhase::ElementAfterContentSpec;
                                }
                            }
                        }
                        _ => { /* keep scanning */ }
                    }
                }
                DtdPhase::ElementAfterContentSpec => {
                    match byte {
                        b'?' | b'*' | b'+' => {
                            // Trailing multiplicity after content model — emit as content_spec
                            // (only hit if we were at buffer boundary after `)`)
                            if self.content_start.is_none() {
                                let span = Span::new(
                                    stream_offset + abs as u64,
                                    stream_offset + abs as u64 + 1,
                                );
                                visitor.element_decl_content_spec(
                                    &buf[abs..abs + 1], span,
                                ).map_err(ParseError::Visitor)?;
                            }
                        }
                        b'>' => {
                            let span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.element_decl_end(span).map_err(ParseError::Visitor)?;
                            self.markup_start = None;
                            self.dtd_phase = DtdPhase::Idle;
                        }
                        b if is_xml_whitespace(b) => { /* skip */ }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedClose(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }

                // --- PE reference in internal subset ---
                DtdPhase::PeRefName { name_start } => {
                    if byte == b';' {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + (name_start - 1) as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.dtd_pe_reference(name, span).map_err(ParseError::Visitor)?;
                        self.markup_start = None;
                        self.dtd_phase = DtdPhase::Idle;
                    } else {
                        check_ref_name_byte(byte, abs, name_start, stream_offset)?;
                    }
                }

                // --- ENTITY declaration ---
                DtdPhase::EntityRequireWs => {
                    if !is_xml_whitespace(byte) {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingWhitespace,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                    self.dtd_phase = DtdPhase::EntityCheckPercent;
                }
                DtdPhase::EntityCheckPercent => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'%' {
                        self.dtd_phase = DtdPhase::EntityPercentRequireWs;
                    } else if is_name_start_byte(byte) {
                        self.dtd_phase = DtdPhase::EntityName { name_start: abs, kind: EntityKind::General };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingName,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::EntityPercentRequireWs => {
                    if !is_xml_whitespace(byte) {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingWhitespace,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                    self.dtd_phase = DtdPhase::EntityBeforeName { kind: EntityKind::Parameter };
                }
                DtdPhase::EntityBeforeName { kind } => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if is_name_start_byte(byte) {
                        self.dtd_phase = DtdPhase::EntityName { name_start: abs, kind };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingName,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::EntityName { name_start, kind } => {
                    if !is_name_byte(byte) {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + name_start as u64,
                            stream_offset + abs as u64,
                        );
                        visitor.entity_decl_start(name, kind, span)
                            .map_err(ParseError::Visitor)?;
                        self.dtd_phase = DtdPhase::EntityBeforeDef { kind };
                        continue;
                    }
                }
                DtdPhase::EntityBeforeDef { kind } => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else {
                        self.dtd_phase = DtdPhase::EntityDefStart { kind };
                        continue;
                    }
                }
                DtdPhase::EntityDefStart { kind } => {
                    match byte {
                        b'"' | b'\'' => {
                            // Internal entity value
                            let quote = if byte == b'"' { QuoteStyle::Double } else { QuoteStyle::Single };
                            self.content_start = Some(abs + 1);
                            self.dtd_phase = DtdPhase::EntityValue { quote };
                        }
                        b'S' => {
                            let ctx = DtdDeclContext::Entity { kind };
                            self.dtd_phase = DtdPhase::ExternalIdSystemKw { ctx, matched: 1 };
                        }
                        b'P' => {
                            let ctx = DtdDeclContext::Entity { kind };
                            self.dtd_phase = DtdPhase::ExternalIdPublicKw { ctx, matched: 1 };
                        }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedQuote(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }
                DtdPhase::EntityValue { quote } => {
                    let delim = if quote == QuoteStyle::Double { b'"' } else { b'\'' };
                    match byte {
                        b if b == delim => {
                            // End of entity value
                            let content_start = self.content_start.unwrap();
                            if abs > content_start {
                                let span = Span::new(
                                    stream_offset + content_start as u64,
                                    stream_offset + abs as u64,
                                );
                                visitor.entity_decl_value(&buf[content_start..abs], span)
                                    .map_err(ParseError::Visitor)?;
                            }
                            let end_span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.entity_decl_value_end(end_span).map_err(ParseError::Visitor)?;
                            self.content_start = None;
                            self.dtd_phase = DtdPhase::EntityBeforeClose;
                        }
                        b'&' => {
                            // Entity or char ref in entity value
                            let content_start = self.content_start.unwrap();
                            if abs > content_start {
                                let span = Span::new(
                                    stream_offset + content_start as u64,
                                    stream_offset + abs as u64,
                                );
                                visitor.entity_decl_value(&buf[content_start..abs], span)
                                    .map_err(ParseError::Visitor)?;
                            }
                            // Look ahead to see if it's &#
                            if abs + 1 < buf.len() && buf[abs + 1] == b'#' {
                                self.dtd_phase = DtdPhase::EntityValueCharRef {
                                    value_start: abs + 2,
                                    quote,
                                };
                                pos += 1; // skip '#'
                            } else {
                                self.dtd_phase = DtdPhase::EntityValueEntityRef {
                                    name_start: abs + 1,
                                    quote,
                                };
                            }
                            self.content_start = None;
                        }
                        b'%' => {
                            // PE reference in entity value
                            let content_start = self.content_start.unwrap();
                            if abs > content_start {
                                let span = Span::new(
                                    stream_offset + content_start as u64,
                                    stream_offset + abs as u64,
                                );
                                visitor.entity_decl_value(&buf[content_start..abs], span)
                                    .map_err(ParseError::Visitor)?;
                            }
                            self.content_start = None;
                            self.dtd_phase = DtdPhase::EntityValuePeRef {
                                name_start: abs + 1,
                                quote,
                            };
                        }
                        _ => { /* keep scanning */ }
                    }
                }
                DtdPhase::EntityValueEntityRef { name_start, quote } => {
                    if byte == b';' {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + (name_start - 1) as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.entity_decl_entity_ref(name, span).map_err(ParseError::Visitor)?;
                        self.content_start = Some(abs + 1);
                        self.dtd_phase = DtdPhase::EntityValue { quote };
                    } else if abs == name_start && byte == b'#' {
                        // `&` was at buffer boundary, `#` arrived now → char ref
                        self.dtd_phase = DtdPhase::EntityValueCharRef {
                            value_start: abs + 1,
                            quote,
                        };
                    } else {
                        check_ref_name_byte(byte, abs, name_start, stream_offset)?;
                    }
                }
                DtdPhase::EntityValueCharRef { value_start, quote } => {
                    if byte == b';' {
                        let value = validate_char_ref(buf, value_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + (value_start - 2) as u64, // from `&#`
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.entity_decl_char_ref(value, span).map_err(ParseError::Visitor)?;
                        self.content_start = Some(abs + 1);
                        self.dtd_phase = DtdPhase::EntityValue { quote };
                    }
                    // Keep scanning — validation happens above
                }
                DtdPhase::EntityValuePeRef { name_start, quote } => {
                    if byte == b';' {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + (name_start - 1) as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.entity_decl_pe_ref(name, span).map_err(ParseError::Visitor)?;
                        self.content_start = Some(abs + 1);
                        self.dtd_phase = DtdPhase::EntityValue { quote };
                    } else {
                        check_ref_name_byte(byte, abs, name_start, stream_offset)?;
                    }
                }
                DtdPhase::EntityAfterExternalId { kind } => {
                    match byte {
                        b'>' => {
                            let span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.entity_decl_end(span).map_err(ParseError::Visitor)?;
                            self.markup_start = None;
                            self.dtd_phase = DtdPhase::Idle;
                        }
                        b'N' if kind == EntityKind::General => {
                            self.dtd_phase = DtdPhase::EntityNdataKeyword { matched: 1 };
                        }
                        b if is_xml_whitespace(b) => { /* skip */ }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedClose(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }
                DtdPhase::EntityNdataKeyword { matched } => {
                    const NDATA: &[u8] = b"NDATA";
                    if (matched as usize) < NDATA.len() {
                        if byte == NDATA[matched as usize] {
                            let new_matched = matched + 1;
                            if (new_matched as usize) == NDATA.len() {
                                self.dtd_phase = DtdPhase::EntityNdataRequireWs;
                            } else {
                                self.dtd_phase = DtdPhase::EntityNdataKeyword { matched: new_matched };
                            }
                        } else {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedKeyword(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }
                DtdPhase::EntityNdataRequireWs => {
                    if !is_xml_whitespace(byte) {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingWhitespace,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                    // Reuse EntityBeforeName-like scanning for the notation name
                    self.dtd_phase = DtdPhase::EntityNdataName { name_start: usize::MAX };
                }
                DtdPhase::EntityNdataName { name_start } => {
                    if name_start == usize::MAX {
                        if is_xml_whitespace(byte) { /* skip */ }
                        else if is_name_start_byte(byte) {
                            self.dtd_phase = DtdPhase::EntityNdataName { name_start: abs };
                        } else {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::DtdDeclMissingName,
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    } else if !is_name_byte(byte) {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + name_start as u64,
                            stream_offset + abs as u64,
                        );
                        visitor.entity_decl_ndata(name, span).map_err(ParseError::Visitor)?;
                        self.dtd_phase = DtdPhase::EntityBeforeClose;
                        continue;
                    }
                }
                DtdPhase::EntityBeforeClose => {
                    match byte {
                        b'>' => {
                            let span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.entity_decl_end(span).map_err(ParseError::Visitor)?;
                            self.markup_start = None;
                            self.dtd_phase = DtdPhase::Idle;
                        }
                        b if is_xml_whitespace(b) => { /* skip */ }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedClose(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }

                // --- Shared External ID scanning ---
                DtdPhase::ExternalIdSystemKw { ctx, matched } => {
                    const SYSTEM: &[u8] = b"SYSTEM";
                    if (matched as usize) < SYSTEM.len() {
                        if byte == SYSTEM[matched as usize] {
                            let new_matched = matched + 1;
                            if (new_matched as usize) == SYSTEM.len() {
                                self.dtd_phase = DtdPhase::ExternalIdBeforeSystemLit { ctx };
                            } else {
                                self.dtd_phase = DtdPhase::ExternalIdSystemKw { ctx, matched: new_matched };
                            }
                        } else {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedKeyword(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }
                DtdPhase::ExternalIdPublicKw { ctx, matched } => {
                    const PUBLIC: &[u8] = b"PUBLIC";
                    if (matched as usize) < PUBLIC.len() {
                        if byte == PUBLIC[matched as usize] {
                            let new_matched = matched + 1;
                            if (new_matched as usize) == PUBLIC.len() {
                                self.dtd_phase = DtdPhase::ExternalIdBeforePublicLit { ctx };
                            } else {
                                self.dtd_phase = DtdPhase::ExternalIdPublicKw { ctx, matched: new_matched };
                            }
                        } else {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedKeyword(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }
                DtdPhase::ExternalIdBeforeSystemLit { ctx } => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'"' || byte == b'\'' {
                        let quote = if byte == b'"' { QuoteStyle::Double } else { QuoteStyle::Single };
                        self.dtd_phase = DtdPhase::ExternalIdSystemLit { ctx, quote, literal_start: abs + 1 };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedQuote(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::ExternalIdSystemLit { ctx, quote, literal_start } => {
                    let delim = if quote == QuoteStyle::Double { b'"' } else { b'\'' };
                    if byte == delim {
                        let literal = &buf[literal_start..abs];
                        if literal.len() > Self::MAX_LITERAL_LENGTH {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::LiteralTooLong,
                                offset: stream_offset + literal_start as u64,
                            }));
                        }
                        let span = Span::new(
                            stream_offset + literal_start as u64,
                            stream_offset + abs as u64,
                        );
                        match ctx {
                            DtdDeclContext::Entity { .. } => {
                                visitor.entity_decl_system_id(literal, span)
                                    .map_err(ParseError::Visitor)?;
                            }
                            DtdDeclContext::Notation => {
                                visitor.notation_decl_system_id(literal, span)
                                    .map_err(ParseError::Visitor)?;
                            }
                        }
                        match ctx {
                            DtdDeclContext::Entity { kind } => {
                                self.dtd_phase = DtdPhase::EntityAfterExternalId { kind };
                            }
                            DtdDeclContext::Notation => {
                                self.dtd_phase = DtdPhase::NotationBeforeClose;
                            }
                        }
                    }
                }
                DtdPhase::ExternalIdBeforePublicLit { ctx } => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'"' || byte == b'\'' {
                        let quote = if byte == b'"' { QuoteStyle::Double } else { QuoteStyle::Single };
                        self.dtd_phase = DtdPhase::ExternalIdPublicLit { ctx, quote, literal_start: abs + 1 };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedQuote(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::ExternalIdPublicLit { ctx, quote, literal_start } => {
                    let delim = if quote == QuoteStyle::Double { b'"' } else { b'\'' };
                    if byte == delim {
                        let literal = &buf[literal_start..abs];
                        if literal.len() > Self::MAX_LITERAL_LENGTH {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::LiteralTooLong,
                                offset: stream_offset + literal_start as u64,
                            }));
                        }
                        let span = Span::new(
                            stream_offset + literal_start as u64,
                            stream_offset + abs as u64,
                        );
                        match ctx {
                            DtdDeclContext::Entity { .. } => {
                                visitor.entity_decl_public_id(literal, span)
                                    .map_err(ParseError::Visitor)?;
                            }
                            DtdDeclContext::Notation => {
                                visitor.notation_decl_public_id(literal, span)
                                    .map_err(ParseError::Visitor)?;
                            }
                        }
                        self.dtd_phase = DtdPhase::ExternalIdBetweenLiterals { ctx };
                    }
                }
                DtdPhase::ExternalIdBetweenLiterals { ctx } => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'"' || byte == b'\'' {
                        // System literal follows public literal
                        let quote = if byte == b'"' { QuoteStyle::Double } else { QuoteStyle::Single };
                        self.dtd_phase = DtdPhase::ExternalIdSystemLit { ctx, quote, literal_start: abs + 1 };
                    } else if byte == b'>' {
                        // PUBLIC with no system literal (NOTATION only allows this)
                        match ctx {
                            DtdDeclContext::Notation => {
                                let span = Span::new(
                                    stream_offset + abs as u64,
                                    stream_offset + abs as u64 + 1,
                                );
                                visitor.notation_decl_end(span).map_err(ParseError::Visitor)?;
                                self.markup_start = None;
                                self.dtd_phase = DtdPhase::Idle;
                            }
                            _ => {
                                return Err(ParseError::Xml(Error {
                                    kind: ErrorKind::ExpectedQuote(byte),
                                    offset: stream_offset + abs as u64,
                                }));
                            }
                        }
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedQuote(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }

                // --- NOTATION declaration ---
                DtdPhase::NotationRequireWs => {
                    if !is_xml_whitespace(byte) {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingWhitespace,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                    self.dtd_phase = DtdPhase::NotationBeforeName;
                }
                DtdPhase::NotationBeforeName => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if is_name_start_byte(byte) {
                        self.dtd_phase = DtdPhase::NotationName { name_start: abs };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingName,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::NotationName { name_start } => {
                    if !is_name_byte(byte) {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + name_start as u64,
                            stream_offset + abs as u64,
                        );
                        visitor.notation_decl_start(name, span).map_err(ParseError::Visitor)?;
                        self.dtd_phase = DtdPhase::NotationBeforeDef;
                        continue;
                    }
                }
                DtdPhase::NotationBeforeDef => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'S' {
                        let ctx = DtdDeclContext::Notation;
                        self.dtd_phase = DtdPhase::ExternalIdSystemKw { ctx, matched: 1 };
                    } else if byte == b'P' {
                        let ctx = DtdDeclContext::Notation;
                        self.dtd_phase = DtdPhase::ExternalIdPublicKw { ctx, matched: 1 };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedKeyword(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::NotationBeforeClose => {
                    match byte {
                        b'>' => {
                            let span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.notation_decl_end(span).map_err(ParseError::Visitor)?;
                            self.markup_start = None;
                            self.dtd_phase = DtdPhase::Idle;
                        }
                        b if is_xml_whitespace(b) => { /* skip */ }
                        _ => {
                            return Err(ParseError::Xml(Error {
                                kind: ErrorKind::ExpectedClose(byte),
                                offset: stream_offset + abs as u64,
                            }));
                        }
                    }
                }

                // --- ATTLIST declaration ---
                DtdPhase::AttlistRequireWs => {
                    if !is_xml_whitespace(byte) {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingWhitespace,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                    self.dtd_phase = DtdPhase::AttlistBeforeName;
                }
                DtdPhase::AttlistBeforeName => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if is_name_start_byte(byte) {
                        self.dtd_phase = DtdPhase::AttlistName { name_start: abs };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::DtdDeclMissingName,
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::AttlistName { name_start } => {
                    if !is_name_byte(byte) {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + name_start as u64,
                            stream_offset + abs as u64,
                        );
                        visitor.attlist_decl_start(name, span).map_err(ParseError::Visitor)?;
                        self.dtd_phase = DtdPhase::AttlistIdle;
                        continue;
                    }
                }
                DtdPhase::AttlistIdle => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'>' {
                        let span = Span::new(
                            stream_offset + abs as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.attlist_decl_end(span).map_err(ParseError::Visitor)?;
                        self.markup_start = None;
                        self.dtd_phase = DtdPhase::Idle;
                    } else if is_name_start_byte(byte) {
                        self.dtd_phase = DtdPhase::AttlistAttrName { name_start: abs };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedName(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::AttlistAttrName { name_start } => {
                    if !is_name_byte(byte) {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + name_start as u64,
                            stream_offset + abs as u64,
                        );
                        visitor.attlist_attr_name(name, span).map_err(ParseError::Visitor)?;
                        self.dtd_phase = DtdPhase::AttlistBeforeType;
                        continue;
                    }
                }
                DtdPhase::AttlistBeforeType => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else {
                        self.dtd_phase = DtdPhase::AttlistTypeStart;
                        continue;
                    }
                }
                DtdPhase::AttlistTypeStart => {
                    if byte == b'(' {
                        // Enumeration type
                        self.content_start = Some(abs);
                        self.dtd_phase = DtdPhase::AttlistTypeEnum { paren_depth: 1 };
                    } else if byte == b'N' {
                        // Could be NOTATION, NMTOKEN, NMTOKENS
                        self.content_start = Some(abs);
                        self.dtd_phase = DtdPhase::AttlistTypeKeyword { start: abs };
                    } else if byte.is_ascii_alphabetic() {
                        // Other keyword: CDATA, ID, IDREF, IDREFS, ENTITY, ENTITIES
                        self.content_start = Some(abs);
                        self.dtd_phase = DtdPhase::AttlistTypeKeyword { start: abs };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedKeyword(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::AttlistTypeKeyword { start } => {
                    if is_xml_whitespace(byte) || byte == b'(' || byte == b'#' || byte == b'"' || byte == b'\'' {
                        // End of type keyword
                        let content = &buf[start..abs];
                        let span = Span::new(
                            stream_offset + start as u64,
                            stream_offset + abs as u64,
                        );
                        visitor.attlist_attr_type(content, span).map_err(ParseError::Visitor)?;
                        self.content_start = None;

                        // Check if this was NOTATION — if so, expect `(` for enumeration
                        if content == b"NOTATION" {
                            self.dtd_phase = DtdPhase::AttlistTypeNotationBeforeParen;
                            continue;
                        }

                        self.dtd_phase = DtdPhase::AttlistBeforeDefault;
                        continue;
                    }
                    // Keep scanning keyword
                }
                DtdPhase::AttlistTypeNotationBeforeParen => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'(' {
                        self.content_start = Some(abs);
                        self.dtd_phase = DtdPhase::AttlistTypeEnum { paren_depth: 1 };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedKeyword(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::AttlistTypeEnum { ref mut paren_depth } => {
                    match byte {
                        b'(' => {
                            *paren_depth += 1;
                            if *paren_depth > Self::MAX_PAREN_DEPTH {
                                return Err(ParseError::Xml(Error {
                                    kind: ErrorKind::DtdParensTooDeep,
                                    offset: stream_offset + abs as u64,
                                }));
                            }
                        }
                        b')' => {
                            *paren_depth -= 1;
                            if *paren_depth == 0 {
                                let content_start = self.content_start.unwrap();
                                let span = Span::new(
                                    stream_offset + content_start as u64,
                                    stream_offset + abs as u64 + 1,
                                );
                                visitor.attlist_attr_type(&buf[content_start..abs + 1], span)
                                    .map_err(ParseError::Visitor)?;
                                self.content_start = None;
                                self.dtd_phase = DtdPhase::AttlistBeforeDefault;
                            }
                        }
                        _ => { /* keep scanning */ }
                    }
                }
                DtdPhase::AttlistBeforeDefault => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'#' {
                        self.dtd_phase = DtdPhase::AttlistDefaultHash { start: abs };
                    } else if byte == b'"' || byte == b'\'' {
                        // Plain default value (no keyword)
                        let quote = if byte == b'"' { QuoteStyle::Double } else { QuoteStyle::Single };
                        let span = Span::new(
                            stream_offset + abs as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.attlist_attr_default_start(false, span).map_err(ParseError::Visitor)?;
                        self.content_start = Some(abs + 1);
                        self.dtd_phase = DtdPhase::AttlistDefaultValue { quote };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedQuote(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::AttlistDefaultHash { start } => {
                    // Matching #REQUIRED, #IMPLIED, or #FIXED
                    if is_xml_whitespace(byte) || byte == b'>' || byte == b'"' || byte == b'\'' {
                        let keyword = &buf[start..abs];
                        let span = Span::new(
                            stream_offset + start as u64,
                            stream_offset + abs as u64,
                        );
                        match keyword {
                            b"#REQUIRED" => {
                                visitor.attlist_attr_required(span).map_err(ParseError::Visitor)?;
                                self.dtd_phase = DtdPhase::AttlistIdle;
                                continue;
                            }
                            b"#IMPLIED" => {
                                visitor.attlist_attr_implied(span).map_err(ParseError::Visitor)?;
                                self.dtd_phase = DtdPhase::AttlistIdle;
                                continue;
                            }
                            b"#FIXED" => {
                                self.dtd_phase = DtdPhase::AttlistFixedBeforeValue;
                                continue;
                            }
                            _ => {
                                return Err(ParseError::Xml(Error {
                                    kind: ErrorKind::ExpectedKeyword(byte),
                                    offset: stream_offset + start as u64,
                                }));
                            }
                        }
                    }
                    // Keep scanning keyword characters
                }
                DtdPhase::AttlistFixedBeforeValue => {
                    if is_xml_whitespace(byte) { /* skip */ }
                    else if byte == b'"' || byte == b'\'' {
                        let quote = if byte == b'"' { QuoteStyle::Double } else { QuoteStyle::Single };
                        let span = Span::new(
                            stream_offset + abs as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.attlist_attr_default_start(true, span).map_err(ParseError::Visitor)?;
                        self.content_start = Some(abs + 1);
                        self.dtd_phase = DtdPhase::AttlistDefaultValue { quote };
                    } else {
                        return Err(ParseError::Xml(Error {
                            kind: ErrorKind::ExpectedQuote(byte),
                            offset: stream_offset + abs as u64,
                        }));
                    }
                }
                DtdPhase::AttlistDefaultValue { quote } => {
                    let delim = if quote == QuoteStyle::Double { b'"' } else { b'\'' };
                    match byte {
                        b if b == delim => {
                            let content_start = self.content_start.unwrap();
                            if abs > content_start {
                                let span = Span::new(
                                    stream_offset + content_start as u64,
                                    stream_offset + abs as u64,
                                );
                                visitor.attlist_attr_default_value(&buf[content_start..abs], span)
                                    .map_err(ParseError::Visitor)?;
                            }
                            let end_span = Span::new(
                                stream_offset + abs as u64,
                                stream_offset + abs as u64 + 1,
                            );
                            visitor.attlist_attr_default_end(end_span).map_err(ParseError::Visitor)?;
                            self.content_start = None;
                            self.dtd_phase = DtdPhase::AttlistIdle;
                        }
                        b'&' => {
                            let content_start = self.content_start.unwrap();
                            if abs > content_start {
                                let span = Span::new(
                                    stream_offset + content_start as u64,
                                    stream_offset + abs as u64,
                                );
                                visitor.attlist_attr_default_value(&buf[content_start..abs], span)
                                    .map_err(ParseError::Visitor)?;
                            }
                            if abs + 1 < buf.len() && buf[abs + 1] == b'#' {
                                self.dtd_phase = DtdPhase::AttlistDefaultCharRef {
                                    value_start: abs + 2,
                                    quote,
                                };
                                pos += 1;
                            } else {
                                self.dtd_phase = DtdPhase::AttlistDefaultEntityRef {
                                    name_start: abs + 1,
                                    quote,
                                };
                            }
                            self.content_start = None;
                        }
                        _ => { /* keep scanning */ }
                    }
                }
                DtdPhase::AttlistDefaultEntityRef { name_start, quote } => {
                    if byte == b';' {
                        let name = validate_name(buf, name_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + (name_start - 1) as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.attlist_attr_default_entity_ref(name, span)
                            .map_err(ParseError::Visitor)?;
                        self.content_start = Some(abs + 1);
                        self.dtd_phase = DtdPhase::AttlistDefaultValue { quote };
                    } else if abs == name_start && byte == b'#' {
                        // `&` was at buffer boundary, `#` arrived now → char ref
                        self.dtd_phase = DtdPhase::AttlistDefaultCharRef {
                            value_start: abs + 1,
                            quote,
                        };
                    } else {
                        check_ref_name_byte(byte, abs, name_start, stream_offset)?;
                    }
                }
                DtdPhase::AttlistDefaultCharRef { value_start, quote } => {
                    if byte == b';' {
                        let value = validate_char_ref(buf, value_start, abs, stream_offset)?;
                        let span = Span::new(
                            stream_offset + (value_start - 2) as u64,
                            stream_offset + abs as u64 + 1,
                        );
                        visitor.attlist_attr_default_char_ref(value, span)
                            .map_err(ParseError::Visitor)?;
                        self.content_start = Some(abs + 1);
                        self.dtd_phase = DtdPhase::AttlistDefaultValue { quote };
                    }
                }
            }

            pos += 1;
        }

        Ok(block_len)
    }

    /// Scan after `]` closing the internal subset, expecting optional whitespace then `>`.
    #[cfg(feature = "dtd")]
    fn scan_doctype_after_subset<V: Visitor>(
        &mut self,
        buf: &[u8],
        block_offset: usize,
        block_len: usize,
        mut pos: usize,
        stream_offset: u64,
        visitor: &mut V,
    ) -> Result<usize, ParseError<V::Error>> {
        while pos < block_len {
            let abs = block_offset + pos;
            let byte = buf[abs];
            match byte {
                b'>' => {
                    let end_span = Span::new(
                        stream_offset + abs as u64,
                        stream_offset + abs as u64 + 1,
                    );
                    visitor.doctype_end(end_span).map_err(ParseError::Visitor)?;
                    self.finish_content_body();
                    return Ok(pos + 1);
                }
                b if is_xml_whitespace(b) => { /* skip */ }
                _ => {
                    return Err(ParseError::Xml(Error {
                        kind: ErrorKind::ExpectedClose(byte),
                        offset: stream_offset + abs as u64,
                    }));
                }
            }
            pos += 1;
        }
        Ok(block_len)
    }

    /// Flush DTD content at buffer boundaries based on current dtd_phase.
    #[cfg(feature = "dtd")]
    fn flush_dtd_content<V: Visitor>(
        &mut self,
        content: &[u8],
        span: Span,
        visitor: &mut V,
    ) -> Result<(), ParseError<V::Error>> {
        match self.dtd_phase {
            DtdPhase::Comment { .. } => {
                visitor.comment_content(content, span).map_err(ParseError::Visitor)?;
            }
            DtdPhase::PIContent { .. } => {
                visitor.pi_content(content, span).map_err(ParseError::Visitor)?;
            }
            DtdPhase::ElementContentModel { .. } => {
                visitor.element_decl_content_spec(content, span).map_err(ParseError::Visitor)?;
            }
            DtdPhase::EntityValue { .. } => {
                visitor.entity_decl_value(content, span).map_err(ParseError::Visitor)?;
            }
            DtdPhase::AttlistTypeEnum { .. } | DtdPhase::AttlistTypeKeyword { .. } => {
                visitor.attlist_attr_type(content, span).map_err(ParseError::Visitor)?;
            }
            DtdPhase::AttlistDefaultValue { .. } => {
                visitor.attlist_attr_default_value(content, span).map_err(ParseError::Visitor)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Find where a name ends in the current block.
///
/// Returns `Some((block_relative_offset, absolute_position))` if a name-ending
/// character is found in the block, or `None` if the name continues past the block.
#[inline(always)]
fn find_name_end(
    name_end_mask: u64,
    pos: usize,
    block_offset: usize,
    block_len: usize,
) -> Option<(usize, usize)> {
    let shifted = name_end_mask >> pos;
    if shifted == 0 {
        return None;
    }
    let next = shifted.trailing_zeros() as usize;
    if pos + next >= block_len {
        return None;
    }
    Some((pos + next, block_offset + pos + next))
}

/// Check that a name being scanned hasn't exceeded the length limit.
///
/// Called when a name continues past the current block boundary.
#[inline]
fn check_name_length<E>(
    block_end: usize,
    name_start: usize,
    stream_offset: u64,
) -> Result<(), ParseError<E>> {
    if block_end - name_start > MAX_NAME_LENGTH {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::NameTooLong,
            offset: stream_offset + name_start as u64,
        }));
    }
    Ok(())
}

/// Validate a completed name: check it is non-empty and within the length limit.
///
/// Returns the name slice on success.
#[inline]
fn validate_name<'a, E>(
    buf: &'a [u8],
    name_start: usize,
    name_end: usize,
    stream_offset: u64,
) -> Result<&'a [u8], ParseError<E>> {
    if name_start == name_end {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::ExpectedName(buf[name_end]),
            offset: stream_offset + name_end as u64,
        }));
    }
    if name_end - name_start > MAX_NAME_LENGTH {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::NameTooLong,
            offset: stream_offset + name_start as u64,
        }));
    }
    Ok(&buf[name_start..name_end])
}

/// Scan for a semicolon-terminated entity name in the current block and validate it.
///
/// Returns `Ok(Some((name, span, next_pos)))` if a valid entity name was found,
/// `Ok(None)` if the semicolon is not in this block (caller should advance to block_len),
/// or `Err` if the name is invalid or too long.
#[inline]
fn find_and_validate_entity_name<'a, E>(
    buf: &'a [u8],
    block_offset: usize,
    block_len: usize,
    pos: usize,
    semicolon_mask: u64,
    stream_offset: u64,
    name_start: usize,
) -> Result<Option<(&'a [u8], Span, usize)>, ParseError<E>> {
    let shifted = semicolon_mask >> pos;
    if shifted == 0 {
        if block_offset + block_len - name_start > MAX_NAME_LENGTH {
            return Err(ParseError::Xml(Error {
                kind: ErrorKind::NameTooLong,
                offset: stream_offset + name_start as u64,
            }));
        }
        return Ok(None);
    }

    let next = shifted.trailing_zeros() as usize;
    if pos + next >= block_len {
        if block_offset + block_len - name_start > MAX_NAME_LENGTH {
            return Err(ParseError::Xml(Error {
                kind: ErrorKind::NameTooLong,
                offset: stream_offset + name_start as u64,
            }));
        }
        return Ok(None);
    }

    let semi_abs = block_offset + pos + next;
    let name = validate_name(buf, name_start, semi_abs, stream_offset)?;
    if !is_name_start_byte(name[0]) {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::ExpectedName(name[0]),
            offset: stream_offset + name_start as u64,
        }));
    }
    for (i, &b) in name[1..].iter().enumerate() {
        if !is_name_byte(b) {
            return Err(ParseError::Xml(Error {
                kind: ErrorKind::ExpectedName(b),
                offset: stream_offset + name_start as u64 + 1 + i as u64,
            }));
        }
    }

    let span = Span::new(
        stream_offset + name_start as u64,
        stream_offset + semi_abs as u64,
    );
    Ok(Some((name, span, pos + next + 1)))
}

/// Scan for a semicolon-terminated char ref value in the current block and validate it.
///
/// Returns `Ok(Some((value, span, next_pos)))` if a valid char ref was found,
/// `Ok(None)` if the semicolon is not in this block (caller should advance to block_len),
/// or `Err` if the value is invalid or too long.
#[inline]
fn find_and_validate_char_ref<'a, E>(
    buf: &'a [u8],
    block_offset: usize,
    block_len: usize,
    pos: usize,
    semicolon_mask: u64,
    stream_offset: u64,
    value_start: usize,
) -> Result<Option<(&'a [u8], Span, usize)>, ParseError<E>> {
    let shifted = semicolon_mask >> pos;
    if shifted == 0 {
        if block_offset + block_len - value_start > MAX_CHAR_REF_LENGTH {
            return Err(ParseError::Xml(Error {
                kind: ErrorKind::CharRefTooLong,
                offset: stream_offset + value_start as u64,
            }));
        }
        return Ok(None);
    }

    let next = shifted.trailing_zeros() as usize;
    if pos + next >= block_len {
        if block_offset + block_len - value_start > MAX_CHAR_REF_LENGTH {
            return Err(ParseError::Xml(Error {
                kind: ErrorKind::CharRefTooLong,
                offset: stream_offset + value_start as u64,
            }));
        }
        return Ok(None);
    }

    let semi_abs = block_offset + pos + next;
    let value = validate_char_ref(buf, value_start, semi_abs, stream_offset)?;
    if value[0] == b'x' {
        let hex_digits = &value[1..];
        if hex_digits.is_empty() || !hex_digits.iter().all(|b| b.is_ascii_hexdigit()) {
            return Err(ParseError::Xml(Error {
                kind: ErrorKind::InvalidCharRef,
                offset: stream_offset + value_start as u64,
            }));
        }
    } else if !value.iter().all(|b| b.is_ascii_digit()) {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::InvalidCharRef,
            offset: stream_offset + value_start as u64,
        }));
    }

    let span = Span::new(
        stream_offset + value_start as u64,
        stream_offset + semi_abs as u64,
    );
    Ok(Some((value, span, pos + next + 1)))
}

/// Validate a single byte during reference name scanning.
///
/// Checks `is_name_start_byte` for the first byte and `is_name_byte` for subsequent bytes.
#[cfg(feature = "dtd")]
#[inline]
fn check_ref_name_byte<E>(
    byte: u8,
    abs: usize,
    name_start: usize,
    stream_offset: u64,
) -> Result<(), ParseError<E>> {
    if abs == name_start && !is_name_start_byte(byte) {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::ExpectedName(byte),
            offset: stream_offset + abs as u64,
        }));
    }
    if abs > name_start && !is_name_byte(byte) {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::ExpectedName(byte),
            offset: stream_offset + abs as u64,
        }));
    }
    Ok(())
}

/// Validate a completed char ref value: check it is non-empty and within the length limit.
///
/// Returns the value slice on success.
#[inline]
fn validate_char_ref<'a, E>(
    buf: &'a [u8],
    value_start: usize,
    value_end: usize,
    stream_offset: u64,
) -> Result<&'a [u8], ParseError<E>> {
    let value = &buf[value_start..value_end];
    if value.is_empty() {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::InvalidCharRef,
            offset: stream_offset + value_end as u64,
        }));
    }
    if value.len() > MAX_CHAR_REF_LENGTH {
        return Err(ParseError::Xml(Error {
            kind: ErrorKind::CharRefTooLong,
            offset: stream_offset + value_start as u64,
        }));
    }
    Ok(value)
}

/// Find the first non-whitespace character in the current block.
///
/// Returns `Some((block_relative_position, absolute_position))` if found,
/// or `None` if the rest of the block is all whitespace.
#[inline(always)]
fn find_non_whitespace(
    whitespace_mask: u64,
    pos: usize,
    block_offset: usize,
    block_len: usize,
) -> Option<(usize, usize)> {
    find_name_end(!whitespace_mask, pos, block_offset, block_len)
}

/// Build a `QName` from a raw name slice and span by locating the first `:`.
#[inline]
fn make_qname(raw: &[u8], span: Span) -> QName<'_> {
    let colon_pos = memchr::memchr(b':', raw).map(|p| p as u16);
    QName::new(raw, colon_pos, span)
}

/// Check if a byte can start an XML name.
#[inline]
fn is_name_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b':' || b >= 0x80
}

/// Check if a byte can continue an XML name (NameChar).
#[inline]
fn is_name_byte(b: u8) -> bool {
    is_name_start_byte(b) || b.is_ascii_digit() || b == b'-' || b == b'.'
}

/// Examine the trailing bytes of a buffer to determine how many bytes
/// to rewind to avoid splitting a multi-byte UTF-8 character.
///
/// Parse the content of an XML declaration (`version`, `encoding`, `standalone`).
///
/// `buf` contains the pseudo-attribute text (between `xml` and `?>`).
/// `offset` is the absolute stream offset for error reporting (the `<?` position).
///
/// Returns `(version, encoding, standalone)`.
fn parse_xml_decl<E>(
    buf: &[u8],
    offset: u64,
) -> Result<(&[u8], Option<&[u8]>, Option<bool>), ParseError<E>> {
    let err = || {
        ParseError::Xml(Error {
            kind: ErrorKind::MalformedXmlDeclaration,
            offset,
        })
    };

    let mut pos = 0;

    // Skip leading whitespace
    while pos < buf.len() && is_xml_whitespace(buf[pos]) {
        pos += 1;
    }

    // --- version (required) ---
    let version = parse_pseudo_attr(buf, &mut pos, b"version").ok_or_else(err)?;

    // Skip whitespace
    while pos < buf.len() && is_xml_whitespace(buf[pos]) {
        pos += 1;
    }

    // Check if done
    if pos >= buf.len() {
        return Ok((version, None, None));
    }

    // --- encoding (optional) ---
    let mut encoding = None;
    let mut standalone = None;

    if buf[pos..].starts_with(b"encoding") {
        encoding = Some(parse_pseudo_attr(buf, &mut pos, b"encoding").ok_or_else(err)?);

        // Skip whitespace
        while pos < buf.len() && is_xml_whitespace(buf[pos]) {
            pos += 1;
        }

        if pos >= buf.len() {
            return Ok((version, encoding, None));
        }
    }

    // --- standalone (optional) ---
    if buf[pos..].starts_with(b"standalone") {
        let val = parse_pseudo_attr(buf, &mut pos, b"standalone").ok_or_else(err)?;
        standalone = Some(match val {
            b"yes" => true,
            b"no" => false,
            _ => return Err(err()),
        });

        // Skip trailing whitespace
        while pos < buf.len() && is_xml_whitespace(buf[pos]) {
            pos += 1;
        }
    }

    // There should be nothing left
    if pos < buf.len() {
        return Err(err());
    }

    Ok((version, encoding, standalone))
}

/// Parse a single pseudo-attribute: `name = "value"` or `name = 'value'`.
/// Advances `pos` past the closing quote. Returns the value slice.
fn parse_pseudo_attr<'a>(buf: &'a [u8], pos: &mut usize, expected_name: &[u8]) -> Option<&'a [u8]> {
    // Match name
    let end = *pos + expected_name.len();
    if end > buf.len() || &buf[*pos..end] != expected_name {
        return None;
    }
    *pos = end;

    // Skip whitespace around '='
    while *pos < buf.len() && is_xml_whitespace(buf[*pos]) {
        *pos += 1;
    }
    if *pos >= buf.len() || buf[*pos] != b'=' {
        return None;
    }
    *pos += 1;
    while *pos < buf.len() && is_xml_whitespace(buf[*pos]) {
        *pos += 1;
    }

    // Opening quote
    if *pos >= buf.len() {
        return None;
    }
    let quote = buf[*pos];
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    *pos += 1;

    // Value up to closing quote
    let value_start = *pos;
    while *pos < buf.len() && buf[*pos] != quote {
        *pos += 1;
    }
    if *pos >= buf.len() {
        return None;
    }
    let value = &buf[value_start..*pos];
    *pos += 1; // skip closing quote

    Some(value)
}

/// Returns `Ok(rewind)` where `rewind` is 0..=3 bytes to exclude.
/// Returns `Err(offset)` if the trailing bytes are invalid UTF-8
/// (e.g., an unexpected continuation byte with no valid leader).
fn utf8_boundary_rewind(buf: &[u8]) -> Result<usize, usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let start = buf.len().saturating_sub(3);
    for i in (start..buf.len()).rev() {
        let b = buf[i];
        if b < 0x80 {
            return Ok(0); // ASCII - no split possible
        }
        if b >= 0xC0 {
            let expected_len = if b < 0xE0 {
                2
            } else if b < 0xF0 {
                3
            } else {
                4
            };
            let available = buf.len() - i;
            if available >= expected_len {
                return Ok(0); // Sequence complete - no rewind
            } else {
                return Ok(available); // Incomplete - rewind
            }
        }
    }
    Err(buf.len().saturating_sub(3)) // No leader found - invalid
}

/// Default buffer size for [`parse_read`].
#[cfg(feature = "std")]
const DEFAULT_BUF_SIZE: usize = 8192;

/// Parse XML from a [`std::io::Read`] source.
///
/// This drives the full read-parse-shift loop internally, freeing the caller
/// from managing a buffer, tracking `stream_offset`, or shifting unconsumed
/// bytes.
///
/// An internal buffer of 8 KiB is used. For control over the buffer size, use
/// [`parse_read_with_capacity`].
///
/// # Errors
///
/// Returns [`ReadError::Xml`] for XML syntax errors, [`ReadError::Visitor`]
/// for errors returned by visitor callbacks, or [`ReadError::Io`] for I/O
/// failures from the reader.
#[cfg(feature = "std")]
pub fn parse_read<R: std::io::Read, V: Visitor>(
    reader: R,
    visitor: &mut V,
) -> Result<(), ReadError<V::Error>> {
    parse_read_with_capacity(reader, visitor, DEFAULT_BUF_SIZE)
}

/// Like [`parse_read`], but with a caller-specified buffer capacity.
///
/// `capacity` is clamped to a minimum of 64 bytes (one SIMD block).
#[cfg(feature = "std")]
pub fn parse_read_with_capacity<R: std::io::Read, V: Visitor>(
    mut reader: R,
    visitor: &mut V,
    capacity: usize,
) -> Result<(), ReadError<V::Error>> {
    let capacity = capacity.max(64);
    let mut buf = std::vec![0u8; capacity];
    let mut parser = Reader::new();
    let mut stream_offset: u64 = 0;
    let mut valid: usize = 0;

    loop {
        // Fill remaining buffer space from the reader
        let n = reader.read(&mut buf[valid..]).map_err(ReadError::Io)?;
        valid += n;
        let is_final = n == 0;

        if valid == 0 {
            break;
        }

        let consumed = parser
            .parse(&buf[..valid], stream_offset, is_final, visitor)
            .map_err(ReadError::from_parse)? as usize;

        // Shift unconsumed bytes to the front
        let leftover = valid - consumed;
        if leftover > 0 {
            buf.copy_within(consumed..valid, 0);
        }
        valid = leftover;
        stream_offset += consumed as u64;

        if consumed == 0 && is_final {
            break;
        }
    }

    Ok(())
}

/// Error type for [`parse_read`] and [`parse_read_with_capacity`].
#[cfg(feature = "std")]
pub enum ReadError<E> {
    /// XML syntax error.
    Xml(Error),
    /// Error returned by a [`Visitor`] callback.
    Visitor(E),
    /// I/O error from the underlying reader.
    Io(std::io::Error),
}

#[cfg(feature = "std")]
impl<E> ReadError<E> {
    fn from_parse(e: ParseError<E>) -> Self {
        match e {
            ParseError::Xml(e) => ReadError::Xml(e),
            ParseError::Visitor(e) => ReadError::Visitor(e),
        }
    }
}

#[cfg(feature = "std")]
impl<E: core::fmt::Debug> core::fmt::Debug for ReadError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReadError::Xml(e) => write!(f, "ReadError::Xml({e:?})"),
            ReadError::Visitor(e) => write!(f, "ReadError::Visitor({e:?})"),
            ReadError::Io(e) => write!(f, "ReadError::Io({e:?})"),
        }
    }
}

#[cfg(feature = "std")]
impl<E: core::fmt::Display> core::fmt::Display for ReadError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReadError::Xml(e) => write!(f, "XML error: {e}"),
            ReadError::Visitor(e) => write!(f, "visitor error: {e}"),
            ReadError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

#[cfg(feature = "std")]
impl<E: core::error::Error> core::error::Error for ReadError<E> {}

#[cfg(feature = "std")]
impl<E> From<Error> for ReadError<E> {
    fn from(e: Error) -> Self {
        ReadError::Xml(e)
    }
}

#[cfg(feature = "std")]
impl<E> From<std::io::Error> for ReadError<E> {
    fn from(e: std::io::Error) -> Self {
        ReadError::Io(e)
    }
}
