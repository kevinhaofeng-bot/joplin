#[cfg(test)]
mod tests {
    use super::{extract_resource_ids, markdown_marker, marker_spans, project_search_text};

    #[test]
    fn marker_round_trip_and_projection_are_joplin_compatible() {
        let id = "0123456789abcdef0123456789abcdef";
        let marker = markdown_marker(id, "庭审截图 [1]").unwrap();
        assert_eq!(
            marker,
            "![庭审截图 \\[1\\]](:/0123456789abcdef0123456789abcdef)"
        );
        let body = format!("证据如下\n\n{marker}\n\n结论");
        assert_eq!(extract_resource_ids(&body), vec![id]);
        let projected = project_search_text(&body);
        assert!(projected.contains("庭审截图 [1]"));
        assert!(!projected.contains("0123456789abcdef"));
        assert!(!projected.contains('\u{fffc}'));
    }

    #[test]
    fn marker_spans_skip_broken_prefixes_and_keep_escaped_alt_and_emoji() {
        let id = "0123456789abcdef0123456789abcdef";
        let escaped = markdown_marker(id, "a [x\\y]").unwrap();
        let body = format!("😀![broken\n{escaped}\n![second](:/{id})");
        let spans = marker_spans(&body);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].alt, "a [x\\y]");
        assert_eq!(spans[1].alt, "second");
        assert_eq!(extract_resource_ids(&body), vec![id, id]);
    }
}
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BodyError {
    #[error("invalid resource id")]
    InvalidResourceId,
}

pub fn markdown_marker(resource_id: &str, alt: &str) -> Result<String, BodyError> {
    validate_resource_id(resource_id)?;
    let escaped = alt
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    Ok(format!("![{escaped}](:/{resource_id})"))
}

pub fn extract_resource_ids(body: &str) -> Vec<String> {
    marker_spans(body)
        .into_iter()
        .map(|span| span.resource_id)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerSpan {
    pub start: usize,
    pub end: usize,
    pub resource_id: String,
    pub alt: String,
}

pub fn marker_spans(body: &str) -> Vec<MarkerSpan> {
    let mut spans = Vec::new();
    let mut offset = 0;
    while offset < body.len() {
        let Some(relative) = body[offset..].find("![") else {
            break;
        };
        let start = offset + relative;
        if let Some((end, id, alt)) = parse_marker(body, start) {
            spans.push(MarkerSpan {
                start,
                end,
                resource_id: id.to_owned(),
                alt: unescape_alt(alt),
            });
            offset = end;
        } else {
            offset = start + 2;
        }
    }
    spans
}

pub fn project_search_text(body: &str) -> String {
    let mut projected = String::with_capacity(body.len());
    let mut cursor = 0;
    for span in marker_spans(body) {
        projected.push_str(&body[cursor..span.start]);
        projected.push_str(&span.alt);
        cursor = span.end;
    }
    projected.push_str(&body[cursor..]);
    projected
}

pub(crate) fn validate_resource_id(resource_id: &str) -> Result<(), BodyError> {
    if resource_id.len() == 32
        && resource_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(BodyError::InvalidResourceId)
    }
}

fn parse_marker(body: &str, start: usize) -> Option<(usize, &str, &str)> {
    if !body.get(start..)?.starts_with("![") {
        return None;
    }
    let bytes = body.as_bytes();
    let mut index = start + 2;
    let alt_start = index;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if !escaped && byte == b'!' && bytes.get(index + 1) == Some(&b'[') {
            return None;
        }
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            index += 1;
            continue;
        }
        if byte == b']' {
            break;
        }
        index += 1;
    }
    if index >= bytes.len() || !body.get(index..)?.starts_with("](:/") {
        return None;
    }
    let id_start = index + 4;
    let id_end = id_start.checked_add(32)?;
    let id = body.get(id_start..id_end)?;
    if validate_resource_id(id).is_err() || body.get(id_end..)?.as_bytes().first() != Some(&b')') {
        return None;
    }
    Some((id_end + 1, id, body.get(alt_start..index)?))
}

fn unescape_alt(alt: &str) -> String {
    let mut output = String::with_capacity(alt.len());
    let mut chars = alt.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some(next @ ('\\' | '[' | ']')) => output.push(next),
                Some(next) => {
                    output.push('\\');
                    output.push(next);
                }
                None => output.push('\\'),
            }
        } else {
            output.push(character);
        }
    }
    output
}
