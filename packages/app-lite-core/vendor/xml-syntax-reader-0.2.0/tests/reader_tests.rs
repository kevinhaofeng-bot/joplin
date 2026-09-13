use xml_syntax_reader::{ErrorKind, ParseError, QName, Span, Visitor};
use xml_syntax_reader::Reader;

/// A recording visitor that logs all events as strings for test assertions.
#[derive(Debug, Default)]
struct Recorder {
    events: Vec<String>,
}

impl Visitor for Recorder {
    type Error = std::convert::Infallible;

    fn start_tag_open(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        let span = name.span();
        self.events.push(format!(
            "StartTagOpen({}, {}..{})",
            String::from_utf8_lossy(&name),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn attribute_name(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        let span = name.span();
        self.events.push(format!(
            "AttrName({}, {}..{})",
            String::from_utf8_lossy(&name),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn attribute_value(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttrValue({}, {}..{})",
            String::from_utf8_lossy(value),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn attribute_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("AttrEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn attribute_entity_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttrEntityRef({}, {}..{})",
            String::from_utf8_lossy(name),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn attribute_char_ref(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttrCharRef({}, {}..{})",
            String::from_utf8_lossy(value),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn start_tag_close(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("StartTagClose({}..{})", span.start, span.end));
        Ok(())
    }

    fn empty_element_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("EmptyElementEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn end_tag(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        let span = name.span();
        self.events.push(format!(
            "EndTag({}, {}..{})",
            String::from_utf8_lossy(&name),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn characters(&mut self, text: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "Characters({}, {}..{})",
            String::from_utf8_lossy(text),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn entity_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityRef({}, {}..{})",
            String::from_utf8_lossy(name),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn char_ref(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "CharRef({}, {}..{})",
            String::from_utf8_lossy(value),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn cdata_start(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("CdataStart({}..{})", span.start, span.end));
        Ok(())
    }

    fn cdata_content(&mut self, text: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "CdataContent({}, {}..{})",
            String::from_utf8_lossy(text),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn cdata_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("CdataEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn comment_start(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("CommentStart({}..{})", span.start, span.end));
        Ok(())
    }

    fn comment_content(&mut self, text: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "CommentContent({}, {}..{})",
            String::from_utf8_lossy(text),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn comment_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("CommentEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn xml_declaration(
        &mut self,
        version: &[u8],
        encoding: Option<&[u8]>,
        standalone: Option<bool>,
        span: Span,
    ) -> Result<(), Self::Error> {
        let enc = match encoding {
            Some(e) => format!("Some({})", String::from_utf8_lossy(e)),
            None => "None".to_string(),
        };
        let sa = match standalone {
            Some(v) => format!("Some({v})"),
            None => "None".to_string(),
        };
        self.events.push(format!(
            "XmlDeclaration({}, {}, {}, {}..{})",
            String::from_utf8_lossy(version),
            enc,
            sa,
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn pi_start(&mut self, target: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "PIStart({}, {}..{})",
            String::from_utf8_lossy(target),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn pi_content(&mut self, data: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "PIContent({}, {}..{})",
            String::from_utf8_lossy(data),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn pi_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("PIEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn doctype_start(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "DoctypeStart({}, {}..{})",
            String::from_utf8_lossy(name),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn doctype_content(&mut self, content: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "DoctypeContent({}, {}..{})",
            String::from_utf8_lossy(content),
            span.start,
            span.end,
        ));
        Ok(())
    }

    fn doctype_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("DoctypeEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn doctype_internal_subset_start(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("SubsetStart({}..{})", span.start, span.end));
        Ok(())
    }

    fn doctype_internal_subset_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events
            .push(format!("SubsetEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn element_decl_start(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "ElementDeclStart({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }

    fn element_decl_empty(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("ElementDeclEmpty({}..{})", span.start, span.end));
        Ok(())
    }

    fn element_decl_any(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("ElementDeclAny({}..{})", span.start, span.end));
        Ok(())
    }

    fn element_decl_content_spec(&mut self, content: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "ElementDeclContentSpec({}, {}..{})",
            String::from_utf8_lossy(content), span.start, span.end,
        ));
        Ok(())
    }

    fn element_decl_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("ElementDeclEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn attlist_decl_start(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttlistDeclStart({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }

    fn attlist_attr_name(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttlistAttrName({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }

    fn attlist_attr_type(&mut self, content: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttlistAttrType({}, {}..{})",
            String::from_utf8_lossy(content), span.start, span.end,
        ));
        Ok(())
    }

    fn attlist_attr_required(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("AttlistAttrRequired({}..{})", span.start, span.end));
        Ok(())
    }

    fn attlist_attr_implied(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("AttlistAttrImplied({}..{})", span.start, span.end));
        Ok(())
    }

    fn attlist_attr_default_start(&mut self, fixed: bool, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("AttlistAttrDefaultStart(fixed={}, {}..{})", fixed, span.start, span.end));
        Ok(())
    }

    fn attlist_attr_default_value(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttlistAttrDefaultValue({}, {}..{})",
            String::from_utf8_lossy(value), span.start, span.end,
        ));
        Ok(())
    }

    fn attlist_attr_default_entity_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttlistAttrDefaultEntityRef({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }

    fn attlist_attr_default_char_ref(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "AttlistAttrDefaultCharRef({}, {}..{})",
            String::from_utf8_lossy(value), span.start, span.end,
        ));
        Ok(())
    }

    fn attlist_attr_default_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("AttlistAttrDefaultEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn attlist_decl_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("AttlistDeclEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn entity_decl_start(&mut self, name: &[u8], kind: xml_syntax_reader::EntityKind, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityDeclStart({}, pe={}, {}..{})",
            String::from_utf8_lossy(name), kind == xml_syntax_reader::EntityKind::Parameter, span.start, span.end,
        ));
        Ok(())
    }

    fn entity_decl_value(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityDeclValue({}, {}..{})",
            String::from_utf8_lossy(value), span.start, span.end,
        ));
        Ok(())
    }

    fn entity_decl_entity_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityDeclEntityRef({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }

    fn entity_decl_char_ref(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityDeclCharRef({}, {}..{})",
            String::from_utf8_lossy(value), span.start, span.end,
        ));
        Ok(())
    }

    fn entity_decl_pe_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityDeclPeRef({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }

    fn entity_decl_value_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("EntityDeclValueEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn entity_decl_ndata(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityDeclNdata({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }

    fn entity_decl_system_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityDeclSystemId({}, {}..{})",
            String::from_utf8_lossy(literal), span.start, span.end,
        ));
        Ok(())
    }

    fn entity_decl_public_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "EntityDeclPublicId({}, {}..{})",
            String::from_utf8_lossy(literal), span.start, span.end,
        ));
        Ok(())
    }

    fn entity_decl_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("EntityDeclEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn notation_decl_start(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "NotationDeclStart({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }

    fn notation_decl_system_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "NotationDeclSystemId({}, {}..{})",
            String::from_utf8_lossy(literal), span.start, span.end,
        ));
        Ok(())
    }

    fn notation_decl_public_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "NotationDeclPublicId({}, {}..{})",
            String::from_utf8_lossy(literal), span.start, span.end,
        ));
        Ok(())
    }

    fn notation_decl_end(&mut self, span: Span) -> Result<(), Self::Error> {
        self.events.push(format!("NotationDeclEnd({}..{})", span.start, span.end));
        Ok(())
    }

    fn dtd_pe_reference(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        self.events.push(format!(
            "DtdPeRef({}, {}..{})",
            String::from_utf8_lossy(name), span.start, span.end,
        ));
        Ok(())
    }
}

/// Parse a complete document in one buffer and return recorded events.
fn parse_full(input: &[u8]) -> Vec<String> {
    let mut reader = Reader::new();
    let mut rec = Recorder::default();
    reader
        .parse(input, 0, true, &mut rec)
        .expect("parse failed");
    rec.events
}

/// Parse a document in chunks of a given size and return recorded events.
///
/// Each iteration adds up to `chunk_size` new bytes from input to the buffer,
/// then calls parse(). Unconsumed bytes are shifted to the front.
fn parse_chunked(input: &[u8], chunk_size: usize) -> Vec<String> {
    let mut reader = Reader::new();
    let mut rec = Recorder::default();
    let mut stream_offset: u64 = 0;
    // Buffer large enough to hold the entire input + headroom
    let mut buf = vec![0u8; input.len() + 64];
    let mut valid = 0usize;
    let mut input_pos = 0usize;

    loop {
        // Add up to chunk_size new bytes from input
        let to_copy = (input.len() - input_pos).min(chunk_size);
        if to_copy > 0 {
            buf[valid..valid + to_copy].copy_from_slice(&input[input_pos..input_pos + to_copy]);
            valid += to_copy;
            input_pos += to_copy;
        }

        let is_final = input_pos >= input.len();

        if valid == 0 {
            break;
        }

        let consumed = match reader.parse(&buf[..valid], stream_offset, is_final, &mut rec) {
            Ok(c) => c as usize,
            Err(ParseError::Xml(e)) => panic!("XML error at offset {}: {}", e.offset, e.kind),
            Err(ParseError::Visitor(_)) => unreachable!(),
        };

        let leftover = valid - consumed;
        if leftover > 0 {
            buf.copy_within(consumed..valid, 0);
        }
        valid = leftover;
        stream_offset += consumed as u64;

        // Safety: if we can't consume anything and can't add more data, stop
        if consumed == 0 && to_copy == 0 {
            if is_final {
                break;
            }
            panic!(
                "Parser stuck: consumed 0 bytes with {} valid bytes and no new data",
                valid
            );
        }
    }

    rec.events
}

/// Parse a "Prefix(TEXT, START..END)" content event into (text, start, end).
fn parse_content_event<'a>(s: &'a str, prefix: &str) -> Option<(&'a str, u64, u64)> {
    let inner = s.strip_prefix(prefix)?.strip_suffix(')')?;
    let last_comma = inner.rfind(", ")?;
    let text = &inner[..last_comma];
    let span = &inner[last_comma + 2..];
    let dots = span.find("..")?;
    let start: u64 = span[..dots].parse().ok()?;
    let end: u64 = span[dots + 2..].parse().ok()?;
    Some((text, start, end))
}

/// Coalesce adjacent content events of the same type with contiguous spans.
///
/// In streaming mode, content bodies (Characters, CommentContent, CdataContent,
/// PIContent, DoctypeContent) may be split across multiple events depending on
/// buffer boundaries. This function merges them so we can compare chunked output
/// against single-buffer output.
fn coalesce_content_events(events: &[String]) -> Vec<String> {
    const PREFIXES: &[&str] = &[
        "Characters(",
        "CommentContent(",
        "CdataContent(",
        "PIContent(",
        "DoctypeContent(",
        "AttrValue(",
        #[cfg(feature = "dtd")]
        "ElementDeclContentSpec(",
        #[cfg(feature = "dtd")]
        "AttlistAttrType(",
        #[cfg(feature = "dtd")]
        "EntityDeclValue(",
    ];

    let mut result: Vec<String> = Vec::new();

    for event in events {
        let mut merged = false;
        if let Some(prev) = result.last() {
            for &prefix in PREFIXES {
                if event.starts_with(prefix) && prev.starts_with(prefix) {
                    if let (Some(prev_parts), Some(cur_parts)) =
                        (parse_content_event(prev, prefix), parse_content_event(event, prefix))
                    {
                        if prev_parts.2 == cur_parts.1 {
                            let tag = &prefix[..prefix.len() - 1]; // strip trailing '('
                            let m = format!(
                                "{}({}{}, {}..{})",
                                tag, prev_parts.0, cur_parts.0, prev_parts.1, cur_parts.2,
                            );
                            *result.last_mut().unwrap() = m;
                            merged = true;
                        }
                    }
                    break;
                }
            }
        }
        if !merged {
            result.push(event.clone());
        }
    }

    result
}

/// Parse a document split at every possible chunk size (1..=input.len())
/// and verify that the events match parsing the full document at once.
fn verify_all_splits(input: &[u8]) {
    let expected = parse_full(input);

    for chunk_size in 1..=input.len() {
        let actual = parse_chunked(input, chunk_size);
        let coalesced = coalesce_content_events(&actual);
        assert_eq!(
            coalesced, expected,
            "Event mismatch with chunk_size={chunk_size} for input {:?}\nRaw events: {:?}",
            String::from_utf8_lossy(input),
            actual,
        );
    }
}

// ========================================================================
// Basic element tests
// Span convention: spans cover exactly the bytes of the data parameter.
// E.g. in `<br/>`, name "br" spans bytes 1..3 (not including '<').
// ========================================================================

#[test]
fn empty_element() {
    // < b r / >
    // 0 1 2 3 4
    let events = parse_full(b"<br/>");
    assert_eq!(
        events,
        vec!["StartTagOpen(br, 1..3)", "EmptyElementEnd(3..5)",]
    );
}

#[test]
fn empty_element_with_space() {
    // < b r   / >
    // 0 1 2 3 4 5
    let events = parse_full(b"<br />");
    assert_eq!(
        events,
        vec!["StartTagOpen(br, 1..3)", "EmptyElementEnd(4..6)",]
    );
}

#[test]
fn simple_element_with_text() {
    // < p > h e l l o < / p  >
    // 0 1 2 3 4 5 6 7 8 9 10 11
    let events = parse_full(b"<p>hello</p>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(p, 1..2)",
            "StartTagClose(2..3)",
            "Characters(hello, 3..8)",
            "EndTag(p, 10..11)",
        ]
    );
}

#[test]
fn nested_elements() {
    // < a > < b > t e x t <  /  b  >  <  /  a  >
    // 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17
    let events = parse_full(b"<a><b>text</b></a>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(a, 1..2)",
            "StartTagClose(2..3)",
            "StartTagOpen(b, 4..5)",
            "StartTagClose(5..6)",
            "Characters(text, 6..10)",
            "EndTag(b, 12..13)",
            "EndTag(a, 16..17)",
        ]
    );
}

#[test]
fn element_with_attribute() {
    // < d i v   c l a s s =  "  m  a  i  n  "  /  >
    // 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18
    let events = parse_full(b"<div class=\"main\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(div, 1..4)",
            "AttrName(class, 5..10)",
            "AttrValue(main, 12..16)",
            "AttrEnd(16..17)",
            "EmptyElementEnd(17..19)",
        ]
    );
}

#[test]
fn element_with_single_quoted_attribute() {
    // < d i v   c l a s s =  '  m  a  i  n  '  /  >
    // 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18
    let events = parse_full(b"<div class='main'/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(div, 1..4)",
            "AttrName(class, 5..10)",
            "AttrValue(main, 12..16)",
            "AttrEnd(16..17)",
            "EmptyElementEnd(17..19)",
        ]
    );
}

#[test]
fn element_with_multiple_attributes() {
    // < i m g   s r c =  "  a  .  p  n  g  "     a  l  t  =  "  p  i  c  "  /  >
    // 0 1 2 3 4 5 6 7 8  9  10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27
    let events = parse_full(b"<img src=\"a.png\" alt=\"pic\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(img, 1..4)",
            "AttrName(src, 5..8)",
            "AttrValue(a.png, 10..15)",
            "AttrEnd(15..16)",
            "AttrName(alt, 17..20)",
            "AttrValue(pic, 22..25)",
            "AttrEnd(25..26)",
            "EmptyElementEnd(26..28)",
        ]
    );
}

#[test]
fn element_with_namespace_prefix() {
    // < n s : e l e m / >
    // 0 1 2 3 4 5 6 7 8 9
    let events = parse_full(b"<ns:elem/>");
    assert_eq!(
        events,
        vec!["StartTagOpen(ns:elem, 1..8)", "EmptyElementEnd(8..10)",]
    );
}

#[test]
fn end_tag_with_whitespace() {
    // < a > < / a     >
    // 0 1 2 3 4 5 6 7 8
    let events = parse_full(b"<a></a  >");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(a, 1..2)",
            "StartTagClose(2..3)",
            "EndTag(a, 5..6)",
        ]
    );
}

#[test]
fn text_between_elements() {
    // < a > o n e <  /  a  >  <  b  >  t  w  o  <  /  b  >
    // 0 1 2 3 4 5 6  7  8  9  10 11 12 13 14 15 16 17 18 19
    let events = parse_full(b"<a>one</a><b>two</b>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(a, 1..2)",
            "StartTagClose(2..3)",
            "Characters(one, 3..6)",
            "EndTag(a, 8..9)",
            "StartTagOpen(b, 11..12)",
            "StartTagClose(12..13)",
            "Characters(two, 13..16)",
            "EndTag(b, 18..19)",
        ]
    );
}

#[test]
fn attribute_with_spaces_around_eq() {
    // < x   a   =   " v "  /  >
    // 0 1 2 3 4 5 6 7 8 9 10 11
    let events = parse_full(b"<x a = \"v\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(x, 1..2)",
            "AttrName(a, 3..4)",
            "AttrValue(v, 8..9)",
            "AttrEnd(9..10)",
            "EmptyElementEnd(10..12)",
        ]
    );
}

#[test]
fn empty_attribute_value() {
    // < x   a = " " / >
    // 0 1 2 3 4 5 6 7 8
    let events = parse_full(b"<x a=\"\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(x, 1..2)",
            "AttrName(a, 3..4)",
            "AttrEnd(6..7)",
            "EmptyElementEnd(7..9)",
        ]
    );
}

// ========================================================================
// Buffer boundary split tests
// ========================================================================

#[test]
fn split_empty_element() {
    verify_all_splits(b"<br/>");
}

#[test]
fn split_simple_element() {
    verify_all_splits(b"<p>hello</p>");
}

#[test]
fn split_nested_elements() {
    verify_all_splits(b"<a><b>text</b></a>");
}

#[test]
fn split_element_with_attribute() {
    verify_all_splits(b"<div class=\"main\"/>");
}

#[test]
fn split_element_with_multiple_attributes() {
    verify_all_splits(b"<img src=\"a.png\" alt=\"pic\"/>");
}

#[test]
fn split_attribute_with_spaces() {
    verify_all_splits(b"<x a = \"v\"/>");
}

#[test]
fn split_complex_document() {
    verify_all_splits(b"<root><child attr=\"val\">text</child><empty/></root>");
}

#[test]
fn split_many_elements() {
    verify_all_splits(b"<a/><b/><c/><d/><e/>");
}

#[test]
fn split_long_text() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<p>");
    doc.extend_from_slice(&[b'x'; 200]);
    doc.extend_from_slice(b"</p>");
    verify_all_splits(&doc);
}

#[test]
fn split_long_attribute_value() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<p a=\"");
    doc.extend_from_slice(&[b'v'; 200]);
    doc.extend_from_slice(b"\"/>");

    let events = parse_full(&doc);
    assert_eq!(events.len(), 5); // StartTagOpen, AttrName, AttrValue, AttrEnd, EmptyElementEnd
    assert_eq!(events[0], "StartTagOpen(p, 1..2)");
    assert_eq!(events[1], "AttrName(a, 3..4)");
    assert!(events[2].starts_with("AttrValue("));
    assert!(events[3].starts_with("AttrEnd("));

    verify_all_splits(&doc);
}

// ========================================================================
// Edge cases
// ========================================================================

#[test]
fn empty_input() {
    let events = parse_full(b"");
    assert!(events.is_empty());
}

#[test]
fn text_only() {
    let events = parse_full(b"hello world");
    assert_eq!(events, vec!["Characters(hello world, 0..11)"]);
}

#[test]
fn whitespace_text() {
    let events = parse_full(b"  \n\t  ");
    assert_eq!(events, vec!["Characters(  \n\t  , 0..6)"]);
}

#[test]
fn element_then_text_then_element() {
    // < a / > t x t < b / >
    // 0 1 2 3 4 5 6 7 8 9 10
    let events = parse_full(b"<a/>txt<b/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(a, 1..2)",
            "EmptyElementEnd(2..4)",
            "Characters(txt, 4..7)",
            "StartTagOpen(b, 8..9)",
            "EmptyElementEnd(9..11)",
        ]
    );
}

#[test]
fn deeply_nested() {
    let input = b"<a><b><c><d>x</d></c></b></a>";
    let events = parse_full(input);
    // Just verify it doesn't crash and produces reasonable output
    assert!(events.len() > 8);
    assert!(events[0].starts_with("StartTagOpen(a,"));
    assert!(events.last().unwrap().starts_with("EndTag(a,"));
}

#[test]
fn split_deeply_nested() {
    verify_all_splits(b"<a><b><c><d>x</d></c></b></a>");
}

// ========================================================================
// Comment tests
// ========================================================================

#[test]
fn simple_comment() {
    // < ! - -   h i   - - >
    // 0 1 2 3 4 5 6 7 8 9 10
    let events = parse_full(b"<!-- hi -->");
    assert_eq!(
        events,
        vec![
            "CommentStart(0..4)",
            "CommentContent( hi , 4..8)",
            "CommentEnd(8..11)",
        ]
    );
}

#[test]
fn empty_comment() {
    // < ! - - - - >
    // 0 1 2 3 4 5 6
    let events = parse_full(b"<!---->");
    assert_eq!(
        events,
        vec![
            "CommentStart(0..4)",
            "CommentEnd(4..7)",
        ]
    );
}

#[test]
fn comment_between_elements() {
    // < a / > <  !  -  -     x     -  -  >  <  b  /  >
    // 0 1 2 3 4  5  6  7  8  9  10 11 12 13 14 15 16 17
    let events = parse_full(b"<a/><!-- x --><b/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(a, 1..2)",
            "EmptyElementEnd(2..4)",
            "CommentStart(4..8)",
            "CommentContent( x , 8..11)",
            "CommentEnd(11..14)",
            "StartTagOpen(b, 15..16)",
            "EmptyElementEnd(16..18)",
        ]
    );
}

#[test]
fn split_simple_comment() {
    verify_all_splits(b"<!-- hello -->");
}

#[test]
fn split_empty_comment() {
    verify_all_splits(b"<!---->");
}

#[test]
fn split_comment_between_elements() {
    verify_all_splits(b"<a/><!-- x --><b/>");
}

// ========================================================================
// Processing Instruction tests
// ========================================================================

#[test]
fn simple_pi() {
    // < ? p  i    d  a  t  a  ?  >
    // 0 1 2  3 4  5  6  7  8  9  10
    let events = parse_full(b"<?pi data?>");
    assert_eq!(
        events,
        vec![
            "PIStart(pi, 2..4)",
            "PIContent(data, 5..9)",
            "PIEnd(9..11)",
        ]
    );
}

#[test]
fn pi_no_content() {
    // < ? x ?  >
    // 0 1 2 3 4
    let events = parse_full(b"<?x?>");
    assert_eq!(
        events,
        vec![
            "PIStart(x, 2..3)",
            "PIEnd(3..5)",
        ]
    );
}

#[test]
fn pi_between_elements() {
    // < a / > <  ?  p  i     d  a  t  a  ?  >  <  b  /  >
    // 0 1 2 3 4  5  6  7  8  9  10 11 12 13 14 15 16 17 18
    let events = parse_full(b"<a/><?pi data?><b/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(a, 1..2)",
            "EmptyElementEnd(2..4)",
            "PIStart(pi, 6..8)",
            "PIContent(data, 9..13)",
            "PIEnd(13..15)",
            "StartTagOpen(b, 16..17)",
            "EmptyElementEnd(17..19)",
        ]
    );
}

#[test]
fn split_simple_pi() {
    verify_all_splits(b"<?pi data?>");
}

#[test]
fn split_pi_no_content() {
    verify_all_splits(b"<?x?>");
}

#[test]
fn split_pi_between_elements() {
    verify_all_splits(b"<a/><?pi data?><b/>");
}

// ========================================================================
// XML Declaration tests
// ========================================================================

#[test]
fn xml_decl_version_only() {
    // < ? x m l   v e r s i  o  n  =  "  1  .  0  "  ?  >
    // 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20
    let events = parse_full(b"<?xml version=\"1.0\"?>");
    assert_eq!(
        events,
        vec!["XmlDeclaration(1.0, None, None, 0..21)"]
    );
}

#[test]
fn xml_decl_version_and_encoding() {
    let input = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>";
    let events = parse_full(input);
    assert_eq!(
        events,
        vec![format!("XmlDeclaration(1.0, Some(UTF-8), None, 0..{})", input.len())]
    );
}

#[test]
fn xml_decl_full() {
    let input = b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";
    let events = parse_full(input);
    assert_eq!(
        events,
        vec![format!("XmlDeclaration(1.0, Some(UTF-8), Some(true), 0..{})", input.len())]
    );
}

#[test]
fn xml_decl_standalone_no() {
    let input = b"<?xml version=\"1.0\" standalone=\"no\"?>";
    let events = parse_full(input);
    assert_eq!(
        events,
        vec![format!("XmlDeclaration(1.0, None, Some(false), 0..{})", input.len())]
    );
}

#[test]
fn xml_decl_single_quotes() {
    let input = b"<?xml version='1.0'?>";
    let events = parse_full(input);
    assert_eq!(
        events,
        vec![format!("XmlDeclaration(1.0, None, None, 0..{})", input.len())]
    );
}

#[test]
fn xml_decl_spaces_around_equals() {
    let input = b"<?xml version = \"1.0\" ?>";
    let events = parse_full(input);
    assert_eq!(
        events,
        vec![format!("XmlDeclaration(1.0, None, None, 0..{})", input.len())]
    );
}

#[test]
fn xml_decl_followed_by_element() {
    // <?xml version="1.0"?> is 21 bytes
    let events = parse_full(b"<?xml version=\"1.0\"?><root/>");
    assert_eq!(
        events,
        vec![
            "XmlDeclaration(1.0, None, None, 0..21)",
            "StartTagOpen(root, 22..26)",
            "EmptyElementEnd(26..28)",
        ]
    );
}

#[test]
fn xml_decl_no_content_error() {
    // <?xml?> - missing version
    let kind = expect_xml_error(b"<?xml?>");
    assert_eq!(kind, ErrorKind::MalformedXmlDeclaration);
}

#[test]
fn xml_decl_after_comment_error() {
    // Comment before XML declaration → ReservedPITarget
    let kind = expect_xml_error(b"<!-- comment --><?xml version=\"1.0\"?>");
    assert_eq!(kind, ErrorKind::ReservedPITarget);
}

#[test]
fn xml_decl_after_element_error() {
    // Element before XML declaration → ReservedPITarget
    let kind = expect_xml_error(b"<root/><?xml version=\"1.0\"?>");
    assert_eq!(kind, ErrorKind::ReservedPITarget);
}

#[test]
fn xml_decl_split_version_only() {
    verify_all_splits(b"<?xml version=\"1.0\"?>");
}

#[test]
fn xml_decl_split_full() {
    verify_all_splits(b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
}

#[test]
fn xml_decl_split_followed_by_element() {
    verify_all_splits(b"<?xml version=\"1.0\"?><root/>");
}

#[test]
fn xml_decl_after_comment_error_all_splits() {
    expect_xml_error_all_splits(b"<!-- comment --><?xml version=\"1.0\"?>");
}

#[test]
fn xml_decl_after_element_error_all_splits() {
    expect_xml_error_all_splits(b"<root/><?xml version=\"1.0\"?>");
}

// ========================================================================
// CDATA tests
// ========================================================================

#[test]
fn simple_cdata() {
    // < ! [ C D A T A [ h  e  l  l  o  ]  ]  >
    // 0 1 2 3 4 5 6 7 8 9  10 11 12 13 14 15 16
    let events = parse_full(b"<![CDATA[hello]]>");
    assert_eq!(
        events,
        vec![
            "CdataStart(0..9)",
            "CdataContent(hello, 9..14)",
            "CdataEnd(14..17)",
        ]
    );
}

#[test]
fn empty_cdata() {
    let events = parse_full(b"<![CDATA[]]>");
    assert_eq!(
        events,
        vec![
            "CdataStart(0..9)",
            "CdataEnd(9..12)",
        ]
    );
}

#[test]
fn cdata_with_markup_chars() {
    // CDATA can contain < > & etc.
    let events = parse_full(b"<![CDATA[<a>&b</c>]]>");
    assert_eq!(
        events,
        vec![
            "CdataStart(0..9)",
            "CdataContent(<a>&b</c>, 9..18)",
            "CdataEnd(18..21)",
        ]
    );
}

#[test]
fn split_simple_cdata() {
    verify_all_splits(b"<![CDATA[hello]]>");
}

#[test]
fn split_empty_cdata() {
    verify_all_splits(b"<![CDATA[]]>");
}

#[test]
fn split_cdata_with_markup() {
    verify_all_splits(b"<![CDATA[<a>&b</c>]]>");
}

// ========================================================================
// DOCTYPE tests
// ========================================================================

#[test]
fn simple_doctype() {
    // < ! D O C T Y P E   h  t  m  l  >
    // 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14
    let events = parse_full(b"<!DOCTYPE html>");
    assert_eq!(
        events,
        vec![
            "DoctypeStart(html, 10..14)",
            "DoctypeEnd(14..15)",
        ]
    );
}

#[test]
#[cfg(not(feature = "dtd"))]
fn doctype_with_internal_subset() {
    let events = parse_full(b"<!DOCTYPE html [<!ENTITY foo \"bar\">]>");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], "DoctypeStart(html, 10..14)");
    assert!(events[1].starts_with("DoctypeContent("));
    assert!(events[2].starts_with("DoctypeEnd("));
}

#[test]
fn split_simple_doctype() {
    verify_all_splits(b"<!DOCTYPE html>");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn split_doctype_with_internal_subset() {
    verify_all_splits(b"<!DOCTYPE html [<!ENTITY foo \"bar\">]>");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn doctype_comment_with_bracket() {
    // ] inside a comment must not decrement bracket depth
    let events = parse_full(b"<!DOCTYPE html [<!-- ] -->]><r/>");
    assert_eq!(events[0], "DoctypeStart(html, 10..14)");
    assert!(events[1].starts_with("DoctypeContent("));
    assert!(events[2].starts_with("DoctypeEnd("));
    assert_eq!(events[3], "StartTagOpen(r, 29..30)");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn doctype_comment_with_gt() {
    // > inside a comment must not close the DOCTYPE
    let events = parse_full(b"<!DOCTYPE html [<!-- > -->]><r/>");
    assert_eq!(events[0], "DoctypeStart(html, 10..14)");
    assert!(events[2].starts_with("DoctypeEnd("));
    assert_eq!(events[3], "StartTagOpen(r, 29..30)");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn doctype_quoted_bracket() {
    // ] inside a quoted string must not decrement bracket depth
    let events = parse_full(b"<!DOCTYPE html [<!ENTITY foo \"]bar\">]><r/>");
    assert_eq!(events[0], "DoctypeStart(html, 10..14)");
    assert!(events[2].starts_with("DoctypeEnd("));
    assert_eq!(events[3], "StartTagOpen(r, 39..40)");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn doctype_single_quoted_bracket() {
    // ] inside a single-quoted string must not decrement bracket depth
    let events = parse_full(b"<!DOCTYPE html [<!ENTITY foo ']bar'>]><r/>");
    assert_eq!(events[0], "DoctypeStart(html, 10..14)");
    assert!(events[2].starts_with("DoctypeEnd("));
    assert_eq!(events[3], "StartTagOpen(r, 39..40)");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn doctype_pi_with_bracket() {
    // ] inside a PI must not decrement bracket depth
    let events = parse_full(b"<!DOCTYPE html [<?pi ]?>]><r/>");
    assert_eq!(events[0], "DoctypeStart(html, 10..14)");
    assert!(events[2].starts_with("DoctypeEnd("));
    assert_eq!(events[3], "StartTagOpen(r, 27..28)");
}

#[test]
fn doctype_quoted_gt_no_subset() {
    // > inside a quoted string in external ID must not close DOCTYPE
    let events = parse_full(b"<!DOCTYPE html SYSTEM \"foo>.dtd\"><r/>");
    assert_eq!(events[0], "DoctypeStart(html, 10..14)");
    assert!(events[1].starts_with("DoctypeContent("));
    assert!(events[2].starts_with("DoctypeEnd("));
    assert_eq!(events[3], "StartTagOpen(r, 34..35)");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn split_doctype_comment_with_bracket() {
    verify_all_splits(b"<!DOCTYPE html [<!-- ] -->]><r/>");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn split_doctype_comment_with_gt() {
    verify_all_splits(b"<!DOCTYPE html [<!-- > -->]><r/>");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn split_doctype_quoted_bracket() {
    verify_all_splits(b"<!DOCTYPE html [<!ENTITY foo \"]bar\">]><r/>");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn split_doctype_single_quoted_bracket() {
    verify_all_splits(b"<!DOCTYPE html [<!ENTITY foo ']bar'>]><r/>");
}

#[test]
#[cfg(not(feature = "dtd"))]
fn split_doctype_pi_with_bracket() {
    verify_all_splits(b"<!DOCTYPE html [<?pi ]?>]><r/>");
}

#[test]
fn split_doctype_quoted_gt_no_subset() {
    verify_all_splits(b"<!DOCTYPE html SYSTEM \"foo>.dtd\"><r/>");
}

// ========================================================================
// Entity and character reference tests
// ========================================================================

#[test]
fn entity_ref_in_content() {
    // < p > &  a  m  p  ;  <  /  p  >
    // 0 1 2 3 4 5 6 7 8  9  10 11
    let events = parse_full(b"<p>&amp;</p>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(p, 1..2)",
            "StartTagClose(2..3)",
            "EntityRef(amp, 4..7)",
            "EndTag(p, 10..11)",
        ]
    );
}

#[test]
fn char_ref_decimal() {
    // < p > & # 6 0 ; <  /  p  >
    // 0 1 2 3 4 5 6 7 8  9  10 11
    let events = parse_full(b"<p>&#60;</p>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(p, 1..2)",
            "StartTagClose(2..3)",
            "CharRef(60, 5..7)",
            "EndTag(p, 10..11)",
        ]
    );
}

#[test]
fn char_ref_hex() {
    // < p > & # x 3 C ;  <  /  p  >
    // 0 1 2 3 4 5 6 7 8  9  10 11 12
    let events = parse_full(b"<p>&#x3C;</p>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(p, 1..2)",
            "StartTagClose(2..3)",
            "CharRef(x3C, 5..8)",
            "EndTag(p, 11..12)",
        ]
    );
}

#[test]
fn text_with_entity_ref() {
    let events = parse_full(b"<p>a&amp;b</p>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(p, 1..2)",
            "StartTagClose(2..3)",
            "Characters(a, 3..4)",
            "EntityRef(amp, 5..8)",
            "Characters(b, 9..10)",
            "EndTag(p, 12..13)",
        ]
    );
}

#[test]
fn multiple_entity_refs() {
    // < p > & l t ; &  g  t  ;  <  /  p  >
    // 0 1 2 3 4 5 6 7  8  9  10 11 12 13 14
    let events = parse_full(b"<p>&lt;&gt;</p>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(p, 1..2)",
            "StartTagClose(2..3)",
            "EntityRef(lt, 4..6)",
            "EntityRef(gt, 8..10)",
            "EndTag(p, 13..14)",
        ]
    );
}

#[test]
fn split_entity_ref() {
    verify_all_splits(b"<p>&amp;</p>");
}

#[test]
fn split_char_ref_decimal() {
    verify_all_splits(b"<p>&#60;</p>");
}

#[test]
fn split_char_ref_hex() {
    verify_all_splits(b"<p>&#x3C;</p>");
}

#[test]
fn split_text_with_entity_ref() {
    verify_all_splits(b"<p>a&amp;b</p>");
}

#[test]
fn split_multiple_entity_refs() {
    verify_all_splits(b"<p>&lt;&gt;</p>");
}

// ========================================================================
// Attribute entity/char ref tests
// ========================================================================

#[test]
fn entity_ref_in_attr_value() {
    // < e   a = " x &  a  m  p  ;  y  "  /  >
    // 0 1 2 3 4 5 6 7 8  9  10 11 12 13 14 15
    let events = parse_full(b"<e a=\"x&amp;y\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(e, 1..2)",
            "AttrName(a, 3..4)",
            "AttrValue(x, 6..7)",
            "AttrEntityRef(amp, 8..11)",
            "AttrValue(y, 12..13)",
            "AttrEnd(13..14)",
            "EmptyElementEnd(14..16)",
        ]
    );
}

#[test]
fn char_ref_decimal_in_attr_value() {
    // < e   a = " &  #  6  0  ;  "  /  >
    // 0 1 2 3 4 5 6 7  8  9  10 11 12 13
    let events = parse_full(b"<e a=\"&#60;\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(e, 1..2)",
            "AttrName(a, 3..4)",
            "AttrCharRef(60, 8..10)",
            "AttrEnd(11..12)",
            "EmptyElementEnd(12..14)",
        ]
    );
}

#[test]
fn char_ref_hex_in_attr_value() {
    // < e   a = " &  #  x  3  C  ;  "  /  >
    // 0 1 2 3 4 5 6 7  8  9  10 11 12 13 14
    let events = parse_full(b"<e a=\"&#x3C;\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(e, 1..2)",
            "AttrName(a, 3..4)",
            "AttrCharRef(x3C, 8..11)",
            "AttrEnd(12..13)",
            "EmptyElementEnd(13..15)",
        ]
    );
}

#[test]
fn multiple_entity_refs_in_attr_value() {
    let events = parse_full(b"<e a=\"&lt;&gt;\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(e, 1..2)",
            "AttrName(a, 3..4)",
            "AttrEntityRef(lt, 7..9)",
            "AttrEntityRef(gt, 11..13)",
            "AttrEnd(14..15)",
            "EmptyElementEnd(15..17)",
        ]
    );
}

#[test]
fn entity_ref_at_attr_value_start() {
    // < e   a = " &  a  m  p  ;  x  "  /  >
    // 0 1 2 3 4 5 6 7  8  9  10 11 12 13 14
    let events = parse_full(b"<e a=\"&amp;x\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(e, 1..2)",
            "AttrName(a, 3..4)",
            "AttrEntityRef(amp, 7..10)",
            "AttrValue(x, 11..12)",
            "AttrEnd(12..13)",
            "EmptyElementEnd(13..15)",
        ]
    );
}

#[test]
fn entity_ref_at_attr_value_end() {
    // < e   a = " x &  a  m  p  ;  "  /  >
    // 0 1 2 3 4 5 6 7 8  9  10 11 12 13 14
    let events = parse_full(b"<e a=\"x&amp;\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(e, 1..2)",
            "AttrName(a, 3..4)",
            "AttrValue(x, 6..7)",
            "AttrEntityRef(amp, 8..11)",
            "AttrEnd(12..13)",
            "EmptyElementEnd(13..15)",
        ]
    );
}

#[test]
fn entity_ref_only_attr_value() {
    // < e   a = " &  a  m  p  ;  "  /  >
    // 0 1 2 3 4 5 6 7  8  9  10 11 12 13
    let events = parse_full(b"<e a=\"&amp;\"/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(e, 1..2)",
            "AttrName(a, 3..4)",
            "AttrEntityRef(amp, 7..10)",
            "AttrEnd(11..12)",
            "EmptyElementEnd(12..14)",
        ]
    );
}

#[test]
fn entity_ref_in_single_quoted_attr() {
    let events = parse_full(b"<e a='&amp;'/>");
    assert_eq!(
        events,
        vec![
            "StartTagOpen(e, 1..2)",
            "AttrName(a, 3..4)",
            "AttrEntityRef(amp, 7..10)",
            "AttrEnd(11..12)",
            "EmptyElementEnd(12..14)",
        ]
    );
}

#[test]
fn split_entity_ref_in_attr_value() {
    verify_all_splits(b"<e a=\"x&amp;y\"/>");
}

#[test]
fn split_char_ref_decimal_in_attr_value() {
    verify_all_splits(b"<e a=\"&#60;\"/>");
}

#[test]
fn split_char_ref_hex_in_attr_value() {
    verify_all_splits(b"<e a=\"&#x3C;\"/>");
}

#[test]
fn split_multiple_entity_refs_in_attr_value() {
    verify_all_splits(b"<e a=\"&lt;&gt;\"/>");
}

#[test]
fn split_entity_ref_at_attr_value_start() {
    verify_all_splits(b"<e a=\"&amp;x\"/>");
}

#[test]
fn split_entity_ref_at_attr_value_end() {
    verify_all_splits(b"<e a=\"x&amp;\"/>");
}

#[test]
fn split_entity_ref_only_attr_value() {
    verify_all_splits(b"<e a=\"&amp;\"/>");
}

#[test]
fn split_entity_ref_in_single_quoted_attr() {
    verify_all_splits(b"<e a='&amp;'/>");
}

#[test]
fn long_attr_value() {
    // Attribute value exceeding old 10 MiB limit should work fine now
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<x a=\"");
    doc.extend_from_slice(&[b'v'; 11 * 1024 * 1024]); // 11 MiB
    doc.extend_from_slice(b"\"/>");
    let mut reader = Reader::new();
    let mut rec = Recorder::default();
    reader.parse(&doc, 0, true, &mut rec).expect("parse should succeed");
    assert!(rec.events.iter().any(|e| e.starts_with("AttrValue(")));
}

// ========================================================================
// Mixed document tests
// ========================================================================

#[test]
fn split_full_document() {
    verify_all_splits(
        b"<!DOCTYPE html><root><!-- comment --><?pi data?><![CDATA[cdata]]><p a=\"v\">&amp;</p></root>",
    );
}

// ========================================================================
// Encoding probe tests (integration)
// ========================================================================

use xml_syntax_reader::{probe_encoding, Encoding};

#[test]
fn probe_utf8_no_bom() {
    let result = probe_encoding(b"<root/>");
    assert_eq!(result.encoding, Encoding::Utf8);
    assert_eq!(result.bom_length, 0);
}

#[test]
fn probe_utf8_bom() {
    let result = probe_encoding(b"\xEF\xBB\xBF<root/>");
    assert_eq!(result.encoding, Encoding::Utf8);
    assert_eq!(result.bom_length, 3);
}

#[test]
fn probe_encoding_decl() {
    let result = probe_encoding(b"<?xml version=\"1.0\" encoding=\"UTF-16\"?>");
    match result.encoding {
        Encoding::Declared(enc) => assert_eq!(enc.as_str(), Some("UTF-16")),
        other => panic!("expected Declared, got {other:?}"),
    }
}

// ========================================================================
// Malformed input rejection tests
// ========================================================================

/// Parse a complete document expecting an XML error. Returns the ErrorKind.
fn expect_xml_error(input: &[u8]) -> ErrorKind {
    let mut reader = Reader::new();
    let mut rec = Recorder::default();
    match reader.parse(input, 0, true, &mut rec) {
        Err(ParseError::Xml(e)) => e.kind,
        Ok(_) => panic!(
            "expected XML error but parse succeeded for {:?}",
            String::from_utf8_lossy(input)
        ),
        Err(ParseError::Visitor(_)) => unreachable!(),
    }
}

/// Parse in all chunk sizes expecting an XML error for each.
fn expect_xml_error_all_splits(input: &[u8]) {
    let kind = expect_xml_error(input);
    for chunk_size in 1..=input.len() {
        let mut reader = Reader::new();
        let mut rec = Recorder::default();
        let mut stream_offset: u64 = 0;
        let mut buf = vec![0u8; input.len() + 64];
        let mut valid = 0usize;
        let mut input_pos = 0usize;
        let mut got_error = false;

        loop {
            let to_copy = (input.len() - input_pos).min(chunk_size);
            if to_copy > 0 {
                buf[valid..valid + to_copy]
                    .copy_from_slice(&input[input_pos..input_pos + to_copy]);
                valid += to_copy;
                input_pos += to_copy;
            }
            let is_final = input_pos >= input.len();
            if valid == 0 {
                break;
            }
            match reader.parse(&buf[..valid], stream_offset, is_final, &mut rec) {
                Ok(c) => {
                    let consumed = c as usize;
                    let leftover = valid - consumed;
                    if leftover > 0 {
                        buf.copy_within(consumed..valid, 0);
                    }
                    valid = leftover;
                    stream_offset += consumed as u64;
                    if consumed == 0 && to_copy == 0 {
                        break;
                    }
                }
                Err(ParseError::Xml(e)) => {
                    assert_eq!(
                        e.kind, kind,
                        "chunk_size={chunk_size}: expected {kind:?}, got {:?} for {:?}",
                        e.kind,
                        String::from_utf8_lossy(input),
                    );
                    got_error = true;
                    break;
                }
                Err(ParseError::Visitor(_)) => unreachable!(),
            }
        }
        assert!(
            got_error,
            "chunk_size={chunk_size}: expected error but parse succeeded for {:?}",
            String::from_utf8_lossy(input),
        );
    }
}

// --- ]]> in text content ---

#[test]
fn error_cdata_end_in_content() {
    assert_eq!(
        expect_xml_error(b"<r>]]></r>"),
        ErrorKind::CdataEndInContent,
    );
}

#[test]
fn error_cdata_end_in_content_multiple_brackets() {
    assert_eq!(
        expect_xml_error(b"<r>]]]></r>"),
        ErrorKind::CdataEndInContent,
    );
}

#[test]
fn single_bracket_ok_in_content() {
    // A single ] or ]] without > is fine
    let events = parse_full(b"<r>a]b</r>");
    assert!(events.iter().any(|e| e.contains("]")));
}

#[test]
fn double_bracket_without_gt_ok() {
    let events = parse_full(b"<r>a]]b</r>");
    assert!(events.iter().any(|e| e.contains("]]")));
}

#[test]
fn split_error_cdata_end_in_content() {
    expect_xml_error_all_splits(b"<r>]]></r>");
}

#[test]
fn split_error_cdata_end_in_content_multiple_brackets() {
    expect_xml_error_all_splits(b"<r>]]]></r>");
}

// --- -- inside comments ---

#[test]
fn error_double_dash_in_comment() {
    assert_eq!(
        expect_xml_error(b"<!-- -- -->"),
        ErrorKind::DoubleDashInComment,
    );
}

#[test]
fn error_double_dash_in_comment_no_space() {
    assert_eq!(
        expect_xml_error(b"<!-- --x -->"),
        ErrorKind::DoubleDashInComment,
    );
}

#[test]
fn valid_comment_with_single_dashes() {
    // Single dashes are fine in comments
    let events = parse_full(b"<!-- a-b-c -->");
    assert!(events.iter().any(|e| e.starts_with("CommentContent(")));
}

#[test]
fn valid_empty_comment() {
    // <!----> is a valid empty comment
    let events = parse_full(b"<!---->");
    assert!(events.iter().any(|e| e.starts_with("CommentEnd(")));
}

#[test]
fn valid_comment_single_dash_content() {
    // <!-- - --> is valid: content is " - "
    let events = parse_full(b"<!-- - -->");
    assert!(events.iter().any(|e| e.starts_with("CommentContent(")));
}

#[test]
fn split_error_double_dash_in_comment() {
    expect_xml_error_all_splits(b"<!-- -- -->");
}

#[test]
fn error_triple_dash_in_comment() {
    assert_eq!(
        expect_xml_error(b"<!-- --- -->"),
        ErrorKind::DoubleDashInComment,
    );
}

#[test]
fn error_triple_dash_closes_comment() {
    // "<!------->" is "<!--" + "---" + "-->" - the "---" triggers the error
    assert_eq!(
        expect_xml_error(b"<!------->"),
        ErrorKind::DoubleDashInComment,
    );
}

#[test]
fn split_error_triple_dash_in_comment() {
    expect_xml_error_all_splits(b"<!-- --- -->");
}

// --- empty entity reference &; ---

#[test]
fn error_empty_entity_ref() {
    assert_eq!(
        expect_xml_error(b"<r>&;</r>"),
        ErrorKind::ExpectedName(b';'),
    );
}

#[test]
fn split_error_empty_entity_ref() {
    expect_xml_error_all_splits(b"<r>&;</r>");
}

// --- entity reference name validation ---

#[test]
fn error_entity_ref_starts_with_digit() {
    assert_eq!(
        expect_xml_error(b"<r>&1foo;</r>"),
        ErrorKind::ExpectedName(b'1'),
    );
}

#[test]
fn error_entity_ref_contains_space() {
    assert_eq!(
        expect_xml_error(b"<r>&fo o;</r>"),
        ErrorKind::ExpectedName(b' '),
    );
}

#[test]
fn valid_entity_ref_with_dots_and_dashes() {
    // Entity names can contain dots, dashes, digits after the first char
    let events = parse_full(b"<r>&a.b-c1;</r>");
    assert!(events.iter().any(|e| e.starts_with("EntityRef(a.b-c1,")));
}

#[test]
fn split_error_entity_ref_starts_with_digit() {
    expect_xml_error_all_splits(b"<r>&1foo;</r>");
}

// --- empty / invalid character references ---

#[test]
fn error_empty_char_ref() {
    assert_eq!(
        expect_xml_error(b"<r>&#;</r>"),
        ErrorKind::InvalidCharRef,
    );
}

#[test]
fn error_empty_hex_char_ref() {
    assert_eq!(
        expect_xml_error(b"<r>&#x;</r>"),
        ErrorKind::InvalidCharRef,
    );
}

#[test]
fn error_char_ref_non_digit() {
    assert_eq!(
        expect_xml_error(b"<r>&#abc;</r>"),
        ErrorKind::InvalidCharRef,
    );
}

#[test]
fn error_hex_char_ref_non_hex() {
    assert_eq!(
        expect_xml_error(b"<r>&#xGG;</r>"),
        ErrorKind::InvalidCharRef,
    );
}

#[test]
fn valid_decimal_char_ref() {
    let events = parse_full(b"<r>&#60;</r>");
    assert!(events.iter().any(|e| e.starts_with("CharRef(60,")));
}

#[test]
fn valid_hex_char_ref() {
    let events = parse_full(b"<r>&#x3C;</r>");
    assert!(events.iter().any(|e| e.starts_with("CharRef(x3C,")));
}

#[test]
fn split_error_empty_char_ref() {
    expect_xml_error_all_splits(b"<r>&#;</r>");
}

#[test]
fn split_error_char_ref_non_digit() {
    expect_xml_error_all_splits(b"<r>&#abc;</r>");
}

// --- DOCTYPE with no name or missing whitespace ---

#[test]
fn error_doctype_no_whitespace() {
    assert_eq!(
        expect_xml_error(b"<!DOCTYPEhtml><html/>"),
        ErrorKind::DoctypeMissingWhitespace,
    );
}

#[test]
fn error_doctype_no_name() {
    assert_eq!(
        expect_xml_error(b"<!DOCTYPE ><r/>"),
        ErrorKind::DoctypeMissingName,
    );
}

#[test]
fn error_doctype_only_whitespace() {
    assert_eq!(
        expect_xml_error(b"<!DOCTYPE   ><r/>"),
        ErrorKind::DoctypeMissingName,
    );
}

#[test]
fn valid_doctype() {
    let events = parse_full(b"<!DOCTYPE html><html/>");
    assert!(events.iter().any(|e| e.starts_with("DoctypeStart(html,")));
}

#[test]
fn split_error_doctype_no_whitespace() {
    expect_xml_error_all_splits(b"<!DOCTYPEhtml><html/>");
}

#[test]
fn split_error_doctype_no_name() {
    expect_xml_error_all_splits(b"<!DOCTYPE ><r/>");
}

// ========================================================================
// UTF-8 boundary rewind tests
// ========================================================================

/// Helper: parse in two chunks and return the events from each chunk separately.
fn parse_two_chunks(chunk1: &[u8], chunk2: &[u8]) -> (Vec<String>, Vec<String>) {
    let mut reader = Reader::new();
    let mut rec1 = Recorder::default();

    let consumed = match reader.parse(chunk1, 0, false, &mut rec1) {
        Ok(c) => c as usize,
        Err(ParseError::Xml(e)) => panic!("XML error in chunk1 at offset {}: {}", e.offset, e.kind),
        Err(ParseError::Visitor(_)) => unreachable!(),
    };

    // Build buffer for second call: unconsumed tail + chunk2
    let leftover = &chunk1[consumed..];
    let mut buf2 = Vec::with_capacity(leftover.len() + chunk2.len());
    buf2.extend_from_slice(leftover);
    buf2.extend_from_slice(chunk2);

    let mut rec2 = Recorder::default();
    match reader.parse(&buf2, consumed as u64, true, &mut rec2) {
        Ok(_) => {}
        Err(ParseError::Xml(e)) => panic!("XML error in chunk2 at offset {}: {}", e.offset, e.kind),
        Err(ParseError::Visitor(_)) => unreachable!(),
    }

    (rec1.events, rec2.events)
}

#[test]
fn utf8_rewind_2byte_split() {
    // 2-byte UTF-8: U+00E9 = 0xC3 0xA9 (é)
    // Chunk1 ends mid-character: "abc" + first byte of é
    let chunk1 = b"<r>abc\xC3";
    let chunk2 = b"\xA9</r>";
    let (events1, events2) = parse_two_chunks(chunk1, chunk2);

    // First chunk: should get "abc" only (rewind excludes the incomplete 0xC3)
    assert!(
        events1.iter().any(|e| e.contains("Characters(abc,")),
        "chunk1 events: {events1:?}"
    );
    assert!(
        !events1.iter().any(|e| e.contains("\u{FFFD}")),
        "chunk1 should not contain replacement char: {events1:?}"
    );

    // Second chunk: should contain the complete é (0xC3 0xA9)
    assert!(
        events2.iter().any(|e| e.contains("Characters(\u{00E9},")),
        "chunk2 events: {events2:?}"
    );
}

#[test]
fn utf8_rewind_3byte_split() {
    // 3-byte UTF-8: U+4E2D = 0xE4 0xB8 0xAD (中)
    // Chunk1 ends with 2 of 3 bytes
    let chunk1 = b"<r>abc\xE4\xB8";
    let chunk2 = b"\xAD</r>";
    let (events1, events2) = parse_two_chunks(chunk1, chunk2);

    assert!(
        events1.iter().any(|e| e.contains("Characters(abc,")),
        "chunk1 events: {events1:?}"
    );

    assert!(
        events2.iter().any(|e| e.contains("Characters(\u{4E2D},")),
        "chunk2 events: {events2:?}"
    );
}

#[test]
fn utf8_rewind_4byte_split_1of4() {
    // 4-byte UTF-8: U+1F600 = 0xF0 0x9F 0x98 0x80 (😀)
    // Chunk1 ends with 1 of 4 bytes
    let chunk1 = b"<r>abc\xF0";
    let chunk2 = b"\x9F\x98\x80</r>";
    let (events1, events2) = parse_two_chunks(chunk1, chunk2);

    assert!(
        events1.iter().any(|e| e.contains("Characters(abc,")),
        "chunk1 events: {events1:?}"
    );

    assert!(
        events2.iter().any(|e| e.contains("Characters(\u{1F600},")),
        "chunk2 events: {events2:?}"
    );
}

#[test]
fn utf8_rewind_4byte_split_2of4() {
    // 4-byte UTF-8: U+1F600 = 0xF0 0x9F 0x98 0x80 (😀)
    // Chunk1 ends with 2 of 4 bytes
    let chunk1 = b"<r>abc\xF0\x9F";
    let chunk2 = b"\x98\x80</r>";
    let (events1, events2) = parse_two_chunks(chunk1, chunk2);

    assert!(
        events1.iter().any(|e| e.contains("Characters(abc,")),
        "chunk1 events: {events1:?}"
    );

    assert!(
        events2.iter().any(|e| e.contains("Characters(\u{1F600},")),
        "chunk2 events: {events2:?}"
    );
}

#[test]
fn utf8_complete_multibyte_at_end_no_rewind() {
    // Complete 2-byte sequence at end of chunk - no rewind needed
    let chunk1 = b"<r>abc\xC3\xA9";
    let chunk2 = b"</r>";
    let (events1, _events2) = parse_two_chunks(chunk1, chunk2);

    // First chunk should include the complete é
    assert!(
        events1.iter().any(|e| e.contains("abc\u{00E9}")),
        "chunk1 events: {events1:?}"
    );
}

#[test]
fn utf8_all_ascii_no_rewind() {
    // All-ASCII content - no rewind
    let chunk1 = b"<r>hello";
    let chunk2 = b"</r>";
    let (events1, _events2) = parse_two_chunks(chunk1, chunk2);

    assert!(
        events1.iter().any(|e| e.contains("Characters(hello,")),
        "chunk1 events: {events1:?}"
    );
}

#[test]
fn utf8_is_final_trailing_incomplete_no_rewind() {
    // When is_final=true, incomplete trailing UTF-8 is delivered as-is (no rewind)
    let mut reader = Reader::new();
    let mut rec = Recorder::default();
    // Buffer with incomplete 2-byte sequence at end, parsed with is_final=true
    let buf = b"<r>abc\xC3";
    let result = reader.parse(&buf[..], 0, true, &mut rec);
    // Should succeed (is_final - no rewind, just flush as-is)
    assert!(result.is_ok(), "is_final parse should succeed: {result:?}");
    // The characters event should contain all bytes including the incomplete one
    let all_text: Vec<&String> = rec.events.iter().filter(|e| e.starts_with("Characters(")).collect();
    assert!(!all_text.is_empty(), "should have characters events");
}

#[test]
fn utf8_invalid_trailing_continuation_bytes() {
    // 3 continuation bytes with no leader - invalid UTF-8
    let mut reader = Reader::new();
    let mut rec = Recorder::default();
    let buf = b"<r>\x80\x80\x80";
    match reader.parse(&buf[..], 0, false, &mut rec) {
        Err(ParseError::Xml(e)) => {
            assert_eq!(e.kind, ErrorKind::InvalidUtf8, "expected InvalidUtf8, got {:?}", e.kind);
        }
        Ok(_) => panic!("expected InvalidUtf8 error but parse succeeded"),
        Err(ParseError::Visitor(_)) => unreachable!(),
    }
}

// ========================================================================
// Token length limit tests
// ========================================================================

#[test]
fn error_element_name_too_long() {
    let mut doc = Vec::new();
    doc.push(b'<');
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.push(b'/');
    doc.push(b'>');
    assert_eq!(expect_xml_error(&doc), ErrorKind::NameTooLong);
}

#[test]
fn split_error_element_name_too_long() {
    let mut doc = Vec::new();
    doc.push(b'<');
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.push(b'/');
    doc.push(b'>');
    expect_xml_error_all_splits(&doc);
}

#[test]
fn element_name_at_limit() {
    // 1000 bytes is exactly at the limit - should succeed
    let mut doc = Vec::new();
    doc.push(b'<');
    doc.extend_from_slice(&[b'a'; 1000]);
    doc.extend_from_slice(b"/>");
    let events = parse_full(&doc);
    assert_eq!(events.len(), 2); // StartTagOpen + EmptyElementEnd
}

#[test]
fn error_attr_name_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<x ");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.extend_from_slice(b"=\"v\"/>");
    assert_eq!(expect_xml_error(&doc), ErrorKind::NameTooLong);
}

#[test]
fn split_error_attr_name_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<x ");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.extend_from_slice(b"=\"v\"/>");
    expect_xml_error_all_splits(&doc);
}

#[test]
fn error_end_tag_name_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"</");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.push(b'>');
    assert_eq!(expect_xml_error(&doc), ErrorKind::NameTooLong);
}


#[test]
fn error_pi_target_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<?");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.extend_from_slice(b"?>");
    assert_eq!(expect_xml_error(&doc), ErrorKind::NameTooLong);
}

#[test]
fn split_error_pi_target_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<?");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.extend_from_slice(b"?>");
    expect_xml_error_all_splits(&doc);
}

#[test]
fn error_entity_ref_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<r>&");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.extend_from_slice(b";</r>");
    assert_eq!(expect_xml_error(&doc), ErrorKind::NameTooLong);
}

#[test]
fn split_error_entity_ref_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<r>&");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.extend_from_slice(b";</r>");
    expect_xml_error_all_splits(&doc);
}

#[test]
fn error_char_ref_too_long() {
    // 8 digits exceeds the 7-byte limit (max valid: &#1114111; or &#x10FFFF;)
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<r>&#");
    doc.extend_from_slice(&[b'1'; 8]);
    doc.extend_from_slice(b";</r>");
    assert_eq!(expect_xml_error(&doc), ErrorKind::CharRefTooLong);
}

#[test]
fn split_error_char_ref_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<r>&#");
    doc.extend_from_slice(&[b'1'; 8]);
    doc.extend_from_slice(b";</r>");
    expect_xml_error_all_splits(&doc);
}

#[test]
fn error_doctype_name_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<!DOCTYPE ");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.push(b'>');
    assert_eq!(expect_xml_error(&doc), ErrorKind::NameTooLong);
}

#[test]
fn split_error_doctype_name_too_long() {
    let mut doc = Vec::new();
    doc.extend_from_slice(b"<!DOCTYPE ");
    doc.extend_from_slice(&[b'a'; 1001]);
    doc.push(b'>');
    expect_xml_error_all_splits(&doc);
}

#[test]
#[cfg(not(feature = "dtd"))]
fn error_doctype_nested_bracket() {
    // `[` inside the internal subset is not valid in well-formed XML
    let doc = b"<!DOCTYPE html [[]>";
    assert_eq!(expect_xml_error(doc), ErrorKind::ExpectedKeyword(b'['));
}

#[test]
#[cfg(not(feature = "dtd"))]
fn split_error_doctype_nested_bracket() {
    let doc = b"<!DOCTYPE html [[]>";
    expect_xml_error_all_splits(doc);
}

// ── parse_slice tests ──────────────────────────────────────────────────────

#[test]
fn parse_slice_simple() {
    let input = b"<root>hello</root>";
    let mut reader = Reader::new();
    let mut rec = Recorder::default();
    reader.parse_slice(input, &mut rec).unwrap();
    assert_eq!(
        rec.events,
        parse_full(input),
    );
}

#[test]
fn parse_slice_empty_input() {
    let mut reader = Reader::new();
    let mut rec = Recorder::default();
    let consumed = reader.parse_slice(b"", &mut rec).unwrap();
    assert_eq!(consumed, 0);
    assert!(rec.events.is_empty());
}

// ── parse_read tests ───────────────────────────────────────────────────────

#[test]
fn parse_read_simple() {
    let input = b"<root>hello</root>";
    let mut rec = Recorder::default();
    xml_syntax_reader::parse_read(&input[..], &mut rec).unwrap();
    assert_eq!(rec.events, parse_full(input));
}

#[test]
fn parse_read_matches_chunked() {
    let input = b"<doc attr=\"value\">text &amp; more<!-- comment --></doc>";
    let mut rec = Recorder::default();
    xml_syntax_reader::parse_read(&input[..], &mut rec).unwrap();
    assert_eq!(
        coalesce_content_events(&rec.events),
        coalesce_content_events(&parse_full(input)),
    );
}

#[test]
fn parse_read_with_small_capacity() {
    // Force many read-parse-shift cycles with a tiny buffer
    let input = b"<root><a/><b>text</b></root>";
    let mut rec = Recorder::default();
    xml_syntax_reader::parse_read_with_capacity(&input[..], &mut rec, 64).unwrap();
    assert_eq!(
        coalesce_content_events(&rec.events),
        coalesce_content_events(&parse_full(input)),
    );
}

#[test]
fn parse_read_empty_input() {
    let mut rec = Recorder::default();
    xml_syntax_reader::parse_read(&b""[..], &mut rec).unwrap();
    assert!(rec.events.is_empty());
}

#[test]
fn parse_read_xml_error() {
    let input = b"<root>]]></root>";
    let mut rec = Recorder::default();
    let err = xml_syntax_reader::parse_read(&input[..], &mut rec);
    assert!(matches!(err, Err(xml_syntax_reader::ReadError::Xml(e)) if e.kind == ErrorKind::CdataEndInContent));
}

// ── DTD feature tests ──────────────────────────────────────────────────────

#[cfg(feature = "dtd")]
mod dtd_tests {
    use super::*;

    // ── ENTITY declaration ──────────────────────────────────────────────

    #[test]
    fn dtd_entity_simple_value() {
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY foo \"bar\">]><r/>");
        assert_eq!(events[0], "DoctypeStart(r, 10..11)");
        assert_eq!(events[1], "SubsetStart(12..13)");
        assert_eq!(events[2], "EntityDeclStart(foo, pe=false, 22..25)");
        assert_eq!(events[3], "EntityDeclValue(bar, 27..30)");
        assert_eq!(events[4], "EntityDeclValueEnd(30..31)");
        assert_eq!(events[5], "EntityDeclEnd(31..32)");
        assert_eq!(events[6], "SubsetEnd(32..33)");
        assert_eq!(events[7], "DoctypeEnd(33..34)");
    }

    #[test]
    fn split_dtd_entity_simple_value() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY foo \"bar\">]><r/>");
    }

    #[test]
    fn dtd_entity_with_entity_ref() {
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY foo \"a&amp;b\">]><r/>");
        assert_eq!(events[2], "EntityDeclStart(foo, pe=false, 22..25)");
        assert_eq!(events[3], "EntityDeclValue(a, 27..28)");
        assert_eq!(events[4], "EntityDeclEntityRef(amp, 28..33)");
        assert_eq!(events[5], "EntityDeclValue(b, 33..34)");
        assert_eq!(events[6], "EntityDeclValueEnd(34..35)");
        assert_eq!(events[7], "EntityDeclEnd(35..36)");
    }

    #[test]
    fn split_dtd_entity_with_entity_ref() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY foo \"a&amp;b\">]><r/>");
    }

    #[test]
    fn dtd_entity_with_char_ref() {
        // <!DOCTYPE r [<!ENTITY foo "&#60;">]><r/>
        // char ref &#60; spans bytes 27..32 (from & to ;)
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY foo \"&#60;\">]><r/>");
        assert_eq!(events[2], "EntityDeclStart(foo, pe=false, 22..25)");
        assert_eq!(events[3], "EntityDeclCharRef(60, 27..32)");
        assert_eq!(events[4], "EntityDeclValueEnd(32..33)");
        assert_eq!(events[5], "EntityDeclEnd(33..34)");
    }

    #[test]
    fn split_dtd_entity_with_char_ref() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY foo \"&#60;\">]><r/>");
    }

    #[test]
    fn dtd_entity_with_pe_ref() {
        // <!DOCTYPE r [<!ENTITY foo "%bar;">]><r/>
        // PE ref %bar; spans bytes 27..32 (from % to ;)
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY foo \"%bar;\">]><r/>");
        assert_eq!(events[2], "EntityDeclStart(foo, pe=false, 22..25)");
        assert_eq!(events[3], "EntityDeclPeRef(bar, 27..32)");
        assert_eq!(events[4], "EntityDeclValueEnd(32..33)");
    }

    #[test]
    fn split_dtd_entity_with_pe_ref() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY foo \"%bar;\">]><r/>");
    }

    #[test]
    fn dtd_parameter_entity() {
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY % dtd SYSTEM \"common.dtd\">]><r/>");
        assert_eq!(events[2], "EntityDeclStart(dtd, pe=true, 24..27)");
        assert_eq!(events[3], "EntityDeclSystemId(common.dtd, 36..46)");
        assert_eq!(events[4], "EntityDeclEnd(47..48)");
    }

    #[test]
    fn split_dtd_parameter_entity() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY % dtd SYSTEM \"common.dtd\">]><r/>");
    }

    #[test]
    fn dtd_entity_public() {
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY logo PUBLIC \"-//Ex//Logo\" \"logo.gif\" NDATA gif>]><r/>");
        assert_eq!(events[2], "EntityDeclStart(logo, pe=false, 22..26)");
        assert_eq!(events[3], "EntityDeclPublicId(-//Ex//Logo, 35..46)");
        assert_eq!(events[4], "EntityDeclSystemId(logo.gif, 49..57)");
        assert_eq!(events[5], "EntityDeclNdata(gif, 65..68)");
        assert_eq!(events[6], "EntityDeclEnd(68..69)");
    }

    #[test]
    fn split_dtd_entity_public() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY logo PUBLIC \"-//Ex//Logo\" \"logo.gif\" NDATA gif>]><r/>");
    }

    #[test]
    fn dtd_entity_single_quoted_value() {
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY foo 'bar'>]><r/>");
        assert_eq!(events[3], "EntityDeclValue(bar, 27..30)");
        assert_eq!(events[4], "EntityDeclValueEnd(30..31)");
    }

    #[test]
    fn split_dtd_entity_single_quoted_value() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY foo 'bar'>]><r/>");
    }

    // ── ELEMENT declaration ─────────────────────────────────────────────

    #[test]
    fn dtd_element_empty() {
        let events = parse_full(b"<!DOCTYPE r [<!ELEMENT br EMPTY>]><r/>");
        assert_eq!(events[2], "ElementDeclStart(br, 23..25)");
        assert_eq!(events[3], "ElementDeclEmpty(26..31)");
        assert_eq!(events[4], "ElementDeclEnd(31..32)");
    }

    #[test]
    fn split_dtd_element_empty() {
        verify_all_splits(b"<!DOCTYPE r [<!ELEMENT br EMPTY>]><r/>");
    }

    #[test]
    fn dtd_element_any() {
        let events = parse_full(b"<!DOCTYPE r [<!ELEMENT div ANY>]><r/>");
        assert_eq!(events[2], "ElementDeclStart(div, 23..26)");
        assert_eq!(events[3], "ElementDeclAny(27..30)");
        assert_eq!(events[4], "ElementDeclEnd(30..31)");
    }

    #[test]
    fn split_dtd_element_any() {
        verify_all_splits(b"<!DOCTYPE r [<!ELEMENT div ANY>]><r/>");
    }

    #[test]
    fn dtd_element_content_model() {
        let events = parse_full(b"<!DOCTYPE r [<!ELEMENT p (#PCDATA|em)*>]><r/>");
        assert_eq!(events[2], "ElementDeclStart(p, 23..24)");
        assert_eq!(events[3], "ElementDeclContentSpec((#PCDATA|em)*, 25..38)");
        assert_eq!(events[4], "ElementDeclEnd(38..39)");
    }

    #[test]
    fn split_dtd_element_content_model() {
        verify_all_splits(b"<!DOCTYPE r [<!ELEMENT p (#PCDATA|em)*>]><r/>");
    }

    // ── NOTATION declaration ────────────────────────────────────────────

    #[test]
    fn dtd_notation_system() {
        let events = parse_full(b"<!DOCTYPE r [<!NOTATION gif SYSTEM \"image/gif\">]><r/>");
        assert_eq!(events[2], "NotationDeclStart(gif, 24..27)");
        assert_eq!(events[3], "NotationDeclSystemId(image/gif, 36..45)");
        assert_eq!(events[4], "NotationDeclEnd(46..47)");
    }

    #[test]
    fn split_dtd_notation_system() {
        verify_all_splits(b"<!DOCTYPE r [<!NOTATION gif SYSTEM \"image/gif\">]><r/>");
    }

    #[test]
    fn dtd_notation_public() {
        let events = parse_full(b"<!DOCTYPE r [<!NOTATION gif PUBLIC \"-//Ex//Gif\" \"image/gif\">]><r/>");
        assert_eq!(events[2], "NotationDeclStart(gif, 24..27)");
        assert_eq!(events[3], "NotationDeclPublicId(-//Ex//Gif, 36..46)");
        assert_eq!(events[4], "NotationDeclSystemId(image/gif, 49..58)");
        assert_eq!(events[5], "NotationDeclEnd(59..60)");
    }

    #[test]
    fn split_dtd_notation_public() {
        verify_all_splits(b"<!DOCTYPE r [<!NOTATION gif PUBLIC \"-//Ex//Gif\" \"image/gif\">]><r/>");
    }

    // ── ATTLIST declaration ─────────────────────────────────────────────

    #[test]
    fn dtd_attlist_required() {
        let events = parse_full(b"<!DOCTYPE r [<!ATTLIST img src CDATA #REQUIRED>]><r/>");
        assert_eq!(events[2], "AttlistDeclStart(img, 23..26)");
        assert_eq!(events[3], "AttlistAttrName(src, 27..30)");
        assert_eq!(events[4], "AttlistAttrType(CDATA, 31..36)");
        assert_eq!(events[5], "AttlistAttrRequired(37..46)");
        assert_eq!(events[6], "AttlistDeclEnd(46..47)");
    }

    #[test]
    fn split_dtd_attlist_required() {
        verify_all_splits(b"<!DOCTYPE r [<!ATTLIST img src CDATA #REQUIRED>]><r/>");
    }

    #[test]
    fn dtd_attlist_implied() {
        let events = parse_full(b"<!DOCTYPE r [<!ATTLIST x a CDATA #IMPLIED>]><r/>");
        assert_eq!(events[5], "AttlistAttrImplied(33..41)");
    }

    #[test]
    fn split_dtd_attlist_implied() {
        verify_all_splits(b"<!DOCTYPE r [<!ATTLIST x a CDATA #IMPLIED>]><r/>");
    }

    #[test]
    fn dtd_attlist_default_value() {
        let events = parse_full(b"<!DOCTYPE r [<!ATTLIST x a CDATA \"default\">]><r/>");
        assert_eq!(events[5], "AttlistAttrDefaultStart(fixed=false, 33..34)");
        assert_eq!(events[6], "AttlistAttrDefaultValue(default, 34..41)");
        assert_eq!(events[7], "AttlistAttrDefaultEnd(41..42)");
    }

    #[test]
    fn split_dtd_attlist_default_value() {
        verify_all_splits(b"<!DOCTYPE r [<!ATTLIST x a CDATA \"default\">]><r/>");
    }

    #[test]
    fn dtd_attlist_fixed_value() {
        let events = parse_full(b"<!DOCTYPE r [<!ATTLIST x a CDATA #FIXED \"val\">]><r/>");
        assert_eq!(events[5], "AttlistAttrDefaultStart(fixed=true, 40..41)");
        assert_eq!(events[6], "AttlistAttrDefaultValue(val, 41..44)");
        assert_eq!(events[7], "AttlistAttrDefaultEnd(44..45)");
    }

    #[test]
    fn split_dtd_attlist_fixed_value() {
        verify_all_splits(b"<!DOCTYPE r [<!ATTLIST x a CDATA #FIXED \"val\">]><r/>");
    }

    #[test]
    fn dtd_attlist_enumerated_type() {
        let events = parse_full(b"<!DOCTYPE r [<!ATTLIST x a (one|two) #REQUIRED>]><r/>");
        assert_eq!(events[4], "AttlistAttrType((one|two), 27..36)");
    }

    #[test]
    fn split_dtd_attlist_enumerated_type() {
        verify_all_splits(b"<!DOCTYPE r [<!ATTLIST x a (one|two) #REQUIRED>]><r/>");
    }

    #[test]
    fn dtd_attlist_default_with_entity_ref() {
        let events = parse_full(b"<!DOCTYPE r [<!ATTLIST x a CDATA \"one&amp;two\">]><r/>");
        assert_eq!(events[6], "AttlistAttrDefaultValue(one, 34..37)");
        assert_eq!(events[7], "AttlistAttrDefaultEntityRef(amp, 37..42)");
        assert_eq!(events[8], "AttlistAttrDefaultValue(two, 42..45)");
        assert_eq!(events[9], "AttlistAttrDefaultEnd(45..46)");
    }

    #[test]
    fn split_dtd_attlist_default_with_entity_ref() {
        verify_all_splits(b"<!DOCTYPE r [<!ATTLIST x a CDATA \"one&amp;two\">]><r/>");
    }

    #[test]
    fn dtd_attlist_multiple_attrs() {
        let events = parse_full(b"<!DOCTYPE r [<!ATTLIST x a CDATA #REQUIRED b CDATA #IMPLIED>]><r/>");
        assert_eq!(events[3], "AttlistAttrName(a, 25..26)");
        assert_eq!(events[4], "AttlistAttrType(CDATA, 27..32)");
        assert_eq!(events[5], "AttlistAttrRequired(33..42)");
        assert_eq!(events[6], "AttlistAttrName(b, 43..44)");
        assert_eq!(events[7], "AttlistAttrType(CDATA, 45..50)");
        assert_eq!(events[8], "AttlistAttrImplied(51..59)");
    }

    #[test]
    fn split_dtd_attlist_multiple_attrs() {
        verify_all_splits(b"<!DOCTYPE r [<!ATTLIST x a CDATA #REQUIRED b CDATA #IMPLIED>]><r/>");
    }

    // ── Comment/PI in internal subset ───────────────────────────────────

    #[test]
    fn dtd_comment_in_subset() {
        let events = parse_full(b"<!DOCTYPE r [<!-- hello -->]><r/>");
        assert_eq!(events[2], "CommentStart(13..17)");
        assert_eq!(events[3], "CommentContent( hello , 17..24)");
        assert_eq!(events[4], "CommentEnd(24..27)");
        assert_eq!(events[5], "SubsetEnd(27..28)");
    }

    #[test]
    fn split_dtd_comment_in_subset() {
        verify_all_splits(b"<!DOCTYPE r [<!-- hello -->]><r/>");
    }

    #[test]
    fn dtd_pi_in_subset() {
        let events = parse_full(b"<!DOCTYPE r [<?pi data?>]><r/>");
        assert_eq!(events[2], "PIStart(pi, 13..17)");
        assert_eq!(events[3], "PIContent(data, 18..22)");
        assert_eq!(events[4], "PIEnd(22..24)");
    }

    #[test]
    fn split_dtd_pi_in_subset() {
        verify_all_splits(b"<!DOCTYPE r [<?pi data?>]><r/>");
    }

    // ── PE reference ────────────────────────────────────────────────────

    #[test]
    fn dtd_pe_reference() {
        let events = parse_full(b"<!DOCTYPE r [%ISOLat1;]><r/>");
        assert_eq!(events[2], "DtdPeRef(ISOLat1, 13..22)");
        assert_eq!(events[3], "SubsetEnd(22..23)");
    }

    #[test]
    fn split_dtd_pe_reference() {
        verify_all_splits(b"<!DOCTYPE r [%ISOLat1;]><r/>");
    }

    // ── Comment/PI with brackets (DTD-mode equivalents of cfg-gated tests) ──

    #[test]
    fn dtd_comment_with_bracket_in_subset() {
        // ] inside a comment must not end the internal subset
        let events = parse_full(b"<!DOCTYPE r [<!-- ] -->]><r/>");
        assert_eq!(events[0], "DoctypeStart(r, 10..11)");
        assert_eq!(events[2], "CommentStart(13..17)");
        assert!(events.iter().any(|e| e.starts_with("SubsetEnd(")));
        assert!(events.iter().any(|e| e.starts_with("DoctypeEnd(")));
        assert!(events.iter().any(|e| e.starts_with("StartTagOpen(r,")));
    }

    #[test]
    fn split_dtd_comment_with_bracket_in_subset() {
        verify_all_splits(b"<!DOCTYPE r [<!-- ] -->]><r/>");
    }

    #[test]
    fn dtd_comment_with_gt_in_subset() {
        // > inside a comment must not close the DOCTYPE
        let events = parse_full(b"<!DOCTYPE r [<!-- > -->]><r/>");
        assert!(events.iter().any(|e| e.starts_with("CommentStart(")));
        assert!(events.iter().any(|e| e.starts_with("DoctypeEnd(")));
        assert!(events.iter().any(|e| e.starts_with("StartTagOpen(r,")));
    }

    #[test]
    fn split_dtd_comment_with_gt_in_subset() {
        verify_all_splits(b"<!DOCTYPE r [<!-- > -->]><r/>");
    }

    #[test]
    fn dtd_entity_value_with_bracket() {
        // ] inside an entity value must not end the internal subset
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY foo \"]bar\">]><r/>");
        assert!(events.iter().any(|e| e.starts_with("EntityDeclStart(foo,")));
        assert!(events.iter().any(|e| e.starts_with("SubsetEnd(")));
        assert!(events.iter().any(|e| e.starts_with("StartTagOpen(r,")));
    }

    #[test]
    fn split_dtd_entity_value_with_bracket() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY foo \"]bar\">]><r/>");
    }

    #[test]
    fn dtd_entity_single_quoted_value_with_bracket() {
        let events = parse_full(b"<!DOCTYPE r [<!ENTITY foo ']bar'>]><r/>");
        assert!(events.iter().any(|e| e.starts_with("EntityDeclStart(foo,")));
        assert!(events.iter().any(|e| e.starts_with("SubsetEnd(")));
        assert!(events.iter().any(|e| e.starts_with("StartTagOpen(r,")));
    }

    #[test]
    fn split_dtd_entity_single_quoted_value_with_bracket() {
        verify_all_splits(b"<!DOCTYPE r [<!ENTITY foo ']bar'>]><r/>");
    }

    #[test]
    fn dtd_pi_with_bracket_in_subset() {
        let events = parse_full(b"<!DOCTYPE r [<?pi ]?>]><r/>");
        assert!(events.iter().any(|e| e.starts_with("PIStart(pi,")));
        assert!(events.iter().any(|e| e.starts_with("SubsetEnd(")));
        assert!(events.iter().any(|e| e.starts_with("StartTagOpen(r,")));
    }

    #[test]
    fn split_dtd_pi_with_bracket_in_subset() {
        verify_all_splits(b"<!DOCTYPE r [<?pi ]?>]><r/>");
    }

    // ── Multiple declarations ───────────────────────────────────────────

    #[test]
    fn dtd_multiple_declarations() {
        let events = parse_full(
            b"<!DOCTYPE r [\n\
              <!ELEMENT p ANY>\n\
              <!ENTITY foo \"bar\">\n\
              <!NOTATION gif SYSTEM \"image/gif\">\n\
              ]><r/>"
        );
        assert!(events.iter().any(|e| e.starts_with("ElementDeclStart(p,")));
        assert!(events.iter().any(|e| e.starts_with("EntityDeclStart(foo,")));
        assert!(events.iter().any(|e| e.starts_with("NotationDeclStart(gif,")));
        assert!(events.iter().any(|e| e.starts_with("SubsetEnd(")));
    }

    #[test]
    fn split_dtd_multiple_declarations() {
        verify_all_splits(
            b"<!DOCTYPE r [\n\
              <!ELEMENT p ANY>\n\
              <!ENTITY foo \"bar\">\n\
              <!NOTATION gif SYSTEM \"image/gif\">\n\
              ]><r/>"
        );
    }

    // ── Error tests ─────────────────────────────────────────────────────

    #[test]
    fn dtd_error_invalid_markup() {
        // <!X is not a valid DTD declaration
        assert_eq!(
            expect_xml_error(b"<!DOCTYPE r [<!X>]><r/>"),
            ErrorKind::DtdInvalidMarkup,
        );
    }

    #[test]
    fn split_dtd_error_invalid_markup() {
        expect_xml_error_all_splits(b"<!DOCTYPE r [<!X>]><r/>");
    }

    #[test]
    fn dtd_error_unexpected_byte_in_subset() {
        // bare `@` is not valid in internal subset
        assert_eq!(
            expect_xml_error(b"<!DOCTYPE r [@]><r/>"),
            ErrorKind::ExpectedName(b'@'),
        );
    }

    #[test]
    fn dtd_error_entity_decl_missing_ws() {
        assert_eq!(
            expect_xml_error(b"<!DOCTYPE r [<!ENTITYfoo \"bar\">]><r/>"),
            ErrorKind::DtdDeclMissingWhitespace,
        );
    }

    #[test]
    fn split_dtd_error_entity_decl_missing_ws() {
        expect_xml_error_all_splits(b"<!DOCTYPE r [<!ENTITYfoo \"bar\">]><r/>");
    }

    #[test]
    fn dtd_error_element_decl_missing_ws() {
        assert_eq!(
            expect_xml_error(b"<!DOCTYPE r [<!ELEMENTbr EMPTY>]><r/>"),
            ErrorKind::DtdDeclMissingWhitespace,
        );
    }

    #[test]
    fn split_dtd_error_element_decl_missing_ws() {
        expect_xml_error_all_splits(b"<!DOCTYPE r [<!ELEMENTbr EMPTY>]><r/>");
    }

    // ── Empty internal subset ───────────────────────────────────────────

    #[test]
    fn dtd_empty_subset() {
        let events = parse_full(b"<!DOCTYPE r []><r/>");
        assert_eq!(events[0], "DoctypeStart(r, 10..11)");
        assert_eq!(events[1], "SubsetStart(12..13)");
        assert_eq!(events[2], "SubsetEnd(13..14)");
        assert_eq!(events[3], "DoctypeEnd(14..15)");
    }

    #[test]
    fn split_dtd_empty_subset() {
        verify_all_splits(b"<!DOCTYPE r []><r/>");
    }

    #[test]
    fn dtd_subset_with_whitespace_only() {
        let events = parse_full(b"<!DOCTYPE r [ \n\t ]><r/>");
        assert_eq!(events[0], "DoctypeStart(r, 10..11)");
        assert_eq!(events[1], "SubsetStart(12..13)");
        assert_eq!(events[2], "SubsetEnd(17..18)");
        assert_eq!(events[3], "DoctypeEnd(18..19)");
    }

    #[test]
    fn split_dtd_subset_with_whitespace_only() {
        verify_all_splits(b"<!DOCTYPE r [ \n\t ]><r/>");
    }

    #[test]
    fn dtd_subset_after_whitespace() {
        // Whitespace between ] and > after internal subset
        let events = parse_full(b"<!DOCTYPE r [] ><r/>");
        assert_eq!(events[3], "DoctypeEnd(15..16)");
    }

    #[test]
    fn split_dtd_subset_after_whitespace() {
        verify_all_splits(b"<!DOCTYPE r [] ><r/>");
    }
}
