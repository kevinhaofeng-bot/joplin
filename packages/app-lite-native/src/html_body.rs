use html5ever::tendril::ByteTendril;
use html5ever::tokenizer::{
    BufferQueue, CharacterTokens, EndTag, NullCharacterToken, StartTag, TagToken, Token, TokenSink,
    TokenSinkResult, Tokenizer, TokenizerOpts,
};
use std::cell::RefCell;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HtmlBodyError {
    #[error("HTML parser failed")]
    Parse,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Document {
    pub fn from_blocks(blocks: Vec<Block>) -> Self {
        Self { blocks }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text { text: String, marks: Marks },
    SoftBreak,
    Image { resource_id: String, alt: String },
}

pub fn parse_html(input: &str) -> Result<Document, HtmlBodyError> {
    let sink = HtmlSink::default();
    let queue = BufferQueue::default();
    queue.push_back(
        ByteTendril::from_slice(input.as_bytes())
            .try_reinterpret()
            .map_err(|_| HtmlBodyError::Parse)?,
    );
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    Ok(tokenizer.sink.finish())
}

pub fn serialize_html(document: &Document) -> String {
    if document.blocks.iter().all(block_is_empty) {
        return String::new();
    }
    let mut output = String::new();
    for block in &document.blocks {
        match block {
            Block::Paragraph(inlines) => {
                output.push_str("<p>");
                serialize_inlines(inlines, &mut output);
                output.push_str("</p>");
            }
        }
    }
    output
}

pub fn search_text(document: &Document) -> String {
    let mut output = String::new();
    for (index, block) in document.blocks.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        match block {
            Block::Paragraph(inlines) => {
                for inline in inlines {
                    match inline {
                        Inline::Text { text, .. } => output.push_str(text),
                        Inline::SoftBreak => output.push('\n'),
                        Inline::Image { alt, .. } => output.push_str(alt),
                    }
                }
            }
        }
    }
    output
}

pub fn resource_ids(document: &Document) -> Vec<String> {
    document
        .blocks
        .iter()
        .flat_map(|block| match block {
            Block::Paragraph(inlines) => inlines
                .iter()
                .filter_map(|inline| match inline {
                    Inline::Image { resource_id, .. }
                        if crate::body::validate_resource_id(resource_id).is_ok() =>
                    {
                        Some(resource_id.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
        })
        .collect()
}

fn block_is_empty(block: &Block) -> bool {
    match block {
        Block::Paragraph(inlines) => inlines.iter().all(|inline| match inline {
            Inline::Text { text, .. } => text.is_empty(),
            Inline::SoftBreak => false,
            Inline::Image { .. } => false,
        }),
    }
}

fn serialize_inlines(inlines: &[Inline], output: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text { text, marks } => serialize_text(text, marks, output),
            Inline::SoftBreak => output.push_str("<br>"),
            Inline::Image { resource_id, alt } => {
                if crate::body::validate_resource_id(resource_id).is_ok() {
                    output.push_str("<img src=\":/");
                    escape_attribute(resource_id, output);
                    output.push_str("\" alt=\"");
                    escape_attribute(alt, output);
                    output.push_str("\">");
                } else {
                    escape_text(alt, output);
                }
            }
        }
    }
}

fn serialize_text(text: &str, marks: &Marks, output: &mut String) {
    if marks.bold {
        output.push_str("<strong>");
    }
    if marks.italic {
        output.push_str("<em>");
    }
    if marks.underline {
        output.push_str("<u>");
    }
    escape_text(text, output);
    if marks.underline {
        output.push_str("</u>");
    }
    if marks.italic {
        output.push_str("</em>");
    }
    if marks.bold {
        output.push_str("</strong>");
    }
}

fn escape_text(text: &str, output: &mut String) {
    escape(text, output);
}

fn escape_attribute(text: &str, output: &mut String) {
    escape(text, output);
}

fn escape(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
}

#[derive(Default)]
struct HtmlSink {
    state: RefCell<ParserState>,
}

#[derive(Default)]
struct ParserState {
    document: Document,
    current: Option<Vec<Inline>>,
    block_stack: Vec<String>,
    mark_stack: Vec<String>,
    skip_tag: Option<String>,
}

impl HtmlSink {
    fn finish(self) -> Document {
        let mut state = self.state.into_inner();
        state.flush_current();
        state.document
    }
}

impl ParserState {
    fn process_tag(&mut self, tag: html5ever::tokenizer::Tag) {
        let name = tag.name.to_string().to_ascii_lowercase();
        match tag.kind {
            StartTag => self.start_tag(&name, &tag.attrs),
            EndTag => self.end_tag(&name),
        }
    }

    fn start_tag(&mut self, name: &str, attrs: &[html5ever::Attribute]) {
        if let Some(skip_tag) = self.skip_tag.as_mut() {
            if skip_tag == name {
                return;
            }
            return;
        }
        if matches!(name, "script" | "style") {
            self.skip_tag = Some(name.to_owned());
            return;
        }
        if is_leaf_block(name) {
            if self
                .current
                .as_ref()
                .is_some_and(|inlines| !inlines.is_empty())
            {
                self.flush_current();
            }
            self.current = Some(Vec::new());
            self.block_stack.push(name.to_owned());
            return;
        }
        if is_container_block(name) {
            if self
                .current
                .as_ref()
                .is_some_and(|inlines| !inlines.is_empty())
            {
                self.flush_current();
            }
            self.block_stack.push(name.to_owned());
            return;
        }
        match name {
            "strong" | "b" | "em" | "i" | "u" => self.mark_stack.push(name.to_owned()),
            "br" => {
                self.ensure_current();
                self.current.as_mut().unwrap().push(Inline::SoftBreak);
            }
            "img" => self.add_image(attrs),
            _ => {}
        }
    }

    fn end_tag(&mut self, name: &str) {
        if self.skip_tag.as_deref() == Some(name) {
            self.skip_tag = None;
            return;
        }
        if matches!(name, "strong" | "b" | "em" | "i" | "u") {
            if let Some(index) = self.mark_stack.iter().rposition(|tag| tag == name) {
                self.mark_stack.remove(index);
            }
            return;
        }
        if is_leaf_block(name) {
            self.flush_current();
            if let Some(index) = self.block_stack.iter().rposition(|tag| tag == name) {
                self.block_stack.remove(index);
            }
        } else if is_container_block(name)
            && let Some(index) = self.block_stack.iter().rposition(|tag| tag == name)
        {
            self.block_stack.remove(index);
        }
    }

    fn add_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.current.is_none() && text.chars().all(char::is_whitespace) {
            return;
        }
        self.ensure_current();
        let marks = self.current_marks();
        let inlines = self.current.as_mut().unwrap();
        if let Some(Inline::Text {
            text: previous,
            marks: previous_marks,
        }) = inlines.last_mut()
            && *previous_marks == marks
        {
            previous.push_str(text);
            return;
        }
        inlines.push(Inline::Text {
            text: text.to_owned(),
            marks,
        });
    }

    fn add_image(&mut self, attrs: &[html5ever::Attribute]) {
        let source = attribute(attrs, "src");
        let alt = attribute(attrs, "alt").unwrap_or_default();
        let Some(source) = source else {
            self.add_text(&alt);
            return;
        };
        let Some(resource_id) = source.strip_prefix(":/") else {
            self.add_text(&alt);
            return;
        };
        if crate::body::validate_resource_id(resource_id).is_err() {
            self.add_text(&alt);
            return;
        }
        self.ensure_current();
        self.current.as_mut().unwrap().push(Inline::Image {
            resource_id: resource_id.to_owned(),
            alt,
        });
    }

    fn ensure_current(&mut self) {
        if self.current.is_none() {
            self.current = Some(Vec::new());
        }
    }

    fn current_marks(&self) -> Marks {
        Marks {
            bold: self
                .mark_stack
                .iter()
                .any(|tag| matches!(tag.as_str(), "strong" | "b")),
            italic: self
                .mark_stack
                .iter()
                .any(|tag| matches!(tag.as_str(), "em" | "i")),
            underline: self.mark_stack.iter().any(|tag| tag == "u"),
        }
    }

    fn flush_current(&mut self) {
        if let Some(inlines) = self.current.take() {
            self.document.blocks.push(Block::Paragraph(inlines));
        }
    }
}

impl TokenSink for HtmlSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<Self::Handle> {
        let mut state = self.state.borrow_mut();
        match token {
            CharacterTokens(text) if state.skip_tag.is_none() => state.add_text(&text),
            NullCharacterToken if state.skip_tag.is_none() => state.add_text("\u{fffd}"),
            TagToken(tag) => state.process_tag(tag),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn attribute(attrs: &[html5ever::Attribute], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|attribute| attribute.name.local.to_string().eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value.to_string())
}

fn is_leaf_block(name: &str) -> bool {
    matches!(name, "p" | "h1" | "h2" | "h3" | "li" | "pre")
}

fn is_container_block(name: &str) -> bool {
    matches!(name, "ul" | "ol" | "blockquote")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESOURCE_ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn escapes_unicode_text_and_preserves_significant_whitespace() {
        let document = Document::from_blocks(vec![Block::Paragraph(vec![Inline::Text {
            text: "  中文 😀 & < > \" '  ".into(),
            marks: Marks::default(),
        }])]);

        let html = serialize_html(&document);
        assert_eq!(html, "<p>  中文 😀 &amp; &lt; &gt; &quot; &#39;  </p>");
        assert_eq!(parse_html(&html).unwrap(), document);
    }

    #[test]
    fn paragraphs_marks_breaks_and_images_round_trip_in_order() {
        let document = Document::from_blocks(vec![
            Block::Paragraph(vec![
                Inline::Text {
                    text: "前".into(),
                    marks: Marks {
                        bold: true,
                        italic: false,
                        underline: false,
                    },
                },
                Inline::SoftBreak,
                Inline::Image {
                    resource_id: RESOURCE_ID.into(),
                    alt: "截图 & 证据.png".into(),
                },
                Inline::Text {
                    text: "后".into(),
                    marks: Marks {
                        bold: false,
                        italic: true,
                        underline: true,
                    },
                },
            ]),
            Block::Paragraph(Vec::new()),
        ]);

        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<p><strong>前</strong><br><img src=\":/0123456789abcdef0123456789abcdef\" alt=\"截图 &amp; 证据.png\"><em><u>后</u></em></p><p></p>"
        );
        assert_eq!(parse_html(&html).unwrap(), document);
    }

    #[test]
    fn identical_documents_have_identical_html_bytes() {
        let document = Document::from_blocks(vec![Block::Paragraph(vec![Inline::Text {
            text: "稳定输出".into(),
            marks: Marks {
                bold: true,
                italic: true,
                underline: true,
            },
        }])]);

        assert_eq!(serialize_html(&document), serialize_html(&document));
        assert_eq!(
            serialize_html(&parse_html(&serialize_html(&document)).unwrap()),
            serialize_html(&document)
        );
    }

    #[test]
    fn only_local_valid_image_ids_survive_as_resource_nodes() {
        let html = format!(
            "<p><img src=\":/{RESOURCE_ID}\" alt=\"ok\"><img src=\"data:image/png;base64,AAAA\" alt=\"data\"><img src=\"https://example.com/a.png\" alt=\"remote\"><img src=\":/NOT-VALID\" alt=\"invalid\"></p>"
        );

        let document = parse_html(&html).unwrap();
        assert_eq!(resource_ids(&document), vec![RESOURCE_ID]);
        assert_eq!(
            document,
            Document::from_blocks(vec![Block::Paragraph(vec![
                Inline::Image {
                    resource_id: RESOURCE_ID.into(),
                    alt: "ok".into(),
                },
                Inline::Text {
                    text: "dataremoteinvalid".into(),
                    marks: Marks::default(),
                },
            ])])
        );
    }

    #[test]
    fn unsafe_elements_attributes_and_urls_are_downgraded_to_visible_text() {
        let document = parse_html(
            r#"<p onclick="alert(1)"><script>secret()</script><style>.x{display:none}</style><mark>visible</mark><a href="javascript:alert(1)">link</a></p>"#,
        )
        .unwrap();

        assert_eq!(search_text(&document), "visiblelink");
        assert!(!serialize_html(&document).contains("script"));
        assert!(!serialize_html(&document).contains("onclick"));
        assert!(!serialize_html(&document).contains("javascript:"));
    }

    #[test]
    fn search_projection_contains_visible_text_and_image_alt_but_not_markup() {
        let document = parse_html(&format!(
            "<p>正文 <strong>重点</strong><img src=\":/{RESOURCE_ID}\" alt=\"付款截图.png\"></p>"
        ))
        .unwrap();

        assert_eq!(search_text(&document), "正文 重点付款截图.png");
        assert!(!search_text(&document).contains(RESOURCE_ID));
        assert!(!search_text(&document).contains("strong"));
    }

    #[test]
    fn empty_semantic_content_serializes_to_empty_html() {
        assert_eq!(serialize_html(&Document::default()), "");
        assert_eq!(serialize_html(&Document::from_blocks(vec![])), "");
        assert_eq!(
            serialize_html(&Document::from_blocks(vec![Block::Paragraph(vec![])])),
            ""
        );
    }
}
