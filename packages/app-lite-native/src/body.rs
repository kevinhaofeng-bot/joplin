#[cfg(test)]
mod tests {
    use super::{extract_resource_ids, markdown_marker, project_search_text};

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
    let mut ids = Vec::new();
    let mut offset = 0;
    while offset < body.len() {
        let Some(relative) = body[offset..].find("![") else {
            break;
        };
        let start = offset + relative;
        if let Some((end, id, _alt)) = parse_marker(body, start) {
            ids.push(id.to_owned());
            offset = end;
        } else {
            offset = start + 2;
        }
    }
    ids
}

pub fn project_search_text(body: &str) -> String {
    let mut projected = String::with_capacity(body.len());
    let mut cursor = 0;
    let mut search_from = 0;
    while search_from < body.len() {
        let Some(relative) = body[search_from..].find("![") else {
            break;
        };
        let start = search_from + relative;
        let Some((end, _id, alt)) = parse_marker(body, start) else {
            search_from = start + 2;
            continue;
        };
        projected.push_str(&body[cursor..start]);
        projected.push_str(&unescape_alt(alt));
        cursor = end;
        search_from = end;
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
