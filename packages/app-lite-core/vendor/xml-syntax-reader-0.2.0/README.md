# xml-syntax-reader

A high-performance, zero-copy, streaming XML syntax reader for Rust.

This is a *syntax* reader, not a full XML parser. It tokenizes well-formed XML into events (start tags, attributes, text, comments, etc.) without building a tree, resolving namespaces, or expanding entity references. It validates syntactic well-formedness constraints that are detectable at the lexical level, but does not check higher-level rules like tag matching or DTD conformance.

## Usage

```rust
use xml_syntax_reader::{Reader, Visitor, Span};

struct MyVisitor;

impl Visitor for MyVisitor {
    type Error = std::convert::Infallible;

    fn start_tag_open(&mut self, name: &[u8], span: Span) -> Result<(), Self::Error> {
        println!("element: {} at {}..{}", String::from_utf8_lossy(name), span.start, span.end);
        Ok(())
    }

    fn characters(&mut self, text: &[u8], span: Span) -> Result<(), Self::Error> {
        println!("text: {:?} at {}..{}", String::from_utf8_lossy(text), span.start, span.end);
        Ok(())
    }

    // ... implement other callbacks as needed; defaults are no-ops
}

fn main() {
    let xml = b"<greeting>Hello, world!</greeting>";
    let mut reader = Reader::new();
    let mut visitor = MyVisitor;
    reader.parse_slice(xml, &mut visitor).unwrap();
}
```

### Streaming

The reader is designed for streaming use. `parse()` borrows the caller's buffer, processes as much as possible, and returns the number of bytes consumed. The caller shifts unconsumed bytes to the front, appends more data, and calls `parse()` again:

```rust
use xml_syntax_reader::{Reader, Visitor, Span};
use std::io::Read;

struct CountVisitor { count: u64 }
impl Visitor for CountVisitor {
    type Error = std::convert::Infallible;
    fn start_tag_open(&mut self, _: &[u8], _: Span) -> Result<(), Self::Error> {
        self.count += 1;
        Ok(())
    }
}

fn count_elements(mut input: impl Read) -> std::io::Result<u64> {
    let mut reader = Reader::new();
    let mut visitor = CountVisitor { count: 0 };
    let mut buf = vec![0u8; 64 * 1024];
    let mut valid = 0usize;
    let mut stream_offset = 0u64;

    loop {
        let n = input.read(&mut buf[valid..])?;
        valid += n;
        let is_final = n == 0;

        let consumed = reader
            .parse(&buf[..valid], stream_offset, is_final, &mut visitor)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e:?}")))?
            as usize;

        buf.copy_within(consumed..valid, 0);
        valid -= consumed;
        stream_offset += consumed as u64;

        if is_final { break; }
    }
    Ok(visitor.count)
}
```

### Buffer size requirements

Because visitor callbacks receive `&[u8]` slices that are zero-copy references
into the caller's buffer, the parser cannot emit a callback for a construct
until all of its bytes are contiguous in the buffer. Certain constructs -
element names, attribute names, end tags, entity references, PI targets,
DOCTYPE names, and the XML declaration - are **atomic**: the parser holds
unconsumed bytes from the opening delimiter (e.g. `<`, `&`) until the closing
delimiter is found. During this window `parse()` cannot advance past the
opening delimiter, so those bytes accumulate in the buffer.

The largest atomic construct is a DOCTYPE declaration with a maximum-length
name: `<!DOCTYPE` (9 bytes) + whitespace (1) + name (up to 1,000 bytes) +
delimiter (1) = **1,011 bytes**. If the buffer is too small to hold the
complete atomic construct, `parse()` will return `consumed == 0` on each call,
the buffer will fill with unconsumed bytes, and no further progress can be
made. When `is_final` is then set to `true`, this produces an `UnexpectedEof`
error.

**Minimum buffer size: 4 KiB (4,096 bytes).** - This is roughly 4× the
theoretical minimum and provides comfortable headroom. A fixed-size buffer is
sufficient - there is no need to grow it dynamically, because the 1,000-byte
name length limit bounds the worst case.

**Recommended default: 8 KiB (8,192 bytes)** - this is what `parse_read` uses
internally.

The built-in `parse_read_with_capacity` clamps its capacity to a minimum of 64
bytes (one SIMD block), but callers writing their own streaming loop should use
at least 4 KiB to avoid stalling on legal XML with long names.

## Events

The `Visitor` trait provides callbacks for all XML syntax constructs:

| Callback | Trigger | Data |
|---|---|---|
| `start_tag_open` | `<name` | element name |
| `attribute_name` | attribute name in start tag | attribute name |
| `attribute_value` | `"value"` or `'value'` | raw value (without quotes) |
| `attribute_entity_ref` | `&name;` in attribute value | entity name |
| `attribute_char_ref` | `&#60;` or `&#x3C;` in attribute value | value between `&#` and `;` |
| `attribute_end` | closing quote of attribute value | |
| `start_tag_close` | `>` | |
| `empty_element_end` | `/>` | |
| `end_tag` | `</name>` | element name |
| `characters` | text content | raw text |
| `entity_ref` | `&name;` | entity name |
| `char_ref` | `&#60;` or `&#x3C;` | value between `&#` and `;` |
| `cdata_start` / `cdata_content` / `cdata_end` | `<![CDATA[...]]>` | raw content |
| `comment_start` / `comment_content` / `comment_end` | `<!--...-->` | comment text |
| `xml_declaration` | `<?xml ...?>` | version, encoding, standalone |
| `pi_start` / `pi_content` / `pi_end` | `<?target ...?>` | target name, content |
| `doctype_start` / `doctype_content` / `doctype_end` | `<!DOCTYPE ...>` | root name, opaque content |

All `&[u8]` slices are zero-copy references into the caller's buffer. Every event includes a `Span` with absolute byte offsets into the input stream.

Attribute values may be segmented at entity/char-ref boundaries and buffer boundaries - `attribute_value` fires for each text segment, interleaved with `attribute_entity_ref` / `attribute_char_ref` callbacks. Empty segments are omitted, so an attribute whose value is empty or consists entirely of references produces zero `attribute_value` calls.

Text content between markup is delivered as interleaved `characters`, `entity_ref`, and `char_ref` callbacks. For example, `a&amp;b` produces `characters("a")`, `entity_ref("amp")`, `characters("b")`.

Content bodies (`cdata_content`, `comment_content`, `pi_content`, `doctype_content`) fire zero or more times per construct - zero for empty constructs (e.g. `<!---->`, `<?target?>`), and more than once when content spans buffer boundaries.

## Error Handling

The parser rejects malformed input with specific error kinds:

| Error | Trigger |
|---|---|
| `UnexpectedByte(u8)` | Invalid byte in the current parsing context |
| `UnexpectedEof` | Input ends inside an incomplete construct |
| `CdataEndInContent` | `]]>` in text content |
| `DoubleDashInComment` | `--` inside a comment body |
| `InvalidCharRef` | Empty or non-numeric character reference |
| `DoctypeMissingWhitespace` | Missing whitespace after `<!DOCTYPE` keyword |
| `DoctypeMissingName` | Missing or invalid name in `<!DOCTYPE` declaration |
| `InvalidUtf8` | Invalid UTF-8 byte sequence |
| `NameTooLong` | Name exceeds 1,000-byte limit |
| `CharRefTooLong` | Character reference exceeds 7-byte limit |
| `DoctypeBracketsTooDeep` | DOCTYPE bracket nesting exceeds 1,024 depth limit |
| `MalformedXmlDeclaration` | Malformed XML declaration (missing version, bad syntax) |
| `ReservedPITarget` | PI target matching `xml` (case-insensitive) after document start |

Errors include the absolute byte offset where the problem was detected.

## Convenience Functions

For in-memory documents, `parse_slice` avoids the streaming boilerplate:

```rust
let mut reader = Reader::new();
reader.parse_slice(xml, &mut visitor).unwrap();
```

For `std::io::Read` sources, `parse_read` manages the buffer internally:

```rust
use xml_syntax_reader::{parse_read, Visitor, Span};

let file = std::fs::File::open("input.xml").unwrap();
let mut visitor = MyVisitor;
parse_read(file, &mut visitor).unwrap();
```

`parse_read_with_capacity` allows specifying the buffer size. The capacity is
clamped to a minimum of 64 bytes, but at least **4 KiB** is recommended to
avoid stalling on legal XML with long names (see
[Buffer size requirements](#buffer-size-requirements)).

## Encoding Detection

`probe_encoding` examines the first few bytes of a document for a BOM and/or XML declaration to determine the encoding:

```rust
use xml_syntax_reader::{probe_encoding, Encoding};

let result = probe_encoding(b"\xEF\xBB\xBF<?xml version=\"1.0\"?>");
assert_eq!(result.encoding, Encoding::Utf8);
assert_eq!(result.bom_length, 3);
```

Supported detections: UTF-8, UTF-16 LE/BE, UTF-32 LE/BE, and encoding names declared in XML declarations.

### UTF-8 Handling

The parser operates on raw bytes and assumes its input is UTF-8. It does **not** fully validate that every byte sequence in the document is valid UTF-8, nor does it transcode from other encodings. To safely reject documents in invalid or unsupported encodings, callers should take these steps:

1. **Probe the encoding** before parsing. Call `probe_encoding()` on the first bytes of the document. If the result is anything other than `Encoding::Utf8` (e.g. UTF-16, UTF-32, or a declared encoding like `ISO-8859-1`), either transcode the input to UTF-8 before feeding it to the reader, or reject the document.

2. **Strip the BOM** if present. `probe_encoding()` returns a `bom_length` - skip that many bytes when passing data to the reader. (A UTF-8 BOM is harmless to the parser but may appear as content in the first text event.)

3. **Validate UTF-8 in visitor callbacks.** The parser delivers `&[u8]` slices, not `&str`. It guarantees that multi-byte UTF-8 sequences are never split across `characters()` calls at buffer boundaries, so `std::str::from_utf8()` on any individual callback slice will not fail due to a buffer-boundary split. However, it **will** fail if the source data contains genuinely invalid UTF-8. Call `std::str::from_utf8()` (or your own validation) on the slices you care about and reject the document if it fails.

4. **Check the `xml_declaration` encoding attribute.** The `xml_declaration` callback receives the declared `encoding` value (if any). A document that declares `encoding="UTF-8"` (or omits the attribute, which defaults to UTF-8) and passes UTF-8 validation in step 3 is safe. A document that declares a non-UTF-8 encoding should be transcoded or rejected.

In summary: `probe_encoding()` detects the transport encoding, the reader handles byte-level tokenization, and the caller is responsible for validating that the bytes are actually valid UTF-8.

## `no_std` Support

The crate supports `no_std` environments. The `std` feature is enabled by default and adds:

- **Runtime SIMD detection** via `is_x86_feature_detected!` - selects AVX2, SSE2, or scalar at runtime.
- **`parse_read` / `parse_read_with_capacity`** - convenience functions for `std::io::Read` sources.
- **`ReadError`** type (wraps `std::io::Error`).

To use without `std`:

```toml
[dependencies]
xml-syntax-reader = { version = "...", default-features = false }
```

SIMD backend selection falls back to compile-time `target_feature` detection. If you compile with `-C target-feature=+avx2`, the AVX2 backend is used; otherwise it falls back to scalar. `Reader`, `parse_slice`, `Visitor`, `probe_encoding`, and all error types remain available.

## Security Considerations

The parser enforces hardcoded limits to bound resource consumption from untrusted input:

- **Name length**: element names, attribute names, PI targets, DOCTYPE names, and entity reference names are capped at **1,000 bytes** (`NameTooLong`).
- **Character reference length**: the value between `&#` and `;` is capped at **7 bytes** - the longest valid reference is `&#x10FFFF;` or `&#1114111;` (`CharRefTooLong`).
- **DOCTYPE bracket nesting**: internal subset `[` nesting is capped at **1,024 levels** (`DoctypeBracketsTooDeep`).

Text content, attribute values, and content bodies (comments, CDATA, PIs, DOCTYPE) have **no size limit** - they are streamed in chunks to the visitor, so memory usage is bounded by the caller's buffer size, not by document size.

All `unsafe` code is confined to SIMD intrinsics in `src/bitstream/`. The parser logic itself contains no `unsafe` blocks.

## DTD Support

The optional `dtd` cargo feature enables full tokenization of DTD internal subsets:

```toml
[dependencies]
xml-syntax-reader = { version = "...", features = ["dtd"] }
```

When enabled, the parser tokenizes declarations inside `<!DOCTYPE name [ ... ]>` instead of delivering them as opaque `doctype_content` chunks. The `Visitor` trait provides callbacks for:

| Declaration | Callbacks |
|---|---|
| `<!ELEMENT>` | `element_decl_start`, `element_decl_empty` / `element_decl_any` / `element_decl_content_spec`, `element_decl_end` |
| `<!ATTLIST>` | `attlist_decl_start`, `attlist_attr_name`, `attlist_attr_type`, `attlist_attr_required` / `attlist_attr_implied` / `attlist_attr_default_*`, `attlist_decl_end` |
| `<!ENTITY>` | `entity_decl_start` (with `is_parameter` flag), `entity_decl_value` / `entity_decl_system_id` / `entity_decl_public_id` / `entity_decl_ndata`, `entity_decl_end` |
| `<!NOTATION>` | `notation_decl_start`, `notation_decl_system_id` / `notation_decl_public_id`, `notation_decl_end` |
| `%name;` | `dtd_pe_reference` |
| Comments / PIs | Same callbacks as outside DTD |

Entity declaration values and ATTLIST default values are streamed with interleaved `entity_ref`, `char_ref`, and `pe_ref` callbacks, just like attribute values in the main document.

ELEMENT content models (e.g. `(a|b,(c,d)*)`) are delivered as raw bytes via `element_decl_content_spec` with parenthesis depth tracked by the parser.

The DTD visitor methods are defined on the `Visitor` trait unconditionally (with default no-op implementations), so visitor code compiles with or without the feature. Only the parser logic is gated.

Additional errors with `dtd` enabled:

| Error | Trigger |
|---|---|
| `LiteralTooLong` | System/public literal exceeds 8,192-byte limit |
| `DtdParensTooDeep` | Content model / enumeration parenthesis nesting exceeds limit |

## Beyond Syntax

This crate is a syntax reader, not a conformant XML processor. If you are building a higher-level layer on top (namespace resolution, DOM construction, validation), you need to know exactly what gaps exist relative to the [XML Information Set](https://www.w3.org/TR/xml-infoset/). This section catalogs them.

### Well-formedness constraints not checked

The XML 1.0 spec requires all conformant processors to enforce these rules. This parser deliberately skips them:

- **Tag matching** - `<a></b>` is accepted without error. The parser does not track a stack of open elements.
- **Attribute uniqueness** - `<e a="x" a="y">` is accepted. Duplicate attribute names are not detected.
- **Character validation** - bytes in text, attributes, comments, and PIs are not checked against the XML `Char` production. Control characters like U+0000 pass through.
- **Character reference range** - `&#0;` and `&#xD800;` are not rejected. The parser validates the *syntax* of character references (digits, hex digits, terminating `;`) but not that the decoded codepoint is a legal XML character.
- **Namespace prefix binding** - the parser does not enforce that `xml` and `xmlns` prefixes are used correctly. This is a namespace-level constraint.

### Information the parser does not provide

The XML Information Set defines these as properties of information items. This parser does not deliver them:

- **Namespace URI / local name** - names are reported verbatim (e.g. `ns:elem`). No prefix resolution is performed.
- **Entity expansion** - `&amp;` is reported as `entity_ref("amp")`, not expanded to `&`. This applies to all five predefined entities and any DTD-defined entities.
- **Character reference decoding** - `&#60;` is reported as `char_ref("60")`, not decoded to `<`.
- **Attribute value normalization** - raw bytes between quotes are delivered as-is. No whitespace normalization (tabs/newlines to spaces, leading/trailing stripping for tokenized types) is applied.
- **Default attributes** - DTD-declared defaults are not applied; only attributes present in the source are reported.
- **Document structure** - no tree, no parent/child relationships, no document-order guarantees beyond event sequence.
- **Base URI** - not tracked.
- **Notations and unparsed entities** - not reported.

### DTD and external features not processed

- **Internal subset** - without the `dtd` feature, delivered as opaque `doctype_content` chunks. With the `dtd` feature enabled, declarations are fully tokenized (see [DTD Support](#dtd-support)), but their semantics are not applied (no entity expansion, no attribute defaulting, no content model validation).
- **External subset** - not fetched or processed.
- **External entities** - not resolved.
- **Standalone enforcement** - `standalone="yes"` is parsed and reported via `xml_declaration`, but its constraints (no external markup declarations affecting content, no attribute defaulting, no normalization changes) are not enforced.
