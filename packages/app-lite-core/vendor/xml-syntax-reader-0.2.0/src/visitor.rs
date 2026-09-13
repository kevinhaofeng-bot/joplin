use crate::types::{EntityKind, QName, Span};

/// Trait for receiving fine-grained XML parsing events.
///
/// All `&[u8]` slices are references into the caller's buffer.
/// Return `Ok(())` to continue parsing, or `Err(Self::Error)` to abort.
///
/// Default implementations do nothing and return `Ok(())`.
///
/// # Callback sequences
///
/// The parser emits callbacks in these patterns for each XML construct.
/// `*` means zero or more, `+` means one or more, `|` means alternatives.
///
/// ## Start tag
///
/// ```text
/// start_tag_open
///   (attribute_name  attribute-value-sequence  attribute_end)*
///   start_tag_close | empty_element_end
/// ```
///
/// `<img src="a.png" alt="pic"/>`:
/// ```text
/// start_tag_open("img")
/// attribute_name("src")
/// attribute_value("a.png")
/// attribute_end
/// attribute_name("alt")
/// attribute_value("pic")
/// attribute_end
/// empty_element_end
/// ```
///
/// `<p>`:
/// ```text
/// start_tag_open("p")
/// start_tag_close
/// ```
///
/// ## Attribute value sequence
///
/// Between the quotes of a single attribute:
/// ```text
/// (attribute_value | attribute_entity_ref | attribute_char_ref)*
/// ```
///
/// The value is segmented at entity/character reference boundaries and at
/// buffer boundaries. Empty attribute values and values consisting solely
/// of references produce zero `attribute_value` calls. `attribute_end`
/// always fires exactly once per attribute, after the closing quote.
///
/// `class="a&amp;b"`:
/// ```text
/// attribute_name("class")
/// attribute_value("a")
/// attribute_entity_ref("amp")
/// attribute_value("b")
/// attribute_end
/// ```
///
/// `v="&amp;"` (ref-only - no `attribute_value` calls):
/// ```text
/// attribute_name("v")
/// attribute_entity_ref("amp")
/// attribute_end
/// ```
///
/// `v=""` (empty - no `attribute_value` calls):
/// ```text
/// attribute_name("v")
/// attribute_end
/// ```
///
/// ## End tag
///
/// `</div>`:
/// ```text
/// end_tag("div")
/// ```
///
/// ## Text content
///
/// When present between markup:
/// ```text
/// (characters | entity_ref | char_ref)+
/// ```
///
/// Not all elements have text content - e.g. `<p></p>` produces no text
/// events between `start_tag_close` and `end_tag`. When text is present,
/// it is segmented at entity/character reference boundaries and at buffer
/// boundaries. There is no trailing `characters` call after a final
/// reference.
///
/// `hello &amp; world`:
/// ```text
/// characters("hello ")
/// entity_ref("amp")
/// characters(" world")
/// ```
///
/// `&lt;&gt;` (references only - no `characters` calls):
/// ```text
/// entity_ref("lt")
/// entity_ref("gt")
/// ```
///
/// ## CDATA section
///
/// `cdata_start → cdata_content* → cdata_end`
///
/// `<![CDATA[hello]]>`:
/// ```text
/// cdata_start
/// cdata_content("hello")
/// cdata_end
/// ```
///
/// `<![CDATA[]]>` (empty - no `cdata_content` call):
/// ```text
/// cdata_start
/// cdata_end
/// ```
///
/// ## Comment
///
/// `comment_start → comment_content* → comment_end`
///
/// `<!-- hi -->`:
/// ```text
/// comment_start
/// comment_content(" hi ")
/// comment_end
/// ```
///
/// `<!---->` (empty - no `comment_content` call):
/// ```text
/// comment_start
/// comment_end
/// ```
///
/// ## Processing instruction
///
/// `pi_start → pi_content* → pi_end`
///
/// Leading whitespace between the target and content is consumed by the
/// parser and not included in `pi_content`.
///
/// `<?pi data?>`:
/// ```text
/// pi_start("pi")
/// pi_content("data")
/// pi_end
/// ```
///
/// `<?x?>` (no content - no `pi_content` call):
/// ```text
/// pi_start("x")
/// pi_end
/// ```
///
/// ## DOCTYPE declaration
///
/// `doctype_start → doctype_content* → doctype_end`
///
/// Content is opaque (not further parsed).
///
/// `<!DOCTYPE html [<!ENTITY foo "bar">]>`:
/// ```text
/// doctype_start("html")
/// doctype_content(" [<!ENTITY foo \"bar\">]")
/// doctype_end
/// ```
///
/// `<!DOCTYPE html>` (no content - no `doctype_content` call):
/// ```text
/// doctype_start("html")
/// doctype_end
/// ```
///
/// ## XML declaration
///
/// A single `xml_declaration` call (never chunked).
///
/// `<?xml version="1.0" encoding="UTF-8"?>`:
/// ```text
/// xml_declaration(version="1.0", encoding=Some("UTF-8"), standalone=None)
/// ```
///
/// For CDATA, comments, PIs, and DOCTYPE, the content callback may fire
/// more than once when the content spans buffer boundaries.
pub trait Visitor {
    type Error;

    // --- Element events ---

    /// Start tag opened: `<name`.
    /// `name` is the element name, with optional namespace prefix accessible
    /// via [`QName::prefix()`] and [`QName::local_name()`].
    /// The byte span is available via [`QName::span()`].
    fn start_tag_open(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        let _ = name;
        Ok(())
    }

    /// Attribute name within a start tag, with optional namespace prefix
    /// accessible via [`QName::prefix()`] and [`QName::local_name()`].
    /// The byte span is available via [`QName::span()`].
    fn attribute_name(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        let _ = name;
        Ok(())
    }

    /// Attribute value text (between entity/char ref boundaries or buffer boundaries).
    /// The surrounding quotes are **not** included.
    ///
    /// Called zero or more times per attribute, segmented at entity/char
    /// reference boundaries and buffer boundaries. Not called for empty
    /// segments - an attribute whose value is empty or consists entirely
    /// of references produces zero `attribute_value` calls.
    fn attribute_value(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (value, span);
        Ok(())
    }

    /// End of an attribute value (the closing quote was consumed).
    fn attribute_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// Entity reference in attribute value: `&name;`.
    /// `name` is the entity name without `&` and `;`.
    fn attribute_entity_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// Character reference in attribute value: `&#NNN;` or `&#xHHH;`.
    /// `value` is the raw text between `&#` and `;` (e.g. `"60"` or `"x3C"`).
    fn attribute_char_ref(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (value, span);
        Ok(())
    }

    /// Start tag closed with `>`.
    fn start_tag_close(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// Empty element closed with `/>`.
    fn empty_element_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// End tag: `</name>`.
    /// `name` is the element name, with optional namespace prefix accessible
    /// via [`QName::prefix()`] and [`QName::local_name()`].
    /// The byte span is available via [`QName::span()`].
    fn end_tag(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        let _ = name;
        Ok(())
    }

    // --- Text events ---

    /// Character data between markup.
    ///
    /// May be called multiple times for a single run of text content -
    /// interleaved with [`entity_ref`](Self::entity_ref) and
    /// [`char_ref`](Self::char_ref) calls at reference boundaries, and
    /// split at buffer boundaries. For example, `a&amp;b` produces
    /// `characters("a")`, `entity_ref("amp")`, `characters("b")`.
    ///
    /// Each `text` slice is guaranteed to not split a multi-byte UTF-8 character
    /// at its boundaries (except when `is_final` is true and the document ends
    /// mid-sequence). If the input is valid UTF-8, `std::str::from_utf8(text)`
    /// will always succeed.
    fn characters(&mut self, text: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (text, span);
        Ok(())
    }

    /// Entity reference in text content: `&name;`.
    /// `name` is the entity name without `&` and `;`.
    fn entity_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// Character reference in text content: `&#NNN;` or `&#xHHH;`.
    /// `value` is the raw text between `&#` and `;` (e.g. `"60"` or `"x3C"`).
    fn char_ref(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (value, span);
        Ok(())
    }

    // --- CDATA ---

    /// Start of a CDATA section: `<![CDATA[`.
    fn cdata_start(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// Content within a CDATA section.
    /// Called zero or more times for a single CDATA section - zero for
    /// empty sections (`<![CDATA[]]>`), and possibly more than once when
    /// content spans buffer boundaries. Consecutive calls have contiguous
    /// spans.
    fn cdata_content(&mut self, text: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (text, span);
        Ok(())
    }

    /// End of a CDATA section: `]]>`.
    fn cdata_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- Comments ---

    /// Start of a comment: `<!--`.
    fn comment_start(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// Content within a comment.
    /// Called zero or more times for a single comment - zero for empty
    /// comments (`<!---->`), and possibly more than once when content
    /// spans buffer boundaries. Consecutive calls have contiguous spans.
    fn comment_content(&mut self, text: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (text, span);
        Ok(())
    }

    /// End of a comment: `-->`.
    fn comment_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- XML Declaration ---

    /// XML declaration: `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>`.
    ///
    /// Fired instead of PI callbacks when `<?xml ...?>` appears at the document
    /// start. Per the XML specification, the XML declaration is NOT a processing
    /// instruction - it is a distinct construct.
    ///
    /// `version` is always present (e.g. `b"1.0"`).
    /// `encoding` and `standalone` are optional.
    fn xml_declaration(
        &mut self,
        version: &[u8],
        encoding: Option<&[u8]>,
        standalone: Option<bool>,
        span: Span,
    ) -> Result<(), Self::Error> {
        let _ = (version, encoding, standalone, span);
        Ok(())
    }

    // --- Processing Instructions ---

    /// Start of a processing instruction: `<?target`.
    /// `target` is the PI target name.
    fn pi_start(&mut self, target: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (target, span);
        Ok(())
    }

    /// Content of a processing instruction (everything between target and `?>`).
    /// Called zero or more times for a single PI - zero when the PI has no
    /// content (`<?target?>`), and possibly more than once when content
    /// spans buffer boundaries. Consecutive calls have contiguous spans.
    fn pi_content(&mut self, data: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (data, span);
        Ok(())
    }

    /// End of a processing instruction: `?>`.
    fn pi_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- DOCTYPE ---

    /// Start of a DOCTYPE declaration: `<!DOCTYPE name`.
    /// `name` is the root element name.
    fn doctype_start(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// Content within a DOCTYPE declaration (opaque).
    /// Called zero or more times for a single DOCTYPE - zero for simple
    /// declarations (`<!DOCTYPE html>`), and possibly more than once when
    /// content spans buffer boundaries. Consecutive calls have contiguous
    /// spans.
    fn doctype_content(&mut self, content: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (content, span);
        Ok(())
    }

    /// End of a DOCTYPE declaration: `>`.
    fn doctype_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- DTD Internal Subset Events ---
    //
    // These methods are always present on the trait (with default no-op impls)
    // regardless of feature flags. The `dtd` feature controls whether the
    // *parser* tokenizes the internal subset or treats it as opaque bytes.
    //
    // # Callback sequences (when `dtd` feature is enabled)
    //
    // ## DOCTYPE with external ID and internal subset
    //
    // ```text
    // doctype_start("html")
    //   doctype_system_id("foo.dtd")
    //   doctype_internal_subset_start
    //     element_decl_start("p") element_decl_content_spec("(#PCDATA)") element_decl_end
    //     entity_decl_start("amp", false)
    //       entity_decl_char_ref("38") entity_decl_value_end
    //     entity_decl_end
    //   doctype_internal_subset_end
    // doctype_end
    // ```
    //
    // Comments and PIs inside the internal subset reuse the existing
    // `comment_*` and `pi_*` callbacks.

    /// System identifier literal from DOCTYPE external ID.
    /// `literal` is the content between quotes (quotes not included).
    /// For `PUBLIC`, this is the second literal.
    fn doctype_system_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (literal, span);
        Ok(())
    }

    /// Public identifier literal from DOCTYPE external ID.
    /// `literal` is the content between quotes (quotes not included).
    /// For `PUBLIC`, this is the first literal; `doctype_system_id` follows.
    fn doctype_public_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (literal, span);
        Ok(())
    }

    /// Start of the internal subset: `[`.
    fn doctype_internal_subset_start(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// End of the internal subset: `]`.
    fn doctype_internal_subset_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- ELEMENT declaration ---

    /// Start of `<!ELEMENT name ...>`. `name` is the element name.
    fn element_decl_start(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// Content spec keyword `EMPTY`.
    fn element_decl_empty(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// Content spec keyword `ANY`.
    fn element_decl_any(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// Content spec raw bytes (parenthesized content model).
    /// May be called multiple times if the model spans buffer boundaries.
    /// Includes the parentheses and any trailing `?`, `*`, `+`.
    fn element_decl_content_spec(&mut self, content: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (content, span);
        Ok(())
    }

    /// End of `<!ELEMENT>`: the closing `>`.
    fn element_decl_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- ATTLIST declaration ---

    /// Start of `<!ATTLIST name ...>`. `name` is the element name.
    fn attlist_decl_start(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// Attribute name within an ATTLIST declaration.
    fn attlist_attr_name(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// Attribute type (raw bytes).
    /// For keyword types: `"CDATA"`, `"ID"`, `"IDREF"`, etc.
    /// For enumerations: `"(a|b|c)"` or `"NOTATION (x|y)"`.
    /// May be called multiple times if the type spans buffer boundaries.
    fn attlist_attr_type(&mut self, content: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (content, span);
        Ok(())
    }

    /// Default declaration: `#REQUIRED`.
    fn attlist_attr_required(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// Default declaration: `#IMPLIED`.
    fn attlist_attr_implied(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// Start of a default attribute value.
    /// `fixed` is `true` if `#FIXED` preceded the value.
    fn attlist_attr_default_start(&mut self, fixed: bool, span: Span) -> Result<(), Self::Error> {
        let _ = (fixed, span);
        Ok(())
    }

    /// Text segment of a default attribute value.
    fn attlist_attr_default_value(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (value, span);
        Ok(())
    }

    /// Entity reference `&name;` in a default attribute value.
    fn attlist_attr_default_entity_ref(
        &mut self,
        name: &[u8],
        span: Span,
    ) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// Character reference `&#NNN;` or `&#xHHH;` in a default attribute value.
    fn attlist_attr_default_char_ref(
        &mut self,
        value: &[u8],
        span: Span,
    ) -> Result<(), Self::Error> {
        let _ = (value, span);
        Ok(())
    }

    /// End of a default attribute value (closing quote consumed).
    fn attlist_attr_default_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// End of `<!ATTLIST>`: the closing `>`.
    fn attlist_decl_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- ENTITY declaration ---

    /// Start of `<!ENTITY name ...>` or `<!ENTITY % name ...>`.
    fn entity_decl_start(
        &mut self,
        name: &[u8],
        kind: EntityKind,
        span: Span,
    ) -> Result<(), Self::Error> {
        let _ = (name, kind, span);
        Ok(())
    }

    /// Text segment of an entity value.
    fn entity_decl_value(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (value, span);
        Ok(())
    }

    /// Entity reference `&name;` in an entity value.
    fn entity_decl_entity_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// Character reference `&#NNN;` or `&#xHHH;` in an entity value.
    fn entity_decl_char_ref(&mut self, value: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (value, span);
        Ok(())
    }

    /// Parameter entity reference `%name;` in an entity value.
    fn entity_decl_pe_ref(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// End of an entity value (closing quote consumed).
    fn entity_decl_value_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    /// NDATA declaration in a general entity: `NDATA name`.
    fn entity_decl_ndata(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// System identifier in an entity's external ID.
    fn entity_decl_system_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (literal, span);
        Ok(())
    }

    /// Public identifier in an entity's external ID.
    fn entity_decl_public_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (literal, span);
        Ok(())
    }

    /// End of `<!ENTITY>`: the closing `>`.
    fn entity_decl_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- NOTATION declaration ---

    /// Start of `<!NOTATION name ...>`. `name` is the notation name.
    fn notation_decl_start(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }

    /// System identifier in a notation declaration.
    fn notation_decl_system_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (literal, span);
        Ok(())
    }

    /// Public identifier in a notation declaration.
    fn notation_decl_public_id(&mut self, literal: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (literal, span);
        Ok(())
    }

    /// End of `<!NOTATION>`: the closing `>`.
    fn notation_decl_end(&mut self, span: Span) -> Result<(), Self::Error> {
        let _ = span;
        Ok(())
    }

    // --- Parameter entity reference in internal subset ---

    /// Parameter entity reference at the top level of the internal subset: `%name;`.
    /// `name` is the entity name without `%` and `;`.
    fn dtd_pe_reference(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        let _ = (name, span);
        Ok(())
    }
}
