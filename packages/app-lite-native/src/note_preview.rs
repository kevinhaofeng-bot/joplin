use crate::core::{Note, NoteListItem};
use crate::html_body::{parse_html, resource_ids, search_text};

const MAX_PREVIEW_TITLE_CHARS: usize = 120;
const MAX_PREVIEW_SNIPPET_CHARS: usize = 96;
const MAX_SEARCH_SNIPPET_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePreview {
    pub note_id: String,
    pub title: String,
    pub snippet: String,
    pub search_text: String,
    pub updated_label: String,
    pub first_image_id: Option<String>,
    pub updated_time: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewRange {
    pub location: usize,
    pub length: usize,
}

impl PreviewRange {
    pub const fn new(location: usize, length: usize) -> Self {
        Self { location, length }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewDisplay {
    pub title: String,
    pub snippet: String,
    pub title_highlights: Vec<PreviewRange>,
    pub snippet_highlights: Vec<PreviewRange>,
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
        search_text: body_text,
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
        search_text: item.body_text.clone(),
        updated_label: updated_label(item.updated_time, now_ms),
        first_image_id: item.first_image_id.clone(),
        updated_time: item.updated_time,
    }
}

pub fn preview_display_for_list_item(
    item: &NoteListItem,
    query: &str,
    now_ms: i64,
) -> PreviewDisplay {
    let preview = preview_from_list_item(item, now_ms);
    preview_display_for_preview(&preview, query)
}

pub fn preview_display_for_preview(preview: &NotePreview, query: &str) -> PreviewDisplay {
    let title = preview.title.clone();
    let terms = search_terms(query);
    let body_spans = match_spans(&preview.search_text, &terms);
    let snippet = if let Some((first_start, _)) = body_spans.first().copied() {
        search_context_snippet(&preview.search_text, first_start)
    } else {
        preview.snippet.clone()
    };
    PreviewDisplay {
        title_highlights: preview_ranges(&title, &terms),
        snippet_highlights: preview_ranges(&snippet, &terms),
        title,
        snippet,
    }
}

fn search_terms(query: &str) -> Vec<Vec<char>> {
    let trimmed = query.trim();
    if trimmed.is_empty() || trimmed.contains('\u{fffc}') {
        return Vec::new();
    }
    let mut terms = Vec::<Vec<char>>::new();
    for candidate in std::iter::once(trimmed).chain(trimmed.split_whitespace()) {
        if candidate.is_empty() || candidate.contains('\u{fffc}') {
            continue;
        }
        let term = candidate.chars().map(ascii_fold_char).collect::<Vec<_>>();
        if !term.is_empty() && !terms.iter().any(|existing| existing == &term) {
            terms.push(term);
        }
    }
    terms
}

fn match_spans(text: &str, terms: &[Vec<char>]) -> Vec<(usize, usize)> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut spans = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\u{fffc}' {
            index += 1;
            continue;
        }
        let matched = terms.iter().find_map(|term| {
            if index + term.len() > chars.len()
                || chars[index..index + term.len()].contains(&'\u{fffc}')
            {
                return None;
            }
            chars[index..index + term.len()]
                .iter()
                .map(|character| ascii_fold_char(*character))
                .eq(term.iter().copied())
                .then_some(term.len())
        });
        if let Some(length) = matched {
            spans.push((index, length));
            index += length;
        } else {
            index += 1;
        }
    }
    spans
}

fn preview_ranges(text: &str, terms: &[Vec<char>]) -> Vec<PreviewRange> {
    let chars = text.chars().collect::<Vec<_>>();
    let utf16_offsets = chars
        .iter()
        .scan(0usize, |offset, character| {
            let start = *offset;
            *offset += character.len_utf16();
            Some(start)
        })
        .chain(std::iter::once(
            chars.iter().map(|character| character.len_utf16()).sum(),
        ))
        .collect::<Vec<_>>();
    match_spans(text, terms)
        .into_iter()
        .map(|(location, length)| {
            PreviewRange::new(
                utf16_offsets[location],
                utf16_offsets[location + length] - utf16_offsets[location],
            )
        })
        .collect()
}

fn search_context_snippet(body_text: &str, first_match: usize) -> String {
    let chars = body_text.chars().collect::<Vec<_>>();
    let start = first_match.saturating_sub(30);
    let prefix_len = usize::from(start > 0);
    let mut end = (start + MAX_SEARCH_SNIPPET_CHARS - prefix_len).min(chars.len());
    let suffix_len = usize::from(end < chars.len());
    if suffix_len > 0 {
        end = (start + MAX_SEARCH_SNIPPET_CHARS - prefix_len - suffix_len).min(chars.len());
    }
    let mut snippet = String::new();
    if prefix_len > 0 {
        snippet.push('…');
    }
    snippet.extend(chars[start..end].iter());
    if suffix_len > 0 {
        snippet.push('…');
    }
    snippet
}

fn ascii_fold_char(character: char) -> char {
    character.to_ascii_lowercase()
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
    use super::{
        NotePreview, PreviewRange, preview_display_for_list_item, preview_display_for_preview,
        preview_from_list_item, preview_from_note,
    };
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
                search_text: "正文标题\nHello world\nBA".into(),
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

    #[test]
    fn search_preview_display_highlights_terms_and_derives_deep_context() {
        let item = NoteListItem {
            id: "note-search".into(),
            title: "😀Alpha 复盘".into(),
            body_text: format!("{}目标在正文深处，后面还有更多内容", "开头内容 ".repeat(12)),
            updated_time: 1_700_000_000_000,
            first_image_id: None,
        };
        let display = preview_display_for_list_item(&item, "alpha 目标", 1_700_000_120_000);
        assert_eq!(display.title, "😀Alpha 复盘");
        assert_eq!(display.title_highlights, vec![PreviewRange::new(2, 5)]);
        assert!(display.snippet.starts_with('…'));
        assert!(display.snippet.contains("目标"));
        assert!(!display.snippet_highlights.is_empty());

        let early = NoteListItem {
            body_text: "目标在开头，后续正文".into(),
            ..item.clone()
        };
        let early_display = preview_display_for_list_item(&early, "目标", 1_700_000_120_000);
        assert!(!early_display.snippet.starts_with('…'));
        assert_eq!(early_display.snippet_highlights[0], PreviewRange::new(0, 2));

        let fallback = preview_display_for_list_item(&item, "不存在", 1_700_000_120_000);
        assert_eq!(fallback.snippet, super::truncate_chars(&item.body_text, 96));
        assert!(fallback.title_highlights.is_empty());
        assert!(fallback.snippet_highlights.is_empty());

        let empty = preview_display_for_list_item(&item, "   ", 1_700_000_120_000);
        assert!(empty.title_highlights.is_empty());
        assert!(empty.snippet_highlights.is_empty());

        let separated = NoteListItem {
            body_text: "😀复盘 \u{fffc}同步".into(),
            ..item
        };
        let separated_display =
            preview_display_for_list_item(&separated, "复盘 同步", 1_700_000_120_000);
        assert_eq!(
            separated_display.snippet_highlights,
            vec![PreviewRange::new(2, 2), PreviewRange::new(6, 2)]
        );

        let preview = preview_from_list_item(&separated, 1_700_000_120_000);
        let highlighted = preview_display_for_preview(&preview, "复盘");
        assert!(!highlighted.snippet_highlights.is_empty());
        let cleared = preview_display_for_preview(&preview, "");
        assert!(cleared.title_highlights.is_empty());
        assert!(cleared.snippet_highlights.is_empty());
    }
}
