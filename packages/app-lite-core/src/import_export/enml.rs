//! Pure, fail-closed ENML conversion into the existing canonical document model.
//! This module never reads a profile or writes a resource.

use crate::{
    document::{CanonicalDocument, CanonicalHtml, MAX_RETAINED_LINK_BYTES, SearchText, valid_link},
    resource::ResourceId,
};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use std::collections::BTreeMap;
use thiserror::Error;

const MAX_ENML_BYTES: usize = 4 * 1024 * 1024;
const MAX_NODES: usize = 50_000;
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEnmlResource {
    pub resource_id: ResourceId,
    pub mime: String,
    pub filename: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("ENML fidelity blocker at {path}: {reason}")]
pub struct EnmlFidelityBlocker {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnmlConversion {
    pub document: CanonicalDocument,
    pub html: CanonicalHtml,
    pub search_text: SearchText,
    pub resource_ids: Vec<ResourceId>,
}

fn blocked(path: &str, reason: impl Into<String>) -> EnmlFidelityBlocker {
    EnmlFidelityBlocker {
        path: path.into(),
        reason: reason.into(),
    }
}

#[derive(Debug)]
enum Child {
    Text(String),
    Element(Element),
}
#[derive(Debug)]
struct Element {
    name: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Child>,
}

/// The resource map must contain only verified occurrences from this note.
/// A hash with multiple candidate destinations is intentionally ambiguous.
pub fn convert_enml(
    enml: &str,
    same_note_resources: &BTreeMap<String, Vec<VerifiedEnmlResource>>,
) -> Result<EnmlConversion, EnmlFidelityBlocker> {
    let root = parse_enml(enml)?;
    let mut html = String::new();
    let mut context = RenderContext {
        resources: same_note_resources,
        conservative_link_bytes: 0,
    };
    let mut inline_run = false;
    for (index, child) in root.children.iter().enumerate() {
        let path = format!("/en-note/{index}");
        let root_inline = match child {
            Child::Text(text) => inline_run || !is_xml_formatting_whitespace(text),
            Child::Element(element) => {
                matches!(
                    element.name.as_str(),
                    "b" | "strong"
                        | "i"
                        | "em"
                        | "u"
                        | "s"
                        | "strike"
                        | "del"
                        | "mark"
                        | "span"
                        | "a"
                ) || (element.name == "en-media"
                    && (inline_run || next_root_flow_child(&root.children, index)))
                    || (element.name == "br"
                        && (inline_run || next_root_flow_child(&root.children, index)))
            }
        };
        if root_inline {
            if !inline_run {
                html.push_str("<p>");
                inline_run = true;
            }
            context.inline(child, &path, &mut html)?;
        } else {
            if inline_run {
                html.push_str("</p>");
                inline_run = false;
            }
            context.block(child, &path, &mut html)?;
        }
    }
    if inline_run {
        html.push_str("</p>");
    }
    let document = CanonicalDocument::parse_html(&html)
        .map_err(|e| blocked("/en-note", format!("canonical parser: {e}")))?;
    let canonical = document.to_canonical_html();
    let search_text = document.search_text();
    let resource_ids = document.resource_ids();
    Ok(EnmlConversion {
        document,
        html: canonical,
        search_text,
        resource_ids,
    })
}

fn root_flow_child(child: &Child) -> bool {
    match child {
        Child::Text(text) => !is_xml_formatting_whitespace(text),
        Child::Element(element) => matches!(
            element.name.as_str(),
            "b" | "strong"
                | "i"
                | "em"
                | "u"
                | "s"
                | "strike"
                | "del"
                | "mark"
                | "span"
                | "a"
                | "br"
        ),
    }
}

fn next_root_flow_child(children: &[Child], index: usize) -> bool {
    children
        .iter()
        .skip(index + 1)
        .find(|child| !matches!(child, Child::Text(text) if is_xml_formatting_whitespace(text)))
        .is_some_and(root_flow_child)
}

fn parse_enml(input: &str) -> Result<Element, EnmlFidelityBlocker> {
    if input.len() > MAX_ENML_BYTES {
        return Err(blocked("/", "ENML body exceeds 4 MiB"));
    }
    let mut reader = Reader::from_reader(input.as_bytes());
    let mut buffer = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;
    let mut nodes = 0usize;
    let mut declaration_seen = false;
    let mut doctype_seen = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let element = element_from_start(&event)?;
                nodes += 1;
                if nodes > MAX_NODES {
                    return Err(blocked("/", "ENML node limit"));
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(blocked("/", "ENML depth limit"));
                }
                stack.push(element);
            }
            Ok(Event::Empty(event)) => {
                let element = element_from_start(&event)?;
                nodes += 1;
                if nodes > MAX_NODES {
                    return Err(blocked("/", "ENML node limit"));
                }
                append_element(element, &mut stack, &mut root)?;
            }
            Ok(Event::End(event)) => {
                let name = std::str::from_utf8(event.name().as_ref())
                    .map_err(|e| blocked("/", e.to_string()))?
                    .to_ascii_lowercase();
                let element = stack
                    .pop()
                    .ok_or_else(|| blocked("/", "unexpected end tag"))?;
                if element.name != name {
                    return Err(blocked("/", "mismatched ENML end tag"));
                }
                append_element(element, &mut stack, &mut root)?;
            }
            Ok(Event::Text(event)) => {
                let value = event
                    .unescape()
                    .map_err(|e| blocked("/", format!("invalid ENML entity: {e}")))?;
                append_text(value.as_ref(), &mut stack)?;
            }
            Ok(Event::CData(event)) => {
                let value =
                    std::str::from_utf8(event.as_ref()).map_err(|e| blocked("/", e.to_string()))?;
                append_text(value, &mut stack)?;
            }
            Ok(Event::DocType(event)) => {
                let value = event.as_ref();
                let root_name = value.split(|byte| byte.is_ascii_whitespace()).next();
                if doctype_seen
                    || root.is_some()
                    || !stack.is_empty()
                    || root_name != Some(b"en-note".as_slice())
                    || value.contains(&b'[')
                {
                    return Err(blocked("/", "unsafe or unexpected ENML DOCTYPE"));
                }
                doctype_seen = true;
            }
            Ok(Event::Decl(decl)) if root.is_none() && stack.is_empty() && !declaration_seen => {
                declaration_seen = true;
                let version = decl
                    .version()
                    .map_err(|e| blocked("/", format!("invalid XML declaration: {e}")))?;
                if version.as_ref() != b"1.0" {
                    return Err(blocked("/", "unsupported ENML XML version"));
                }
                if let Some(encoding) = decl.encoding() {
                    let encoding =
                        encoding.map_err(|e| blocked("/", format!("invalid XML encoding: {e}")))?;
                    if !encoding.eq_ignore_ascii_case(b"utf-8") {
                        return Err(blocked("/", "non-UTF-8 ENML declaration"));
                    }
                }
            }
            Ok(Event::Comment(_)) => {} // Evernote sanitizer removes comments.
            Ok(Event::PI(_)) | Ok(Event::Decl(_)) => {
                return Err(blocked("/", "unsupported ENML processing instruction"));
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(blocked("/", format!("malformed ENML XML: {e}"))),
        }
        buffer.clear();
    }
    if !stack.is_empty() {
        return Err(blocked("/", "unclosed ENML element"));
    }
    let root = root.ok_or_else(|| blocked("/", "missing en-note root"))?;
    if root.name != "en-note" {
        return Err(blocked("/", "root must be en-note"));
    }
    for name in root.attrs.keys() {
        if name != "xmlns" {
            return Err(blocked(
                "/en-note",
                format!("unsupported root attribute {name}"),
            ));
        }
    }
    Ok(root)
}

fn element_from_start(event: &BytesStart<'_>) -> Result<Element, EnmlFidelityBlocker> {
    let name = std::str::from_utf8(event.name().as_ref())
        .map_err(|e| blocked("/", e.to_string()))?
        .to_ascii_lowercase();
    let mut attrs = BTreeMap::new();
    for attr in event.attributes().with_checks(true) {
        let attr = attr.map_err(|e| blocked(&name, format!("invalid attribute: {e}")))?;
        let key = std::str::from_utf8(attr.key.as_ref())
            .map_err(|e| blocked(&name, e.to_string()))?
            .to_ascii_lowercase();
        let value = attr
            .unescape_value()
            .map_err(|e| blocked(&name, format!("invalid attribute entity: {e}")))?
            .into_owned();
        if attrs.insert(key.clone(), value).is_some() {
            return Err(blocked(&name, format!("duplicate attribute {key}")));
        }
    }
    Ok(Element {
        name,
        attrs,
        children: Vec::new(),
    })
}

fn append_element(
    element: Element,
    stack: &mut [Element],
    root: &mut Option<Element>,
) -> Result<(), EnmlFidelityBlocker> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(Child::Element(element));
    } else if root.replace(element).is_some() {
        return Err(blocked("/", "multiple ENML roots"));
    }
    Ok(())
}
fn append_text(value: &str, stack: &mut [Element]) -> Result<(), EnmlFidelityBlocker> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(Child::Text(value.to_owned()));
        Ok(())
    } else if is_xml_formatting_whitespace(value) {
        Ok(())
    } else {
        Err(blocked("/", "text outside en-note root"))
    }
}

struct RenderContext<'a> {
    resources: &'a BTreeMap<String, Vec<VerifiedEnmlResource>>,
    conservative_link_bytes: usize,
}
impl RenderContext<'_> {
    fn block(
        &mut self,
        child: &Child,
        path: &str,
        out: &mut String,
    ) -> Result<(), EnmlFidelityBlocker> {
        match child {
            Child::Text(text) if is_xml_formatting_whitespace(text) => Ok(()),
            Child::Text(text) => {
                out.push_str("<p>");
                escape(text, out);
                out.push_str("</p>");
                Ok(())
            }
            Child::Element(element) => match element.name.as_str() {
                "div" | "p" | "h1" | "h2" | "h3" => {
                    self.attrs(element, &[], path)?;
                    if element.name == "div" && element.children.iter().filter(|child| !matches!(child, Child::Text(text) if is_xml_formatting_whitespace(text))).count() == 1 {
                        if let Some((media_index, Child::Element(media))) = element.children.iter().enumerate().find(|(_, child)| !matches!(child, Child::Text(text) if is_xml_formatting_whitespace(text))) {
                            if media.name == "en-media" {
                                return self.media(media, &format!("{path}/{media_index}"), out, false);
                            }
                        }
                    }
                    let tag = if element.name == "div" {
                        "p"
                    } else {
                        &element.name
                    };
                    out.push('<');
                    out.push_str(tag);
                    out.push('>');
                    for (i, item) in element.children.iter().enumerate() {
                        self.inline(item, &format!("{path}/{i}"), out)?;
                    }
                    out.push_str("</");
                    out.push_str(tag);
                    out.push('>');
                    Ok(())
                }
                "ul" | "ol" => self.list(element, path, out),
                "en-media" => self.media(element, path, out, false),
                "br" => {
                    self.attrs(element, &[], path)?;
                    out.push_str("<p><br></p>");
                    Ok(())
                }
                _ => Err(blocked(
                    path,
                    format!("unsupported block <{}>", element.name),
                )),
            },
        }
    }
    fn inline(
        &mut self,
        child: &Child,
        path: &str,
        out: &mut String,
    ) -> Result<(), EnmlFidelityBlocker> {
        match child {
            Child::Text(text) => {
                escape(text, out);
                Ok(())
            }
            Child::Element(element) => {
                let tag = match element.name.as_str() {
                    "b" | "strong" => "strong",
                    "i" | "em" => "em",
                    "u" => "u",
                    "s" | "strike" | "del" => "s",
                    "mark" => "mark",
                    "span" => "span",
                    "a" => "a",
                    "br" => "br",
                    "en-media" => return self.media(element, path, out, true),
                    _ => {
                        return Err(blocked(
                            path,
                            format!("unsupported inline <{}>", element.name),
                        ));
                    }
                };
                if tag == "a" {
                    self.attrs(element, &["href"], path)?;
                    if element.children.iter().any(contains_media) {
                        return Err(blocked(
                            path,
                            "linked image is not representable in canonical inline model",
                        ));
                    }
                    if element.children.iter().any(contains_anchor) {
                        return Err(blocked(path, "nested links cannot be projected faithfully"));
                    }
                    let href = element
                        .attrs
                        .get("href")
                        .ok_or_else(|| blocked(path, "link missing href"))?;
                    if !valid_link(href) || href.chars().any(char::is_whitespace) {
                        return Err(blocked(path, format!("unsafe link {href}")));
                    }
                    let runs = element
                        .children
                        .iter()
                        .map(conservative_link_runs)
                        .sum::<usize>();
                    if runs == 0 {
                        return Err(blocked(path, "empty link cannot be represented"));
                    }
                    let charge = href.len().saturating_mul(runs);
                    self.conservative_link_bytes =
                        self.conservative_link_bytes.saturating_add(charge);
                    if self.conservative_link_bytes > MAX_RETAINED_LINK_BYTES {
                        return Err(blocked(path, "canonical link retention budget exceeded"));
                    }
                    out.push_str("<a href=\"");
                    escape(href, out);
                    out.push_str("\">");
                } else {
                    self.attrs(element, &[], path)?;
                    out.push('<');
                    out.push_str(tag);
                    out.push('>');
                }
                if tag == "br" {
                    if !element.children.is_empty() {
                        return Err(blocked(path, "br must be empty"));
                    }
                    return Ok(());
                }
                for (i, item) in element.children.iter().enumerate() {
                    self.inline(item, &format!("{path}/{i}"), out)?;
                }
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
                Ok(())
            }
        }
    }
    fn list(
        &mut self,
        element: &Element,
        path: &str,
        out: &mut String,
    ) -> Result<(), EnmlFidelityBlocker> {
        self.attrs(element, &[], path)?;
        let mut checked = Vec::new();
        for (i, child) in element.children.iter().enumerate() {
            let Child::Element(li) = child else {
                if matches!(child, Child::Text(t) if is_xml_formatting_whitespace(t)) {
                    continue;
                }
                return Err(blocked(path, "list contains non-item content"));
            };
            if li.name != "li" {
                return Err(blocked(path, "list contains non-li element"));
            }
            self.attrs(li, &[], &format!("{path}/{i}"))?;
            let state = match li.children.first() {
                Some(Child::Element(todo)) if todo.name == "en-todo" => {
                    self.attrs(todo, &["checked"], &format!("{path}/{i}/0"))?;
                    if !todo.children.is_empty() {
                        return Err(blocked(path, "en-todo must be empty"));
                    }
                    let checked = match todo.attrs.get("checked").map(String::as_str) {
                        Some("true") => true,
                        None | Some("false") => false,
                        Some(_) => return Err(blocked(path, "invalid en-todo checked value")),
                    };
                    Some(checked)
                }
                _ => None,
            };
            checked.push((li, state));
        }
        let checklist = checked.iter().any(|(_, state)| state.is_some());
        if checklist && (element.name != "ul" || checked.iter().any(|(_, state)| state.is_none())) {
            return Err(blocked(
                path,
                "mixed or ordered checklist cannot be represented",
            ));
        }
        out.push('<');
        out.push_str(&element.name);
        if checklist {
            out.push_str(" data-type=\"checklist\"");
        }
        out.push('>');
        for (index, (li, state)) in checked.iter().enumerate() {
            out.push_str("<li");
            if let Some(state) = state {
                out.push_str(if *state {
                    " data-checked=\"true\""
                } else {
                    " data-checked=\"false\""
                });
            }
            out.push('>');
            for (i, child) in li
                .children
                .iter()
                .enumerate()
                .skip(usize::from(state.is_some()))
            {
                self.inline(child, &format!("{path}/li{index}/{i}"), out)?;
            }
            out.push_str("</li>");
        }
        out.push_str("</");
        out.push_str(&element.name);
        out.push('>');
        Ok(())
    }
    fn media(
        &mut self,
        element: &Element,
        path: &str,
        out: &mut String,
        inline: bool,
    ) -> Result<(), EnmlFidelityBlocker> {
        self.attrs(element, &["hash", "type"], path)?;
        if !element.children.is_empty() {
            return Err(blocked(path, "en-media must be empty"));
        }
        let hash = element
            .attrs
            .get("hash")
            .ok_or_else(|| blocked(path, "en-media missing hash"))?
            .to_ascii_lowercase();
        if hash.len() != 32 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(blocked(path, "invalid en-media MD5 hash"));
        }
        let candidates = self
            .resources
            .get(&hash)
            .ok_or_else(|| blocked(path, format!("missing same-note resource {hash}")))?;
        if candidates.len() != 1 {
            return Err(blocked(
                path,
                format!("ambiguous same-note resource {hash}"),
            ));
        }
        let resource = &candidates[0];
        let declared = element
            .attrs
            .get("type")
            .ok_or_else(|| blocked(path, "en-media missing type"))?;
        if declared != &resource.mime {
            return Err(blocked(
                path,
                format!(
                    "MIME mismatch for {hash}: declared {declared}, verified {}",
                    resource.mime
                ),
            ));
        }
        if resource.filename.is_empty()
            || resource.filename.contains(['/', '\\'])
            || resource.filename.chars().any(char::is_control)
        {
            return Err(blocked(path, "unsafe or empty resource filename"));
        }
        if resource.mime.starts_with("image/") {
            if inline {
                out.push_str("<img src=\":/");
            } else {
                out.push_str("<img data-joplin-lite-block-image=\"true\" src=\":/");
            }
            out.push_str(resource.resource_id.as_str());
            out.push_str("\" alt=\"");
            escape(&resource.filename, out);
            out.push_str("\">");
        } else {
            if inline {
                return Err(blocked(
                    path,
                    "inline non-image attachment is not representable",
                ));
            }
            out.push_str("<a data-joplin-lite-block-attachment=\"true\" href=\":/");
            out.push_str(resource.resource_id.as_str());
            out.push_str("\" data-resource-id=\"");
            out.push_str(resource.resource_id.as_str());
            out.push_str("\" data-filename=\"");
            escape(&resource.filename, out);
            out.push_str("\" data-media-type=\"");
            escape(&resource.mime, out);
            out.push_str("\">");
            escape(&resource.filename, out);
            out.push_str("</a>");
        }
        Ok(())
    }
    fn attrs(
        &self,
        element: &Element,
        allowed: &[&str],
        path: &str,
    ) -> Result<(), EnmlFidelityBlocker> {
        for name in element.attrs.keys() {
            if !allowed.contains(&name.as_str()) {
                return Err(blocked(
                    path,
                    format!("unsupported {} attribute {name}", element.name),
                ));
            }
        }
        Ok(())
    }
}

fn contains_media(child: &Child) -> bool {
    match child {
        Child::Text(_) => false,
        Child::Element(element) => {
            element.name == "en-media" || element.children.iter().any(contains_media)
        }
    }
}

fn contains_anchor(child: &Child) -> bool {
    match child {
        Child::Text(_) => false,
        Child::Element(element) => {
            element.name == "a" || element.children.iter().any(contains_anchor)
        }
    }
}

// A conservative upper bound on canonical text runs after whitespace and
// line-break normalization. Over-rejection is explicit; silent link loss is not.
fn conservative_link_runs(child: &Child) -> usize {
    match child {
        Child::Text(text) if text.is_empty() => 0,
        Child::Text(text) => 2 + text.chars().filter(|c| matches!(c, '\n' | '\r')).count(),
        Child::Element(element) => element.children.iter().map(conservative_link_runs).sum(),
    }
}

fn escape(input: &str, out: &mut String) {
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

fn is_xml_formatting_whitespace(text: &str) -> bool {
    text.chars()
        .all(|character| matches!(character, ' ' | '\t' | '\r' | '\n'))
}
