/// Which quote character delimited the attribute value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteStyle {
    Double,
    Single,
}

/// Sub-state within DOCTYPE content scanning.
///
/// Tracks whether the scanner is inside a comment, PI, or quoted string
/// so that `[`, `]`, and `>` inside those constructs are not misinterpreted
/// as structural delimiters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctypeSubState {
    /// Normal scanning - `[`, `]`, `>` are structural.
    Normal,
    /// Saw `<` - checking for `!` (comment) or `?` (PI).
    AfterLt,
    /// Saw `<!` - checking for `-`.
    AfterLtBang,
    /// Saw `<!-` - expecting `-` to enter comment.
    AfterLtBangDash,
    /// Inside `<!-- ... -->`, tracking consecutive dashes for exit.
    Comment { dash_count: u8 },
    /// Inside `<? ... ?>`, tracking whether last byte was `?`.
    PI { saw_qmark: bool },
    /// Inside a double-quoted string.
    DoubleQuoted,
    /// Inside a single-quoted string.
    SingleQuoted,
}

/// Which declaration context an ExternalID appears in (DTD feature).
#[cfg(feature = "dtd")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtdDeclContext {
    Entity { kind: crate::types::EntityKind },
    Notation,
}

/// DTD declaration kind being matched.
#[cfg(feature = "dtd")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtdDeclKind {
    Element,
    Attlist,
    Entity,
    Notation,
}

/// Phase within the DTD internal subset tokenizer.
///
/// Stored as a field on `Reader` (not inside `ParserState`) to keep the
/// `ParserState` enum small. Only used when `feature = "dtd"`.
#[cfg(feature = "dtd")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtdPhase {
    /// Between declarations. Scanning for `<`, `%`, `]`, or whitespace.
    Idle,

    // --- Dispatch ---
    /// Saw `<` in internal subset.
    AfterLt,
    /// Saw `<!` — determining declaration type.
    AfterLtBang,
    /// Saw `<!-` — expecting second `-` for comment.
    AfterLtBangDash,
    /// Comment inside internal subset.
    Comment { dash_count: u8 },
    /// Saw `<?` — reading PI target name.
    PITarget { name_start: usize },
    /// PI content inside internal subset.
    PIContent { saw_qmark: bool },

    // --- Keyword matching after `<!` ---
    /// Matching keyword: `ELEMENT`, `ATTLIST`, `ENTITY`, `NOTATION`.
    /// `matched` is how many bytes of the keyword have matched so far.
    MatchKeyword { kind: DtdDeclKind, matched: u8 },

    // --- ELEMENT declaration ---
    ElementRequireWs,
    ElementBeforeName,
    ElementName { name_start: usize },
    ElementAfterName,
    ElementContentSpecKeyword { matched: u8 },
    ElementContentModel { paren_depth: u32 },
    ElementAfterContentSpec,

    // --- ATTLIST declaration ---
    AttlistRequireWs,
    AttlistBeforeName,
    AttlistName { name_start: usize },
    /// Between attributes or before `>`.
    AttlistIdle,
    AttlistAttrName { name_start: usize },
    AttlistBeforeType,
    /// Scanning type keyword or `(` for enumeration.
    AttlistTypeStart,
    /// Inside type keyword text.
    AttlistTypeKeyword { start: usize },
    /// Inside parenthesized type enumeration.
    AttlistTypeEnum { paren_depth: u32 },
    /// After `NOTATION` keyword, before `(`.
    AttlistTypeNotationBeforeParen,
    AttlistBeforeDefault,
    /// Matching `#REQUIRED`, `#IMPLIED`, or `#FIXED`.
    AttlistDefaultHash { start: usize },
    /// After `#FIXED`, before whitespace/quote.
    AttlistFixedBeforeValue,
    /// Inside default attribute value.
    AttlistDefaultValue { quote: QuoteStyle },
    /// Entity ref inside default value.
    AttlistDefaultEntityRef { name_start: usize, quote: QuoteStyle },
    /// `&#` in default value.
    AttlistDefaultCharRef { value_start: usize, quote: QuoteStyle },

    // --- ENTITY declaration ---
    EntityRequireWs,
    /// After whitespace — checking for `%`.
    EntityCheckPercent,
    /// After `%`, requiring whitespace.
    EntityPercentRequireWs,
    EntityBeforeName { kind: crate::types::EntityKind },
    EntityName { name_start: usize, kind: crate::types::EntityKind },
    /// After entity name, before definition (need whitespace first).
    EntityBeforeDef { kind: crate::types::EntityKind },
    /// After whitespace, determine entity value vs external ID.
    EntityDefStart { kind: crate::types::EntityKind },
    /// Inside entity value.
    EntityValue { quote: QuoteStyle },
    EntityValueEntityRef { name_start: usize, quote: QuoteStyle },
    EntityValueCharRef { value_start: usize, quote: QuoteStyle },
    EntityValuePeRef { name_start: usize, quote: QuoteStyle },
    /// After external ID, checking for NDATA or `>`.
    EntityAfterExternalId { kind: crate::types::EntityKind },
    /// Matching NDATA keyword.
    EntityNdataKeyword { matched: u8 },
    EntityNdataRequireWs,
    EntityNdataName { name_start: usize },
    /// After entity definition, expecting `>`.
    EntityBeforeClose,

    // --- NOTATION declaration ---
    NotationRequireWs,
    NotationBeforeName,
    NotationName { name_start: usize },
    /// After name, before SYSTEM/PUBLIC.
    NotationBeforeDef,
    /// After external ID, before `>`.
    NotationBeforeClose,

    // --- Shared External ID scanning ---
    /// Matching `SYSTEM` keyword.
    ExternalIdSystemKw { ctx: DtdDeclContext, matched: u8 },
    /// Matching `PUBLIC` keyword.
    ExternalIdPublicKw { ctx: DtdDeclContext, matched: u8 },
    /// Before system literal quote.
    ExternalIdBeforeSystemLit { ctx: DtdDeclContext },
    /// Inside system literal.
    ExternalIdSystemLit { ctx: DtdDeclContext, quote: QuoteStyle, literal_start: usize },
    /// Before public literal quote.
    ExternalIdBeforePublicLit { ctx: DtdDeclContext },
    /// Inside public literal.
    ExternalIdPublicLit { ctx: DtdDeclContext, quote: QuoteStyle, literal_start: usize },
    /// Between public and system literals.
    ExternalIdBetweenLiterals { ctx: DtdDeclContext },

    // --- PE reference at internal subset level ---
    PeRefName { name_start: usize },
}

#[cfg(feature = "dtd")]
impl DtdPhase {
    /// Adjust all buffer-relative positions after the caller shifts the buffer.
    pub fn adjust_positions(&mut self, consumed: usize) {
        match self {
            DtdPhase::PITarget { name_start }
            | DtdPhase::ElementName { name_start }
            | DtdPhase::AttlistName { name_start }
            | DtdPhase::AttlistAttrName { name_start }
            | DtdPhase::AttlistDefaultEntityRef { name_start, .. }
            | DtdPhase::EntityName { name_start, .. }
            | DtdPhase::EntityValueEntityRef { name_start, .. }
            | DtdPhase::EntityValuePeRef { name_start, .. }
            | DtdPhase::NotationName { name_start }
            | DtdPhase::PeRefName { name_start } => *name_start -= consumed,

            // EntityNdataName uses usize::MAX as sentinel for "skipping whitespace"
            DtdPhase::EntityNdataName { name_start } if *name_start < usize::MAX => {
                *name_start -= consumed;
            }

            DtdPhase::AttlistDefaultCharRef { value_start, .. }
            | DtdPhase::EntityValueCharRef { value_start, .. } => *value_start -= consumed,

            DtdPhase::ExternalIdSystemLit { literal_start, .. }
            | DtdPhase::ExternalIdPublicLit { literal_start, .. } => *literal_start -= consumed,

            DtdPhase::AttlistTypeKeyword { start }
            | DtdPhase::AttlistDefaultHash { start } => *start -= consumed,

            // All other variants have no buffer-relative positions.
            _ => {}
        }
    }
}

/// Parser state: tracks which XML construct we are currently inside.
///
/// The `usize` fields store buffer-relative positions (not absolute stream offsets).
/// They are converted to absolute offsets when emitting events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserState {
    /// Between markup. Scanning for `<` or `&` in text content.
    Content,

    /// Saw `<`, determining what kind of markup follows.
    AfterLt,

    /// Inside a start tag, reading the tag name.
    /// `name_start` is the buffer position of the first name character.
    StartTagName { name_start: usize },

    /// After the tag name in a start tag, before attributes or `>`.
    StartTagPostName,

    /// Reading an attribute name.
    /// `name_start` is the buffer position of the first name character.
    AttrName { name_start: usize },

    /// After attribute name, expecting `=`.
    AfterAttrName,

    /// After `=`, expecting a quote character.
    BeforeAttrValue,

    /// Inside a quoted attribute value.
    /// Position tracking uses `content_start` on Reader.
    /// `quote` records which delimiter character opened the value.
    AttrValue { quote: QuoteStyle },

    /// Entity reference inside an attribute value: after `&`, scanning for `;`.
    /// `name_start` is the buffer position of the first character after `&`.
    /// `quote` records which AttrValue state to return to after `;`.
    AttrEntityRef { name_start: usize, quote: QuoteStyle },

    /// Character reference inside an attribute value: after `&#`, scanning for `;`.
    /// `value_start` is the buffer position of the first character after `&#`.
    /// `quote` records which AttrValue state to return to after `;`.
    AttrCharRef { value_start: usize, quote: QuoteStyle },

    /// Inside `</`, reading the end tag name.
    /// `name_start` is the buffer position of the first name character.
    EndTagName { name_start: usize },

    /// After end tag name, expecting `>`.
    EndTagPostName,

    /// Saw `/` in a start tag, expecting `>` to complete `/>`.
    /// Used when `/` appears at the end of a buffer and `>` is in the next chunk.
    StartTagGotSlash,

    // --- Phase 2 states (comments, PIs, CDATA, DOCTYPE, references) ---

    /// After `<!`, need to determine comment, CDATA, or DOCTYPE.
    AfterLtBang,

    /// After `<!-`, expecting second `-` to start a comment.
    AfterLtBangDash,

    /// Comment content: scanning for `-->`.
    /// `dash_count` tracks consecutive dashes at the scanning position.
    CommentContent { dash_count: u8 },

    /// After `<![`, matching `CDATA[`.
    /// `matched` is how many characters of "CDATA[" have been matched so far.
    AfterLtBangBracket { matched: u8 },

    /// CDATA content: scanning for `]]>`.
    /// `bracket_count` tracks consecutive `]` at the scanning position.
    CdataContent { bracket_count: u8 },

    /// After `<!D`, matching `OCTYPE`.
    /// `matched` is how many characters of "OCTYPE" have been matched.
    AfterLtBangD { matched: u8 },

    /// After `<!DOCTYPE `, reading the root element name.
    /// `name_start` is the buffer position of the first name character.
    DoctypeName { name_start: usize },

    /// DOCTYPE content: scanning for `>` with balanced `[`/`]`.
    /// `depth` tracks bracket nesting depth.
    /// `sub` tracks comment/PI/quote context to avoid misinterpreting delimiters.
    DoctypeContent { depth: u32, sub: DoctypeSubState },

    // --- DTD-specific ParserState variants (feature = "dtd") ---

    /// DTD internal subset tokenization. Actual phase in `Reader::dtd_phase`.
    #[cfg(feature = "dtd")]
    DtdInternalSubset,

    /// After `]` closing the internal subset, expecting optional whitespace then `>`.
    #[cfg(feature = "dtd")]
    DoctypeAfterSubset,

    /// Processing instruction: reading target name after `<?`.
    /// `name_start` is the buffer position of the first name character.
    PITarget { name_start: usize },

    /// Processing instruction content: scanning for `?>`.
    /// `saw_qmark` is true if the last character seen was `?`.
    PIContent { saw_qmark: bool },

    /// Entity reference in text content: after `&`, scanning for `;`.
    /// `name_start` is the buffer position of the first character after `&`.
    EntityRef { name_start: usize },

    /// Character reference: after `&#`, scanning for `;`.
    /// `value_start` is the buffer position of the first character after `&#` or `&#x`.
    CharRef { value_start: usize },
}

impl ParserState {
    /// Adjust all buffer-relative positions after the caller shifts the buffer
    /// by `consumed` bytes. Only states with `usize` position fields need updating.
    pub fn adjust_positions(&mut self, consumed: usize) {
        match self {
            ParserState::StartTagName { name_start } => *name_start -= consumed,
            ParserState::AttrName { name_start } => *name_start -= consumed,
            ParserState::AttrEntityRef { name_start, .. } => *name_start -= consumed,
            ParserState::AttrCharRef { value_start, .. } => *value_start -= consumed,
            ParserState::EndTagName { name_start } => *name_start -= consumed,
            ParserState::DoctypeName { name_start } if *name_start < usize::MAX - 1 => *name_start -= consumed,
            ParserState::PITarget { name_start } => *name_start -= consumed,
            ParserState::EntityRef { name_start } => *name_start -= consumed,
            ParserState::CharRef { value_start } => *value_start -= consumed,
            // States without buffer-relative positions:
            ParserState::Content
            | ParserState::AfterLt
            | ParserState::StartTagPostName
            | ParserState::StartTagGotSlash
            | ParserState::AfterAttrName
            | ParserState::BeforeAttrValue
            | ParserState::AttrValue { .. }
            | ParserState::EndTagPostName
            | ParserState::AfterLtBang
            | ParserState::AfterLtBangDash
            | ParserState::CommentContent { .. }
            | ParserState::AfterLtBangBracket { .. }
            | ParserState::CdataContent { .. }
            | ParserState::AfterLtBangD { .. }
            | ParserState::DoctypeName { .. }
            | ParserState::DoctypeContent { .. }
            | ParserState::PIContent { .. } => {}
            #[cfg(feature = "dtd")]
            ParserState::DtdInternalSubset
            | ParserState::DoctypeAfterSubset => {}
        }
    }
}
