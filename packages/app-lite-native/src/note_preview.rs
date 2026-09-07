use crate::core::{Note, NoteListItem};
use crate::html_body::{parse_html, resource_ids, search_text};

const MAX_PREVIEW_TITLE_CHARS: usize = 120;
const MAX_PREVIEW_SNIPPET_CHARS: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePreview {
    pub note_id: String,
    pub title: String,
    pub snippet: String,
    pub updated_label: String,
    pub first_image_id: Option<String>,
    pub updated_time: i64,
}

pub fn preview_from_note(note: &Note, now_ms: i64) -> NotePreview {
    let parsed = parse_html(&note.body);
    let (body_text, first_image_id) = match parsed {
        Ok(document) => (
            search_text(&document),
            resource_ids(&document).into_iter().next(),
        ),
        Err(_) => (note.body_text.clone(), None),
    };
    let title_source = if note.title.trim().is_empty() {
        first_nonempty_line(&body_text).unwrap_or("无标题笔记")
    } else {
        note.title.trim()
    };
    NotePreview {
        note_id: note.id.clone(),
        title: truncate_chars(title_source, MAX_PREVIEW_TITLE_CHARS),
        snippet: truncate_chars(&body_text, MAX_PREVIEW_SNIPPET_CHARS),
        updated_label: updated_label(note.updated_time, now_ms),
        first_image_id,
        updated_time: note.updated_time,
    }
}

pub fn preview_from_list_item(item: &NoteListItem, now_ms: i64) -> NotePreview {
    let title_source = if item.title.trim().is_empty() {
        first_nonempty_line(&item.body_text).unwrap_or("无标题笔记")
    } else {
        item.title.trim()
    };
    NotePreview {
        note_id: item.id.clone(),
        title: truncate_chars(title_source, MAX_PREVIEW_TITLE_CHARS),
        snippet: truncate_chars(&item.body_text, MAX_PREVIEW_SNIPPET_CHARS),
        updated_label: updated_label(item.updated_time, now_ms),
        first_image_id: item.first_image_id.clone(),
        updated_time: item.updated_time,
    }
}

fn first_nonempty_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut characters = text.chars();
    let truncated: String = characters.by_ref().take(limit).collect();
    if characters.next().is_some() {
        truncated
            .chars()
            .take(limit.saturating_sub(1))
            .chain(std::iter::once('…'))
            .collect()
    } else {
        truncated
    }
}

fn updated_label(updated_time: i64, now_ms: i64) -> String {
    let elapsed = now_ms.saturating_sub(updated_time);
    if elapsed < 60_000 {
        "刚刚".into()
    } else if elapsed < 3_600_000 {
        format!("{}分钟前", elapsed / 60_000)
    } else if elapsed < 86_400_000 {
        "今天".into()
    } else {
        format!("{}天前", elapsed / 86_400_000)
    }
}

#[cfg(test)]
mod tests {
    use super::{NotePreview, preview_from_note};
    use crate::core::{Note, NoteListItem};

    fn note(title: &str, body: &str, body_text: &str, updated_time: i64) -> Note {
        Note {
            id: "note-1".into(),
            title: title.into(),
            body: body.into(),
            body_text: body_text.into(),
            markup_language: 2,
            is_draft: false,
            created_time: updated_time,
            updated_time,
            deleted_time: 0,
        }
    }

    #[test]
    fn red_preview_strips_markup_falls_back_to_body_title_and_keeps_first_image_order() {
        let body = concat!(
            "<h2>正文标题</h2><p>Hello <strong>world</strong></p>",
            "<p><img src=\":/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" alt=\"B\">",
            "<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"A\"></p>"
        );
        let preview = preview_from_note(
            &note("", body, "stale fallback", 1_700_000_000_000),
            1_700_000_120_000,
        );
        assert_eq!(
            preview,
            NotePreview {
                note_id: "note-1".into(),
                title: "正文标题".into(),
                snippet: "正文标题\nHello world\nBA".into(),
                updated_label: "2分钟前".into(),
                first_image_id: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
                updated_time: 1_700_000_000_000,
            }
        );
        assert!(!preview.snippet.contains('<'));
        assert!(!preview.snippet.contains("strong"));
    }

    #[test]
    fn red_preview_truncates_unicode_by_scalar_and_uses_deterministic_dates() {
        let body = format!("<p>{}</p>", "😀中".repeat(80));
        let preview = preview_from_note(
            &note(
                "显式标题",
                &body,
                "fallback",
                1_700_000_000_000 - 3 * 86_400_000,
            ),
            1_700_000_000_000,
        );
        assert_eq!(preview.title, "显式标题");
        assert!(preview.snippet.chars().count() <= 96);
        assert_eq!(preview.updated_label, "3天前");
        assert!(
            preview
                .snippet
                .chars()
                .all(|character| character != '\u{fffd}')
        );
    }

    #[test]
    fn red_preview_parse_failure_is_text_only_and_never_fakes_a_resource() {
        let body = "<div>".repeat(4097);
        let preview = preview_from_note(&note("", &body, "safe fallback", 0), 60_000);
        assert_eq!(preview.title, "safe fallback");
        assert_eq!(preview.snippet, "safe fallback");
        assert_eq!(preview.first_image_id, None);
    }

    #[test]
    fn lightweight_preview_uses_projected_text_without_parsing_html() {
        let preview = super::preview_from_list_item(
            &NoteListItem {
                id: "note-2".into(),
                title: String::new(),
                body_text: "😀 projected".into(),
                updated_time: 1_700_000_000_000,
                first_image_id: Some("resource-1".into()),
            },
            1_700_000_060_000,
        );
        assert_eq!(preview.title, "😀 projected");
        assert_eq!(preview.first_image_id.as_deref(), Some("resource-1"));
    }
}
