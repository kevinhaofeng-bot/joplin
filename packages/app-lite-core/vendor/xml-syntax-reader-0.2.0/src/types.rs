/// Whether an `<!ENTITY ...>` declaration defines a general or parameter entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    /// General entity, e.g. `<!ENTITY name "value">`.
    General,
    /// Parameter entity, e.g. `<!ENTITY % name "value">`.
    Parameter,
}

/// A qualified name from the XML source, providing zero-copy access to both
/// the full raw name and its prefix/local-name components, along with the
/// byte span in the input stream.
///
/// The underlying `&[u8]` slice points directly into the caller's buffer.
/// For unprefixed names like `div`, [`prefix()`](Self::prefix) returns `None`
/// and [`local_name()`](Self::local_name) returns the full name.
/// For prefixed names like `svg:rect`, `prefix()` returns `Some(b"svg")`
/// and `local_name()` returns `b"rect"`.
///
/// `QName` implements `Deref<Target = [u8]>`, so it can be used anywhere
/// a `&[u8]` is expected (e.g. comparison, hashing, printing).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct QName<'a> {
    raw: &'a [u8],
    colon_pos: Option<u16>,
    span: Span,
}

impl<'a> QName<'a> {
    /// Construct a `QName` from a raw name slice, optional colon position,
    /// and byte span in the input stream.
    #[inline]
    pub fn new(raw: &'a [u8], colon_pos: Option<u16>, span: Span) -> Self {
        Self { raw, colon_pos, span }
    }

    /// The full raw name as it appears in the source (e.g. `b"svg:rect"` or `b"div"`).
    #[inline]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.raw
    }

    /// The byte span of the full name in the input stream.
    #[inline]
    pub fn span(&self) -> Span {
        self.span
    }

    /// The namespace prefix, if present.
    ///
    /// Returns `Some(b"svg")` for `svg:rect`, `None` for `div`.
    #[inline]
    pub fn prefix(&self) -> Option<&'a [u8]> {
        self.colon_pos.map(|pos| &self.raw[..pos as usize])
    }

    /// The byte span of the prefix in the input stream, if present.
    #[inline]
    pub fn prefix_span(&self) -> Option<Span> {
        self.colon_pos.map(|pos| Span::new(self.span.start, self.span.start + pos as u64))
    }

    /// The local name after the prefix and colon.
    ///
    /// Returns `b"rect"` for `svg:rect`, `b"div"` for `div`.
    #[inline]
    pub fn local_name(&self) -> &'a [u8] {
        match self.colon_pos {
            Some(pos) => &self.raw[pos as usize + 1..],
            None => self.raw,
        }
    }

    /// The byte span of the local name in the input stream.
    ///
    /// For prefixed names, this starts after the colon.
    /// For unprefixed names, this is the same as [`span()`](Self::span).
    #[inline]
    pub fn local_name_span(&self) -> Span {
        match self.colon_pos {
            Some(pos) => Span::new(self.span.start + pos as u64 + 1, self.span.end),
            None => self.span,
        }
    }
}

impl<'a> core::ops::Deref for QName<'a> {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        self.raw
    }
}

impl core::fmt::Debug for QName<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "QName(\"{}\")", self.raw.escape_ascii())
    }
}

/// Absolute byte range in the input stream.
/// `start` is inclusive, `end` is exclusive: `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

impl Span {
    #[inline]
    pub fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    #[inline]
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Error from the XML syntax reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// Absolute byte offset in the stream where the error occurred.
    pub offset: u64,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} at byte offset {}", self.kind, self.offset)
    }
}

impl core::error::Error for Error {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// Expected an element or attribute name, but found a byte that is not a
    /// valid name-start character.
    ExpectedName(u8),
    /// Expected `=` between attribute name and value.
    ExpectedEquals(u8),
    /// Expected a quote character (`"` or `'`) to open an attribute value.
    ExpectedQuote(u8),
    /// Expected `>` to close a tag (e.g. after `/>`, end-tag name, or `?` in PI).
    ExpectedClose(u8),
    /// Expected a specific keyword character (e.g. in `DOCTYPE`, `CDATA`,
    /// `NDATA`, `SYSTEM`, `PUBLIC`, `FIXED`, `<!--`, etc.) but found another byte.
    ExpectedKeyword(u8),
    /// Unexpected end of input within a construct.
    UnexpectedEof,
    /// Invalid character reference.
    InvalidCharRef,
    /// Double-hyphen (`--`) in comment body (XML 1.0 §2.5).
    DoubleDashInComment,
    /// Missing whitespace after `DOCTYPE` keyword.
    DoctypeMissingWhitespace,
    /// Missing or invalid name in `DOCTYPE` declaration.
    DoctypeMissingName,
    /// `]]>` appeared in text content (not allowed in well-formed XML).
    CdataEndInContent,
    /// Invalid UTF-8 byte sequence.
    InvalidUtf8,
    /// Name exceeded the 1000-byte limit.
    NameTooLong,
    /// Character reference value exceeded the 7-byte limit.
    CharRefTooLong,
    /// Malformed XML declaration (missing version, bad syntax, invalid standalone value).
    MalformedXmlDeclaration,
    /// PI target matching `[Xx][Mm][Ll]` appeared after the document start.
    ReservedPITarget,
    /// Unrecognized `<!` construct inside DTD internal subset.
    DtdInvalidMarkup,
    /// Missing whitespace after DTD declaration keyword.
    DtdDeclMissingWhitespace,
    /// Missing or invalid name in DTD declaration.
    DtdDeclMissingName,
    /// System or public literal exceeded the 8192-byte limit.
    LiteralTooLong,
    /// Parenthesis nesting in element content model or enumerated type exceeded depth limit.
    DtdParensTooDeep,
}

impl core::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ErrorKind::ExpectedName(b) => write!(f, "expected name, found 0x{b:02X}"),
            ErrorKind::ExpectedEquals(b) => write!(f, "expected '=', found 0x{b:02X}"),
            ErrorKind::ExpectedQuote(b) => write!(f, "expected quote, found 0x{b:02X}"),
            ErrorKind::ExpectedClose(b) => write!(f, "expected '>', found 0x{b:02X}"),
            ErrorKind::ExpectedKeyword(b) => write!(f, "expected keyword, found 0x{b:02X}"),
            ErrorKind::UnexpectedEof => write!(f, "unexpected end of input"),
            ErrorKind::InvalidCharRef => write!(f, "invalid character reference"),
            ErrorKind::DoubleDashInComment => write!(f, "double-hyphen (--) in comment body"),
            ErrorKind::DoctypeMissingWhitespace => {
                write!(f, "missing whitespace after DOCTYPE keyword")
            }
            ErrorKind::DoctypeMissingName => {
                write!(f, "missing or invalid name in DOCTYPE declaration")
            }
            ErrorKind::CdataEndInContent => write!(f, "]]> in text content"),
            ErrorKind::InvalidUtf8 => write!(f, "invalid UTF-8"),
            ErrorKind::NameTooLong => write!(f, "name exceeds 1000-byte limit"),
            ErrorKind::CharRefTooLong => write!(f, "character reference exceeds 7-byte limit"),
            ErrorKind::MalformedXmlDeclaration => {
                write!(f, "malformed XML declaration")
            }
            ErrorKind::ReservedPITarget => {
                write!(f, "reserved PI target (xml) after document start")
            }
            ErrorKind::DtdInvalidMarkup => {
                write!(f, "unrecognized markup in DTD internal subset")
            }
            ErrorKind::DtdDeclMissingWhitespace => {
                write!(f, "missing whitespace after DTD declaration keyword")
            }
            ErrorKind::DtdDeclMissingName => {
                write!(f, "missing or invalid name in DTD declaration")
            }
            ErrorKind::LiteralTooLong => {
                write!(f, "system or public literal exceeds 8192-byte limit")
            }
            ErrorKind::DtdParensTooDeep => {
                write!(f, "parenthesis nesting exceeds depth limit in DTD")
            }
        }
    }
}

/// Result of `Reader::parse()`.
pub enum ParseError<E> {
    /// XML syntax error.
    Xml(Error),
    /// Error returned by a `Visitor` callback.
    Visitor(E),
}

impl<E: core::fmt::Debug> core::fmt::Debug for ParseError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ParseError::Xml(e) => write!(f, "ParseError::Xml({e:?})"),
            ParseError::Visitor(e) => write!(f, "ParseError::Visitor({e:?})"),
        }
    }
}

impl<E: core::fmt::Display> core::fmt::Display for ParseError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ParseError::Xml(e) => write!(f, "XML error: {e}"),
            ParseError::Visitor(e) => write!(f, "visitor error: {e}"),
        }
    }
}

impl<E: core::error::Error> core::error::Error for ParseError<E> {}

impl<E> From<Error> for ParseError<E> {
    fn from(e: Error) -> Self {
        ParseError::Xml(e)
    }
}

/// Encoding detected by `probe_encoding()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
    /// Encoding declared in the XML declaration but not detectable from BOM.
    Declared(DeclaredEncoding),
    /// Could not determine encoding (e.g. empty or insufficient data).
    Unknown,
}

/// Check if a byte is XML whitespace (SP, TAB, LF, CR).
#[inline]
pub(crate) fn is_xml_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Encoding name extracted from the XML declaration, stored inline to avoid allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclaredEncoding {
    buf: [u8; 40],
    len: u8,
}

impl DeclaredEncoding {
    pub fn new(name: &[u8]) -> Option<Self> {
        if name.len() > 40 {
            return None;
        }
        let mut buf = [0u8; 40];
        buf[..name.len()].copy_from_slice(name);
        Some(Self {
            buf,
            len: name.len() as u8,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(self.as_bytes()).ok()
    }
}
