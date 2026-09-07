use joplin_lite_native::body::{markdown_marker, marker_spans};
use joplin_lite_native::core::{
    CreateNote, HtmlNoteConversion, LegacyNoteForHtmlMigration, Note, NoteContentUpdate,
    NoteRepository, ResourceImport,
};
use joplin_lite_native::html_body::{
    Block, Document, HtmlBodyError, Inline, Marks, parse_html, resource_ids, search_text,
    serialize_html,
};
use joplin_lite_native::native_editor::{
    EmptyBlockCarrier, InlineCommand, NativeEditorSession, RenderedAttachment, RenderedDocument,
    apply_committed_text_delta, apply_inline_command, delete_image_anchor_if_identity,
    document_from_session, insert_image_anchor, render_session, session_from_document,
};
use joplin_lite_native::resource_store::MAX_IMAGE_BYTES;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
#[allow(deprecated)]
use objc2_app_kit::NSObliquenessAttributeName;
#[allow(deprecated)]
use objc2_app_kit::NSShadowAttributeName;
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSAttachmentAttributeName, NSAttributedStringAppKitDocumentFormats,
    NSAttributedStringAttachmentConveniences, NSBackgroundColorAttributeName, NSBackingStoreType,
    NSBaselineOffsetAttributeName, NSBezelStyle, NSBitmapImageFileType, NSBitmapImageRep,
    NSBorderType, NSBox, NSBoxType, NSButton, NSButtonType, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSControlTextEditingDelegate, NSDragOperation, NSDraggingDestination,
    NSDraggingInfo, NSEventModifierFlags, NSFont, NSFontAttributeName, NSFontTraitMask,
    NSForegroundColorAttributeName, NSImage, NSKernAttributeName, NSLayoutAttribute,
    NSLineBreakMode, NSMenu, NSMenuItem, NSMutableAttributedStringAppKitAdditions,
    NSMutableParagraphStyle, NSParagraphStyle, NSParagraphStyleAttributeName, NSPasteboard,
    NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeTIFF, NSResponder, NSScrollView,
    NSSearchField, NSStackView, NSStackViewDistribution, NSStrikethroughStyleAttributeName,
    NSStrokeColorAttributeName, NSStrokeWidthAttributeName, NSTextAlignment, NSTextAttachment,
    NSTextDelegate, NSTextField, NSTextFieldDelegate, NSTextInputClient, NSTextView,
    NSTextViewDelegate, NSUnderlineStyle, NSUnderlineStyleAttributeName,
    NSUserInterfaceLayoutOrientation, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSData, NSDictionary,
    NSMutableAttributedString, NSMutableCopying, NSNotification, NSNumber, NSObject,
    NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSURL, ns_string,
};
use std::cell::{OnceCell, RefCell};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::{NonNull, null_mut};
use std::sync::Arc;

const RESOURCE_ID_ATTRIBUTE: &str = "com.kevinhao.joplin-lite.resource-id";
const RESOURCE_ALT_ATTRIBUTE: &str = "com.kevinhao.joplin-lite.resource-alt";
const MISSING_RESOURCE_ATTRIBUTE: &str = "com.kevinhao.joplin-lite.missing-resource";

struct PreparedNoteContent {
    update: NoteContentUpdate,
}

struct EditorSnapshot {
    attributed: Retained<NSMutableAttributedString>,
    selection: NSRange,
}

#[derive(Debug, Clone)]
struct PendingEditorIntent {
    range: NSRange,
    replacement: String,
    old_view_text: String,
    old_semantic_text: String,
    covered_attachments: Vec<RenderedAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingIntentDecision {
    Noop,
    ApplyText { range: NSRange, replacement: String },
    DeleteImage { range: NSRange, resource_id: String },
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingEditorComposition {
    baseline_view_text: String,
    baseline_semantic_text: String,
    baseline_range: NSRange,
    current_view_text: String,
    current_marked_range: NSRange,
    replacement: String,
    covered_attachments: Vec<RenderedAttachment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingIntentRejection {
    StaleBaseline,
    InvalidUtf16,
    InvalidReplacement,
    AmbiguousAttachment,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
enum EditorCodecError {
    #[error("HTML document error: {0}")]
    Html(#[from] HtmlBodyError),
    #[error("invalid attachment marker: {0}")]
    Body(#[from] joplin_lite_native::body::BodyError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteRoute {
    BodyImporter,
    NativeResponder,
}

fn paste_route(body_is_first_responder: bool, has_current_note: bool) -> PasteRoute {
    if body_is_first_responder && has_current_note {
        PasteRoute::BodyImporter
    } else {
        PasteRoute::NativeResponder
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn should_sync_editor_change(has_marked_text: bool, loading_guard: bool) -> bool {
    !has_marked_text && !loading_guard
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum EditorTextSyncDecision {
    Noop,
    Reject,
    DeleteImage,
    ApplyDelta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorSessionSyncResult {
    Noop,
    Applied,
    Rejected,
}

fn should_persist_after_editor_sync(result: EditorSessionSyncResult) -> bool {
    matches!(
        result,
        EditorSessionSyncResult::Noop | EditorSessionSyncResult::Applied
    )
}

fn should_restore_after_editor_sync(result: EditorSessionSyncResult) -> bool {
    matches!(result, EditorSessionSyncResult::Rejected)
}

#[cfg_attr(not(test), allow(dead_code))]
fn classify_editor_text_change(old_text: &str, new_text: &str) -> EditorTextSyncDecision {
    if old_text == new_text {
        return EditorTextSyncDecision::Noop;
    }
    let old_chars: Vec<char> = old_text.chars().collect();
    let new_chars: Vec<char> = new_text.chars().collect();
    let mut prefix = 0usize;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old_chars.len().saturating_sub(prefix)
        && suffix < new_chars.len().saturating_sub(prefix)
        && old_chars[old_chars.len() - suffix - 1] == new_chars[new_chars.len() - suffix - 1]
    {
        suffix += 1;
    }
    let old_end = old_chars.len() - suffix;
    let new_end = new_chars.len() - suffix;
    let removed = &old_chars[prefix..old_end];
    let replacement = &new_chars[prefix..new_end];
    if removed == ['\u{fffc}'] && replacement.is_empty() {
        EditorTextSyncDecision::DeleteImage
    } else if replacement.contains(&'\u{fffc}') || removed.contains(&'\u{fffc}') {
        EditorTextSyncDecision::Reject
    } else {
        EditorTextSyncDecision::ApplyDelta
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn editor_text_delta(old_text: &str, new_text: &str) -> Option<(NSRange, String)> {
    if old_text.contains('\u{fffc}') || new_text.contains('\u{fffc}') {
        return None;
    }
    let old_chars: Vec<char> = old_text.chars().collect();
    let new_chars: Vec<char> = new_text.chars().collect();
    let mut prefix = 0usize;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old_chars.len().saturating_sub(prefix)
        && suffix < new_chars.len().saturating_sub(prefix)
        && old_chars[old_chars.len() - suffix - 1] == new_chars[new_chars.len() - suffix - 1]
    {
        suffix += 1;
    }
    let replacement: String = new_chars[prefix..new_chars.len() - suffix].iter().collect();
    let location = old_chars[..prefix]
        .iter()
        .map(|character| character.len_utf16())
        .sum();
    let length = old_chars[prefix..old_chars.len() - suffix]
        .iter()
        .map(|character| character.len_utf16())
        .sum();
    Some((NSRange::new(location, length), replacement))
}

fn utf16_scalar_range(text: &str, range: NSRange) -> Option<(usize, usize)> {
    let end = range.location.checked_add(range.length)?;
    let mut offset = 0usize;
    let mut start = (range.location == 0).then_some(0);
    let mut finish = (end == 0).then_some(0);
    for (scalar, character) in text.chars().enumerate() {
        if offset == range.location {
            start = Some(scalar);
        }
        offset += character.len_utf16();
        if offset == end {
            finish = Some(scalar + 1);
        }
    }
    if start.is_none() && range.location == offset {
        start = Some(text.chars().count());
    }
    if finish.is_none() && end == offset {
        finish = Some(text.chars().count());
    }
    Some((start?, finish?))
}

fn apply_utf16_intent_to_text(old_text: &str, range: NSRange, replacement: &str) -> Option<String> {
    let (start, end) = utf16_scalar_range(old_text, range)?;
    let mut result = String::new();
    result.extend(old_text.chars().take(start));
    result.push_str(replacement);
    result.extend(old_text.chars().skip(end));
    Some(result)
}

fn decide_pending_editor_intent(
    intent: &PendingEditorIntent,
    new_view_text: &str,
) -> PendingIntentDecision {
    if intent.old_view_text != intent.old_semantic_text {
        return PendingIntentDecision::Reject;
    }
    let Some(expected_new_text) =
        apply_utf16_intent_to_text(&intent.old_view_text, intent.range, &intent.replacement)
    else {
        return PendingIntentDecision::Reject;
    };
    if expected_new_text != new_view_text {
        return PendingIntentDecision::Reject;
    }
    let Some((start, end)) = utf16_scalar_range(&intent.old_semantic_text, intent.range) else {
        return PendingIntentDecision::Reject;
    };
    let old_slice: String = intent
        .old_semantic_text
        .chars()
        .skip(start)
        .take(end - start)
        .collect();
    if old_slice.contains('\u{fffc}') || intent.replacement.contains('\u{fffc}') {
        if old_slice == "\u{fffc}"
            && intent.replacement.is_empty()
            && intent.covered_attachments.len() == 1
        {
            return PendingIntentDecision::DeleteImage {
                range: intent.range,
                resource_id: intent.covered_attachments[0].resource_id.clone(),
            };
        }
        return PendingIntentDecision::Reject;
    }
    if old_slice == intent.replacement {
        PendingIntentDecision::Noop
    } else {
        PendingIntentDecision::ApplyText {
            range: intent.range,
            replacement: intent.replacement.clone(),
        }
    }
}

fn preflight_pending_editor_intent(
    intent: &PendingEditorIntent,
) -> Result<(), PendingIntentRejection> {
    if intent.old_view_text != intent.old_semantic_text {
        return Err(PendingIntentRejection::StaleBaseline);
    }
    if intent
        .replacement
        .chars()
        .any(|character| character == '\0' || character == '\u{fffc}')
    {
        return Err(PendingIntentRejection::InvalidReplacement);
    }
    let Some((start, end)) = utf16_scalar_range(&intent.old_view_text, intent.range) else {
        return Err(PendingIntentRejection::InvalidUtf16);
    };
    let old_slice: String = intent
        .old_view_text
        .chars()
        .skip(start)
        .take(end - start)
        .collect();
    if old_slice.contains('\u{fffc}')
        && (old_slice != "\u{fffc}"
            || !intent.replacement.is_empty()
            || intent.covered_attachments.len() != 1)
    {
        return Err(PendingIntentRejection::AmbiguousAttachment);
    }
    Ok(())
}

fn accumulate_marked_editor_intent(
    current: Option<PendingEditorComposition>,
    old_view_text: &str,
    old_semantic_text: &str,
    range: NSRange,
    replacement: &str,
    marked_range: NSRange,
    covered_attachments: Vec<RenderedAttachment>,
) -> Result<PendingEditorComposition, PendingIntentRejection> {
    if replacement
        .chars()
        .any(|character| character == '\0' || character == '\u{fffc}')
    {
        return Err(PendingIntentRejection::InvalidReplacement);
    }
    match current {
        None => {
            let intent = PendingEditorIntent {
                range,
                replacement: replacement.to_owned(),
                old_view_text: old_view_text.to_owned(),
                old_semantic_text: old_semantic_text.to_owned(),
                covered_attachments,
            };
            preflight_pending_editor_intent(&intent)?;
            if marked_range != range {
                return Err(PendingIntentRejection::InvalidUtf16);
            }
            let Some(current_view_text) =
                apply_utf16_intent_to_text(old_view_text, range, replacement)
            else {
                return Err(PendingIntentRejection::InvalidUtf16);
            };
            Ok(PendingEditorComposition {
                baseline_view_text: old_view_text.to_owned(),
                baseline_semantic_text: old_semantic_text.to_owned(),
                baseline_range: range,
                current_view_text,
                current_marked_range: NSRange::new(
                    range.location,
                    NSString::from_str(replacement).length(),
                ),
                replacement: replacement.to_owned(),
                covered_attachments: intent.covered_attachments,
            })
        }
        Some(mut current) => {
            if old_view_text != current.current_view_text
                || old_semantic_text != current.baseline_semantic_text
                || range != current.current_marked_range
                || marked_range != range
            {
                return Err(PendingIntentRejection::StaleBaseline);
            }
            let Some(current_view_text) =
                apply_utf16_intent_to_text(old_view_text, range, replacement)
            else {
                return Err(PendingIntentRejection::InvalidUtf16);
            };
            let Some(expected_view_text) = apply_utf16_intent_to_text(
                &current.baseline_view_text,
                current.baseline_range,
                replacement,
            ) else {
                return Err(PendingIntentRejection::InvalidUtf16);
            };
            if current_view_text != expected_view_text {
                return Err(PendingIntentRejection::StaleBaseline);
            }
            current.current_view_text = current_view_text;
            current.current_marked_range =
                NSRange::new(range.location, NSString::from_str(replacement).length());
            current.replacement = replacement.to_owned();
            Ok(current)
        }
    }
}

fn finish_marked_editor_intent(
    composition: &PendingEditorComposition,
    new_view_text: &str,
) -> PendingIntentDecision {
    if new_view_text != composition.current_view_text {
        return PendingIntentDecision::Reject;
    }
    let intent = PendingEditorIntent {
        range: composition.baseline_range,
        replacement: composition.replacement.clone(),
        old_view_text: composition.baseline_view_text.clone(),
        old_semantic_text: composition.baseline_semantic_text.clone(),
        covered_attachments: composition.covered_attachments.clone(),
    };
    decide_pending_editor_intent(&intent, new_view_text)
}

fn exact_empty_block_carrier(
    rendered: &RenderedDocument,
    selection: NSRange,
) -> Option<&EmptyBlockCarrier> {
    if selection.length != 0 {
        return None;
    }
    rendered
        .empty_block_carriers
        .iter()
        .find(|carrier| carrier.addressable_offset == selection.location)
}

fn adjust_projection_attachments(
    attachments: &mut Vec<RenderedAttachment>,
    range: NSRange,
    replacement_length: usize,
    removed_resource_id: Option<&str>,
) -> bool {
    let Some(end) = range.location.checked_add(range.length) else {
        return false;
    };
    if let Some(resource_id) = removed_resource_id
        && !attachments.iter().any(|attachment| {
            attachment.addressable_offset >= range.location
                && attachment.addressable_offset < end
                && attachment.resource_id == resource_id
        })
    {
        return false;
    }
    let mut removed = false;
    attachments.retain(|attachment| {
        if attachment.addressable_offset >= range.location && attachment.addressable_offset < end {
            let matches_identity =
                removed_resource_id.is_none_or(|resource_id| resource_id == attachment.resource_id);
            if matches_identity {
                removed = true;
                return false;
            }
            return true;
        }
        true
    });
    let delta = replacement_length as isize - range.length as isize;
    for attachment in attachments.iter_mut() {
        if attachment.addressable_offset >= end {
            let shifted = attachment.addressable_offset as isize + delta;
            if shifted < 0 {
                return false;
            }
            attachment.addressable_offset = shifted as usize;
        }
    }
    removed_resource_id.is_none() || removed
}

fn resource_id_attribute_key() -> Retained<NSAttributedStringKey> {
    NSString::from_str(RESOURCE_ID_ATTRIBUTE)
}

fn resource_alt_attribute_key() -> Retained<NSAttributedStringKey> {
    NSString::from_str(RESOURCE_ALT_ATTRIBUTE)
}

fn missing_resource_attribute_key() -> Retained<NSAttributedStringKey> {
    NSString::from_str(MISSING_RESOURCE_ATTRIBUTE)
}

fn string_for_range(source: &NSAttributedString, range: NSRange) -> String {
    source
        .attributedSubstringFromRange(range)
        .string()
        .to_string()
}

fn attribute_string(
    attributes: &NSDictionary<NSAttributedStringKey, AnyObject>,
    key: &NSAttributedStringKey,
) -> Option<String> {
    unsafe { attributes.objectForKey_unchecked(key) }
        .and_then(|value| value.downcast_ref::<NSString>())
        .map(ToString::to_string)
}

#[allow(deprecated)]
fn projection_attachments_from_storage(body: &NSTextView) -> Vec<RenderedAttachment> {
    let Some(storage) = (unsafe { body.textStorage() }) else {
        return Vec::new();
    };
    let length = storage.length();
    let key = resource_id_attribute_key();
    let mut result = Vec::new();
    let mut location = 0usize;
    while location < length {
        let mut effective_range = NSRange::new(location, 0);
        let attributes = unsafe {
            storage.attributesAtIndex_longestEffectiveRange_inRange(
                location,
                &mut effective_range,
                NSRange::new(0, length),
            )
        };
        if let Some(resource_id) = attribute_string(&attributes, &key) {
            result.push(RenderedAttachment {
                addressable_offset: location,
                resource_id,
            });
        }
        let next = effective_range.location + effective_range.length;
        if next <= location {
            break;
        }
        location = next;
    }
    result
}

fn missing_resource_placeholder_text(alt: &str) -> String {
    if alt.is_empty() {
        "[图片]".to_string()
    } else {
        format!("[图片：{alt}]")
    }
}

fn missing_resource_marker(
    attributes: &NSDictionary<NSAttributedStringKey, AnyObject>,
    text: &str,
) -> Option<(String, String)> {
    let marker_key = missing_resource_attribute_key();
    let marked = unsafe { attributes.objectForKey_unchecked(&marker_key) }
        .and_then(|value| value.downcast_ref::<NSString>())
        .is_some_and(|value| value.to_string() == "1");
    if !marked {
        return None;
    }
    let resource_id = attribute_string(attributes, &resource_id_attribute_key())?;
    let alt = attribute_string(attributes, &resource_alt_attribute_key())?;
    if text != missing_resource_placeholder_text(&alt)
        || markdown_marker(&resource_id, &alt).is_err()
    {
        return None;
    }
    Some((resource_id, alt))
}

fn marks_from_attributes(attributes: &NSDictionary<NSAttributedStringKey, AnyObject>) -> Marks {
    let font_key = unsafe { NSFontAttributeName };
    let underline_key = unsafe { NSUnderlineStyleAttributeName };
    let font_traits = unsafe { attributes.objectForKey_unchecked(font_key) }
        .and_then(|value| value.downcast_ref::<NSFont>())
        .map(|font| {
            let marker = font.fontDescriptor().symbolicTraits();
            (
                marker.contains(objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitBold),
                marker.contains(objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitItalic),
            )
        })
        .unwrap_or((false, false));
    let underline = unsafe { attributes.objectForKey_unchecked(underline_key) }
        .and_then(|value| value.downcast_ref::<NSNumber>())
        .is_some_and(|value| value.intValue() != 0);
    Marks {
        bold: font_traits.0,
        italic: font_traits.1,
        underline,
        strikethrough: false,
        highlight: false,
        link: None,
    }
}

fn append_document_text(inlines: &mut Vec<Inline>, text: &str, marks: &Marks) {
    if text.is_empty() {
        return;
    }
    if let Some(Inline::Text {
        text: previous,
        marks: previous_marks,
    }) = inlines.last_mut()
        && previous_marks == marks
    {
        previous.push_str(text);
    } else {
        inlines.push(Inline::Text {
            text: text.to_owned(),
            marks: marks.clone(),
        });
    }
}

fn document_from_attributed_string(
    source: &NSAttributedString,
) -> Result<Document, EditorCodecError> {
    let length = source.string().length();
    let mut blocks = vec![Vec::new()];
    let mut location = 0;
    let id_key = resource_id_attribute_key();
    let alt_key = resource_alt_attribute_key();
    let attachment_key = unsafe { NSAttachmentAttributeName };
    while location < length {
        let mut effective_range = NSRange::new(location, 0);
        let attributes = unsafe {
            source.attributesAtIndex_longestEffectiveRange_inRange(
                location,
                &mut effective_range,
                NSRange::new(0, length),
            )
        };
        let text = string_for_range(source, effective_range).replace("\r\n", "\n");
        let marks = marks_from_attributes(&attributes);
        let resource_id = attribute_string(&attributes, &id_key);
        let alt = attribute_string(&attributes, &alt_key).unwrap_or_else(|| "图片".into());
        let has_attachment = unsafe { attributes.objectForKey_unchecked(attachment_key) }.is_some();
        if let Some((resource_id, alt)) = missing_resource_marker(&attributes, &text) {
            blocks
                .last_mut()
                .expect("document always has a block")
                .push(Inline::Image { resource_id, alt });
        } else {
            for character in text.chars() {
                match character {
                    '\n' | '\r' => blocks.push(Vec::new()),
                    '\u{2028}' | '\u{000b}' => blocks
                        .last_mut()
                        .expect("document always has a block")
                        .push(Inline::SoftBreak),
                    '\u{fffc}' if has_attachment => {
                        let inlines = blocks.last_mut().expect("document always has a block");
                        if let Some(resource_id) = resource_id.as_ref()
                            && markdown_marker(resource_id, "").is_ok()
                        {
                            inlines.push(Inline::Image {
                                resource_id: resource_id.to_string(),
                                alt: alt.clone(),
                            });
                        } else {
                            append_document_text(inlines, "[图片]", &marks);
                        }
                    }
                    _ => append_document_text(
                        blocks.last_mut().expect("document always has a block"),
                        &character.to_string(),
                        &marks,
                    ),
                }
            }
        }
        let next = effective_range
            .location
            .saturating_add(effective_range.length);
        if next <= location {
            break;
        }
        location = next;
    }
    Ok(Document::from_blocks(
        blocks
            .into_iter()
            .map(|inlines| Block::Paragraph {
                style: Default::default(),
                inlines,
            })
            .collect(),
    ))
}

#[allow(dead_code)]
fn attributed_text_with_marks(text: &str, marks: &Marks) -> Retained<NSMutableAttributedString> {
    let attributed = NSMutableAttributedString::from_nsstring(&NSString::from_str(text));
    if text.is_empty() {
        return attributed;
    }
    let range = NSRange::new(0, NSString::from_str(text).length());
    let mut font = NSFont::systemFontOfSize(17.0);
    if marks.bold || marks.italic {
        let descriptor = font.fontDescriptor();
        let mut traits = descriptor.symbolicTraits();
        if marks.bold {
            traits.insert(objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitBold);
        }
        if marks.italic {
            traits.insert(objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitItalic);
        }
        if let Some(converted) = NSFont::fontWithDescriptor_size(
            &descriptor.fontDescriptorWithSymbolicTraits(traits),
            17.0,
        ) {
            font = converted;
        }
    }
    unsafe {
        attributed.addAttribute_value_range(NSFontAttributeName, &font, range);
        if marks.underline {
            let value = NSNumber::numberWithInteger(NSUnderlineStyle::Single.0);
            attributed.addAttribute_value_range(NSUnderlineStyleAttributeName, &value, range);
        }
    }
    attributed
}

#[allow(dead_code)]
fn render_document_to_attributed_string<F>(
    document: &Document,
    mut resource_loader: F,
    available_width: f64,
) -> (Retained<NSMutableAttributedString>, usize)
where
    F: FnMut(&str) -> Option<joplin_lite_native::core::StoredResource>,
{
    let output = NSMutableAttributedString::from_nsstring(ns_string!(""));
    let mut attachment_failures = 0;
    for (block_index, block) in document.blocks.iter().enumerate() {
        let inlines: Vec<&Inline> = match block {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => {
                inlines.iter().collect()
            }
            Block::List { items, .. } => {
                items.iter().flat_map(|item| item.inlines.iter()).collect()
            }
        };
        for inline in inlines {
            match inline {
                Inline::Text { text, marks } => {
                    output.appendAttributedString(&attributed_text_with_marks(text, marks));
                }
                Inline::SoftBreak => output.appendAttributedString(&attributed_text_with_marks(
                    "\u{2028}",
                    &Marks::default(),
                )),
                Inline::Image { resource_id, alt } => {
                    let inline = resource_loader(resource_id).and_then(|resource| {
                        inline_attachment_with_width(&resource, alt, available_width)
                    });
                    if let Some(inline) = inline {
                        output.appendAttributedString(&inline);
                    } else {
                        attachment_failures += 1;
                        let placeholder = attributed_text_with_marks(
                            &missing_resource_placeholder_text(alt),
                            &Marks::default(),
                        );
                        let range = NSRange::new(0, placeholder.string().length());
                        let marker = NSString::from_str("1");
                        let id = NSString::from_str(resource_id);
                        let alt = NSString::from_str(alt);
                        unsafe {
                            placeholder.addAttribute_value_range(
                                &missing_resource_attribute_key(),
                                &marker,
                                range,
                            );
                            placeholder.addAttribute_value_range(
                                &resource_id_attribute_key(),
                                &id,
                                range,
                            );
                            placeholder.addAttribute_value_range(
                                &resource_alt_attribute_key(),
                                &alt,
                                range,
                            );
                        }
                        output.appendAttributedString(&placeholder);
                    }
                }
            }
        }
        if block_index + 1 < document.blocks.len() {
            output.appendAttributedString(&attributed_text_with_marks("\n", &Marks::default()));
        }
    }
    (output, attachment_failures)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LayoutRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl LayoutRect {
    fn ns_rect(self) -> NSRect {
        NSRect::new(
            NSPoint::new(self.x, self.y),
            NSSize::new(self.width, self.height),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ContentLayout {
    sidebar: LayoutRect,
    list: LayoutRect,
    title: LayoutRect,
    toolbar: LayoutRect,
    body: LayoutRect,
    delete: LayoutRect,
    status: LayoutRect,
    empty_editor: LayoutRect,
}

/// Derive every content frame from the current content size in one place.
/// This pure calculation is also exercised at the two supported window sizes.
fn content_layout(width: f64, height: f64) -> ContentLayout {
    let width = width.max(860.0);
    let height = height.max(560.0);
    let sidebar_width = (width * 0.30).clamp(258.0, 280.0);
    let right_width = width - sidebar_width;
    let margin = if right_width >= 700.0 { 48.0 } else { 36.0 };
    // Byword-like reading measure: keep long lines comfortable on wide
    // windows, while still filling the available space at the MVP minimum.
    let editor_width = (right_width - margin * 2.0).clamp(400.0, 720.0);
    let editor_x = sidebar_width + (right_width - editor_width) / 2.0;
    let toolbar_y = height - 132.0;
    let body_y = 28.0;
    let body_height = (toolbar_y - body_y - 16.0).max(220.0);

    let sidebar = LayoutRect {
        x: 0.0,
        y: 0.0,
        width: sidebar_width,
        height,
    };
    let list = LayoutRect {
        x: 0.0,
        y: 16.0,
        width: sidebar_width,
        height: (height - 116.0).max(240.0),
    };
    let title = LayoutRect {
        x: editor_x,
        y: height - 84.0,
        width: (editor_width - 84.0).max(260.0),
        height: 42.0,
    };
    let toolbar = LayoutRect {
        x: editor_x,
        y: toolbar_y,
        width: editor_width,
        height: 30.0,
    };
    let body = LayoutRect {
        x: editor_x,
        y: body_y,
        width: editor_width,
        height: body_height,
    };
    let delete = LayoutRect {
        x: editor_x + editor_width - 62.0,
        y: height - 60.0,
        width: 62.0,
        height: 26.0,
    };
    let status = LayoutRect {
        x: editor_x + editor_width - 164.0,
        y: toolbar_y + 5.0,
        width: 154.0,
        height: 20.0,
    };
    let empty_editor = LayoutRect {
        x: body.x,
        // The empty copy is two lines; keep its optical center at the same
        // slightly-above-middle position used by the writing canvas.
        y: body.y + body.height * 0.45 - 22.0,
        width: body.width,
        height: 44.0,
    };

    ContentLayout {
        sidebar,
        list,
        title,
        toolbar,
        body,
        delete,
        status,
        empty_editor,
    }
}

fn display_note_title(title: &str, body: &str) -> String {
    if !title.trim().is_empty() {
        return title.trim().chars().take(120).collect();
    }
    body.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(120).collect())
        .filter(|line: &String| !line.is_empty())
        .unwrap_or_else(|| "无标题笔记".to_string())
}

fn note_list_title(note: &Note) -> String {
    display_note_title(&note.title, &note.body_text)
}

fn note_list_summary(note: &Note) -> String {
    note.body_text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(52).collect::<String>())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "暂无正文".to_string())
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FormatDecision {
    Add,
    Remove,
    Clear,
}

fn format_decision(format: TextFormat, active: bool) -> FormatDecision {
    if format == TextFormat::Clear {
        FormatDecision::Clear
    } else if active {
        FormatDecision::Remove
    } else {
        FormatDecision::Add
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FontTraitOperation {
    Have,
    NotHave,
    None,
}

fn typing_trait_operation(format: TextFormat, decision: FormatDecision) -> FontTraitOperation {
    match (format, decision) {
        (TextFormat::Bold | TextFormat::Italic, FormatDecision::Add) => FontTraitOperation::Have,
        (TextFormat::Bold | TextFormat::Italic, FormatDecision::Remove) => {
            FontTraitOperation::NotHave
        }
        _ => FontTraitOperation::None,
    }
}

fn candidate_with_attachment(
    source: &NSAttributedString,
    insertion_range: NSRange,
    inline: &NSMutableAttributedString,
) -> Option<Retained<NSMutableAttributedString>> {
    let source_length = source.string().length();
    let end = insertion_range
        .location
        .checked_add(insertion_range.length)?;
    if end > source_length {
        return None;
    }
    let candidate = source.mutableCopy();
    candidate.replaceCharactersInRange_withAttributedString(insertion_range, inline);
    Some(candidate)
}

fn canonical_marker_ranges(body: &str) -> Vec<(NSRange, String, String)> {
    marker_spans(body)
        .into_iter()
        .map(|span| {
            let location = body[..span.start].encode_utf16().count();
            let length = body[span.start..span.end].encode_utf16().count();
            (NSRange::new(location, length), span.resource_id, span.alt)
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum DataFileError {
    Symlink,
    NotRegularFile,
    MetadataFailed,
    CreateFailed,
}

/// Validate the database inode without following links, creating a new file
/// atomically when it does not exist yet.
fn ensure_notes_database_file(path: &Path) -> Result<(), DataFileError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(DataFileError::Symlink);
            }
            if !file_type.is_file() {
                return Err(DataFileError::NotRegularFile);
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(_) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure_notes_database_file(path)
                }
                Err(_) => Err(DataFileError::CreateFailed),
            }
        }
        Err(_) => Err(DataFileError::MetadataFailed),
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum DataDirError {
    OverrideMustBeAbsolute,
    DefaultDirectoryUnavailable,
    HomeDirectoryUnavailable,
    CanonicalizationFailed,
    CreateDirectoryFailed,
    OfficialJoplinProfile,
}

fn choose_data_dir(
    override_path: Option<&Path>,
    default_path: Option<&Path>,
) -> Result<PathBuf, DataDirError> {
    if let Some(path) = override_path {
        if !path.is_absolute() {
            return Err(DataDirError::OverrideMustBeAbsolute);
        }
        return Ok(path.to_path_buf());
    }
    default_path
        .map(Path::to_path_buf)
        .ok_or(DataDirError::DefaultDirectoryUnavailable)
}

fn validate_canonical_data_dir(
    candidate: &Path,
    official_profiles: &[PathBuf],
) -> Result<(), DataDirError> {
    if official_profiles
        .iter()
        .any(|profile| candidate == profile || candidate.starts_with(profile))
    {
        return Err(DataDirError::OfficialJoplinProfile);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacyMigrationFailure {
    note_id: String,
    reason: String,
}

fn decode_legacy_rtf_for_html_migration(
    legacy: &LegacyNoteForHtmlMigration,
) -> Result<Retained<NSMutableAttributedString>, LegacyMigrationFailure> {
    if legacy.body_rtf.is_empty() {
        return Ok(NSMutableAttributedString::from_nsstring(
            &NSString::from_str(&legacy.body),
        ));
    }
    let rtf = NSData::with_bytes(&legacy.body_rtf);
    let parsed = unsafe {
        NSAttributedString::initWithRTF_documentAttributes(NSAttributedString::alloc(), &rtf, None)
    }
    .ok_or_else(|| LegacyMigrationFailure {
        note_id: legacy.id.clone(),
        reason: "RTF 解码失败".into(),
    })?;
    if parsed.string().to_string() != legacy.body {
        return Err(LegacyMigrationFailure {
            note_id: legacy.id.clone(),
            reason: "RTF 正文与旧正文不一致".into(),
        });
    }
    Ok(NSMutableAttributedString::from_attributed_nsstring(&parsed))
}

fn migrate_legacy_note_to_conversion(
    repository: &NoteRepository,
    legacy: &LegacyNoteForHtmlMigration,
) -> Result<HtmlNoteConversion, LegacyMigrationFailure> {
    let attributed = decode_legacy_rtf_for_html_migration(legacy)?;
    for (range, resource_id, alt) in canonical_marker_ranges(&legacy.body).into_iter().rev() {
        let resource = repository
            .get_resource(&resource_id)
            .map_err(|_| LegacyMigrationFailure {
                note_id: legacy.id.clone(),
                reason: "读取图片资源失败".into(),
            })?
            .ok_or_else(|| LegacyMigrationFailure {
                note_id: legacy.id.clone(),
                reason: "图片资源不存在".into(),
            })?;
        let inline =
            inline_attachment_with_alt(&resource, &alt).ok_or_else(|| LegacyMigrationFailure {
                note_id: legacy.id.clone(),
                reason: "图片资源无法解码".into(),
            })?;
        attributed.replaceCharactersInRange_withAttributedString(range, inline.as_ref());
    }
    let source: &NSAttributedString = &attributed;
    let document = document_from_attributed_string(source).map_err(|_| LegacyMigrationFailure {
        note_id: legacy.id.clone(),
        reason: "正文无法转换为 HTML".into(),
    })?;
    Ok(HtmlNoteConversion {
        id: legacy.id.clone(),
        source_updated_time: legacy.updated_time,
        body: serialize_html(&document),
        body_text: search_text(&document),
        resource_ids: resource_ids(&document),
    })
}

fn migrate_legacy_notes_before_window(
    repository: &NoteRepository,
) -> Result<(), LegacyMigrationFailure> {
    let legacy_notes = repository
        .list_legacy_notes_for_html_migration()
        .map_err(|_| LegacyMigrationFailure {
            note_id: "<database>".into(),
            reason: "读取旧笔记失败".into(),
        })?;
    if legacy_notes.is_empty() {
        return Ok(());
    }
    let conversions = legacy_notes
        .iter()
        .map(|legacy| migrate_legacy_note_to_conversion(repository, legacy))
        .collect::<Result<Vec<_>, _>>()?;
    repository
        .apply_html_migration(conversions)
        .map(|_| ())
        .map_err(|_| LegacyMigrationFailure {
            note_id: "<database>".into(),
            reason: "原子迁移失败，数据库保持旧格式".into(),
        })
}

fn show_legacy_migration_failure(
    mtm: MainThreadMarker,
    failure: &LegacyMigrationFailure,
    profile: &Path,
) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("笔记迁移未完成"));
    alert.setInformativeText(&NSString::from_str(&legacy_migration_recovery_message(
        failure, profile,
    )));
    let _ = alert.runModal();
}

fn legacy_migration_recovery_message(failure: &LegacyMigrationFailure, profile: &Path) -> String {
    format!(
        "笔记 {}：{}\n数据目录：{}\n迁移尚未完成；如果迁移已经开始，备份可能位于该目录。请先保留当前数据，再修复后重试。",
        failure.note_id,
        failure.reason,
        profile.display()
    )
}

fn canonicalize_for_comparison(path: &Path) -> Result<PathBuf, DataDirError> {
    let mut suffix = Vec::<OsString>::new();
    let mut current = path;
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut canonical) => {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(component) = current.file_name() else {
                    return Err(DataDirError::CanonicalizationFailed);
                };
                suffix.push(component.to_os_string());
                let Some(parent) = current.parent() else {
                    return Err(DataDirError::CanonicalizationFailed);
                };
                current = parent;
            }
            Err(_) => return Err(DataDirError::CanonicalizationFailed),
        }
    }
}

fn resolve_native_data_dir(
    override_path: Option<PathBuf>,
    default_path: Option<PathBuf>,
    home_path: Option<PathBuf>,
) -> Result<PathBuf, DataDirError> {
    let candidate = choose_data_dir(override_path.as_deref(), default_path.as_deref())?;
    let home = home_path.ok_or(DataDirError::HomeDirectoryUnavailable)?;
    let official_profiles = [
        home.join(".config").join("joplin-desktop"),
        home.join("Library")
            .join("Application Support")
            .join("Joplin"),
        home.join("Library")
            .join("Application Support")
            .join("Joplin")
            .join("Joplin Desktop"),
    ];
    let canonical_official_profiles = official_profiles
        .iter()
        .map(|profile| canonicalize_for_comparison(profile))
        .collect::<Result<Vec<_>, _>>()?;
    let canonical_candidate = canonicalize_for_comparison(&candidate)?;
    validate_canonical_data_dir(&canonical_candidate, &canonical_official_profiles)?;
    std::fs::create_dir_all(&candidate).map_err(|_| DataDirError::CreateDirectoryFailed)?;
    let canonical_candidate = canonicalize_for_comparison(&candidate)?;
    validate_canonical_data_dir(&canonical_candidate, &canonical_official_profiles)?;
    Ok(canonical_candidate)
}

struct BodyTextViewIvars {
    owner: NonNull<AppDelegate>,
}

define_class!(
    #[unsafe(super = NSTextView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = BodyTextViewIvars]
    struct BodyTextView;
    unsafe impl NSObjectProtocol for BodyTextView {}
    unsafe impl NSDraggingDestination for BodyTextView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            let owner = unsafe { self.ivars().owner.as_ref() };
            if owner.accept_drag(sender) {
                NSDragOperation::Copy
            } else {
                NSDragOperation::None
            }
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let owner = unsafe { self.ivars().owner.as_ref() };
            owner.perform_drag(self, sender)
        }
    }
);

impl BodyTextView {
    fn new(mtm: MainThreadMarker, owner: &AppDelegate, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(BodyTextViewIvars {
            owner: NonNull::from(owner),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

struct AppDelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    repository: Arc<NoteRepository>,
    current_note_id: RefCell<Option<String>>,
    notes: RefCell<Vec<Note>>,
    loading_guard: RefCell<bool>,
    editor_session: RefCell<Option<NativeEditorSession>>,
    projection_attachments: RefCell<Vec<RenderedAttachment>>,
    projection_empty_carriers: RefCell<Vec<EmptyBlockCarrier>>,
    pending_editor_intents: RefCell<Vec<PendingEditorIntent>>,
    pending_editor_composition: RefCell<Option<PendingEditorComposition>>,
    pending_editor_intent_invalid: RefCell<bool>,
    sidebar_background: OnceCell<Retained<NSBox>>,
    sidebar_separator: OnceCell<Retained<NSBox>>,
    list_scroll: OnceCell<Retained<NSScrollView>>,
    list_stack: OnceCell<Retained<NSStackView>>,
    list_empty_label: OnceCell<Retained<NSTextField>>,
    new_button: OnceCell<Retained<NSButton>>,
    search_field: OnceCell<Retained<NSSearchField>>,
    title_field: OnceCell<Retained<NSTextField>>,
    bold_button: OnceCell<Retained<NSButton>>,
    italic_button: OnceCell<Retained<NSButton>>,
    underline_button: OnceCell<Retained<NSButton>>,
    clear_button: OnceCell<Retained<NSButton>>,
    body_scroll: OnceCell<Retained<NSScrollView>>,
    body_view: OnceCell<Retained<NSTextView>>,
    delete_button: OnceCell<Retained<NSButton>>,
    save_status: OnceCell<Retained<NSTextField>>,
    editor_empty_label: OnceCell<Retained<NSTextField>>,
    note_buttons: RefCell<Vec<Retained<NSButton>>>,
    note_rows: RefCell<Vec<Retained<NSBox>>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct AppDelegate;
    unsafe impl NSObjectProtocol for AppDelegate {}
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn application_should_terminate_after_last_window_closed(
            &self,
            _sender: &NSApplication,
        ) -> bool {
            true
        }

        #[unsafe(method(applicationSupportsSecureRestorableState:))]
        fn application_supports_secure_restorable_state(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, notification: &NSNotification) {
            let mtm = self.mtm();
            let application = notification
                .object()
                .unwrap()
                .downcast::<NSApplication>()
                .unwrap();
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1100.0, 720.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Miniaturizable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            window.setContentMinSize(NSSize::new(860.0, 560.0));
            unsafe { window.setReleasedWhenClosed(false) };
            window.setTitle(ns_string!("Joplin Lite Native"));

            let content = window.contentView().expect("window content view");

            // A softly tinted reading-list rail keeps the writing canvas calm
            // while remaining fully dynamic in light and dark appearance.
            let sidebar_background = NSBox::initWithFrame(
                NSBox::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            sidebar_background.setBoxType(NSBoxType::Custom);
            sidebar_background.setTransparent(false);
            sidebar_background.setFillColor(&NSColor::underPageBackgroundColor());
            sidebar_background.setBorderWidth(0.0);
            content.addSubview(&sidebar_background);

            let separator = NSBox::initWithFrame(
                NSBox::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            separator.setBoxType(NSBoxType::Separator);
            content.addSubview(&separator);

            let list_scroll = NSScrollView::initWithFrame(
                NSScrollView::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            list_scroll.setHasVerticalScroller(true);
            list_scroll.setAutohidesScrollers(true);
            list_scroll.setBorderType(NSBorderType::NoBorder);
            list_scroll.setDrawsBackground(false);
            let list_stack = NSStackView::initWithFrame(
                NSStackView::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            list_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            list_stack.setSpacing(4.0);
            list_stack.setDistribution(NSStackViewDistribution::GravityAreas);
            list_stack.setAlignment(NSLayoutAttribute::Width);
            list_stack.setEdgeInsets(objc2_foundation::NSEdgeInsets {
                top: 10.0,
                left: 12.0,
                bottom: 10.0,
                right: 12.0,
            });
            list_scroll.setDocumentView(Some(&list_stack));
            content.addSubview(&list_scroll);

            let list_empty_label = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            list_empty_label.setStringValue(ns_string!("还没有笔记"));
            list_empty_label.setBezeled(false);
            list_empty_label.setDrawsBackground(false);
            list_empty_label.setEditable(false);
            list_empty_label.setAlignment(NSTextAlignment::Center);
            list_empty_label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
            list_empty_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            content.addSubview(&list_empty_label);

            let new_button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("新建笔记"),
                    Some(self),
                    Some(sel!(newNote:)),
                    mtm,
                )
            };
            new_button.setBezelStyle(NSBezelStyle::Push);
            new_button.setBezelColor(Some(&NSColor::controlAccentColor()));
            new_button.setContentTintColor(Some(&NSColor::whiteColor()));
            content.addSubview(&new_button);

            let search_field = NSSearchField::initWithFrame(
                NSSearchField::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            search_field.setPlaceholderString(Some(ns_string!("搜索笔记")));
            search_field.setContinuous(true);
            search_field.setFont(Some(&NSFont::systemFontOfSize(13.0)));
            unsafe {
                search_field.setTarget(Some(self));
                search_field.setAction(Some(sel!(searchNotes:)));
            }
            content.addSubview(&search_field);

            let title_field = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            title_field.setStringValue(ns_string!(""));
            title_field.setPlaceholderString(Some(ns_string!("标题")));
            title_field.setEditable(true);
            title_field.setBezeled(false);
            title_field.setDrawsBackground(false);
            title_field.setFont(Some(&NSFont::systemFontOfSize_weight(28.0, 0.5)));
            title_field.setTextColor(Some(&NSColor::labelColor()));
            title_field.setMaximumNumberOfLines(1);
            title_field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            content.addSubview(&title_field);

            let bold_button = Self::make_format_button(mtm, self, "B", sel!(toggleBoldText:));
            let italic_button = Self::make_format_button(mtm, self, "I", sel!(toggleItalicText:));
            let underline_button =
                Self::make_format_button(mtm, self, "U", sel!(toggleUnderlineText:));
            let clear_button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("清除格式"),
                    Some(self),
                    Some(sel!(clearFormatting:)),
                    mtm,
                )
            };
            clear_button.setBezelStyle(NSBezelStyle::Toolbar);
            clear_button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            content.addSubview(&bold_button);
            content.addSubview(&italic_button);
            content.addSubview(&underline_button);
            content.addSubview(&clear_button);

            let body = BodyTextView::new(
                mtm,
                self,
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            let file_url_type = unsafe { NSPasteboardTypeFileURL };
            let drag_types: Retained<NSArray<NSString>> = NSArray::from_slice(&[file_url_type]);
            body.registerForDraggedTypes(&drag_types);
            body.setEditable(true);
            body.setRichText(true);
            // NativeEditorSession owns the only undo history.  NSTextView is
            // deliberately kept as a projection so it cannot create a
            // second, divergent undo stack for the same edit.
            body.setAllowsUndo(false);
            body.setImportsGraphics(false);
            body.setUsesFontPanel(false);
            body.setDrawsBackground(false);
            body.setFont(Some(&NSFont::systemFontOfSize(17.0)));
            body.setTextColor(Some(&NSColor::labelColor()));
            body.setTextContainerInset(NSSize::new(0.0, 16.0));
            body.setHorizontallyResizable(false);
            body.setVerticallyResizable(true);
            let paragraph = NSMutableParagraphStyle::new();
            paragraph.setLineSpacing(4.0);
            paragraph.setParagraphSpacing(6.0);
            body.setDefaultParagraphStyle(Some(&paragraph));

            let body_scroll = NSScrollView::initWithFrame(
                NSScrollView::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            body_scroll.setHasVerticalScroller(true);
            body_scroll.setAutohidesScrollers(true);
            body_scroll.setBorderType(NSBorderType::NoBorder);
            body_scroll.setDrawsBackground(false);
            body_scroll.setDocumentView(Some(&body));
            content.addSubview(&body_scroll);

            let delete_button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("删除"),
                    Some(self),
                    Some(sel!(deleteNote:)),
                    mtm,
                )
            };
            delete_button.setBordered(false);
            delete_button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            delete_button.setHasDestructiveAction(true);
            content.addSubview(&delete_button);

            let save_status = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            save_status.setStringValue(ns_string!("已保存"));
            save_status.setBezeled(false);
            save_status.setDrawsBackground(false);
            save_status.setEditable(false);
            save_status.setAlignment(NSTextAlignment::Right);
            save_status.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            save_status.setTextColor(Some(&NSColor::secondaryLabelColor()));
            content.addSubview(&save_status);

            let editor_empty_label = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            editor_empty_label.setStringValue(ns_string!("从一条笔记开始\n点击左上角「新建笔记」"));
            editor_empty_label.setBezeled(false);
            editor_empty_label.setDrawsBackground(false);
            editor_empty_label.setEditable(false);
            editor_empty_label.setAlignment(NSTextAlignment::Center);
            editor_empty_label.setUsesSingleLineMode(false);
            editor_empty_label.setMaximumNumberOfLines(2);
            editor_empty_label.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
            editor_empty_label.setFont(Some(&NSFont::systemFontOfSize(16.0)));
            editor_empty_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            content.addSubview(&editor_empty_label);

            self.ivars().window.set(window.clone()).unwrap();
            self.ivars()
                .sidebar_background
                .set(sidebar_background)
                .unwrap();
            self.ivars().sidebar_separator.set(separator).unwrap();
            self.ivars().list_scroll.set(list_scroll).unwrap();
            self.ivars().list_stack.set(list_stack).unwrap();
            self.ivars().list_empty_label.set(list_empty_label).unwrap();
            self.ivars().new_button.set(new_button).unwrap();
            self.ivars().search_field.set(search_field).unwrap();
            self.ivars().title_field.set(title_field.clone()).unwrap();
            self.ivars().bold_button.set(bold_button).unwrap();
            self.ivars().italic_button.set(italic_button).unwrap();
            self.ivars().underline_button.set(underline_button).unwrap();
            self.ivars().clear_button.set(clear_button).unwrap();
            self.ivars().body_scroll.set(body_scroll).unwrap();
            self.ivars()
                .body_view
                .set(body.clone().into_super())
                .unwrap();
            self.ivars().delete_button.set(delete_button).unwrap();
            self.ivars().save_status.set(save_status).unwrap();
            self.ivars()
                .editor_empty_label
                .set(editor_empty_label)
                .unwrap();

            unsafe {
                title_field.setDelegate(Some(ProtocolObject::from_ref(self)));
            }
            body.setDelegate(Some(ProtocolObject::from_ref(self)));
            window.setDelegate(Some(ProtocolObject::from_ref(self)));
            self.layout_content(content.frame().size.width, content.frame().size.height);
            Self::install_menu(&application, self, mtm);
            window.center();
            window.makeKeyAndOrderFront(None);
            #[allow(deprecated)]
            application.activateIgnoringOtherApps(true);

            self.refresh_notes();
            if let Some(note) = self.ivars().notes.borrow().first().cloned() {
                self.load_note(&note);
            } else {
                self.update_editor_visibility();
            }

            if let Some(count) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_NEW_COUNT") {
                let count = count.to_string_lossy().parse::<usize>().unwrap_or(0);
                let sender = NSObject::new();
                for _ in 0..count {
                    self.new_note(sel!(newNote:), &sender);
                }
            } else if std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_NEW_NOTE").is_some() {
                let sender = NSObject::new();
                self.new_note(sel!(newNote:), &sender);
            }
            if let Some(body_text) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_BODY") {
                self.ivars()
                    .body_view
                    .get()
                    .unwrap()
                    .setString(&NSString::from_str(&body_text.to_string_lossy()));
                self.save_current_note();
            }
            if let Some(image_path) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_NATIVE_UNDO") {
                self.run_native_undo_smoke(Path::new(&image_path));
            }
            if let Some(image_path) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_RESIZE") {
                self.run_resize_smoke(Path::new(&image_path));
            }
            if let Some(query) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_SEARCH") {
                self.search_notes(&query.to_string_lossy());
                println!(
                    "searchNotes: result count={}",
                    self.ivars().notes.borrow().len()
                );
            }
            if std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_DELETE_CURRENT").is_some() {
                let sender = NSObject::new();
                self.delete_note(sel!(deleteNote:), &sender);
            }
            if std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_EXIT").is_some() {
                application.terminate(None);
            }
        }
    }
    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            if let Some(content) = self
                .ivars()
                .window
                .get()
                .and_then(|window| window.contentView())
            {
                self.layout_content(content.frame().size.width, content.frame().size.height);
            }
        }
    }
    unsafe impl NSControlTextEditingDelegate for AppDelegate {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.save_current_note();
        }
    }
    unsafe impl NSTextDelegate for AppDelegate {
        #[unsafe(method(textDidChange:))]
        #[allow(deprecated)]
        fn text_did_change(&self, _notification: &NSNotification) {
            let loading_guard = *self.ivars().loading_guard.borrow();
            let Some(body) = self.ivars().body_view.get() else {
                return;
            };
            if loading_guard {
                self.clear_pending_editor_intent();
                return;
            }
            if body.hasMarkedText() {
                let current_text = body.string().to_string();
                let composition_valid = self
                    .ivars()
                    .pending_editor_composition
                    .borrow()
                    .as_ref()
                    .is_none_or(|composition| composition.current_view_text == current_text);
                if !composition_valid {
                    self.restore_body_from_session_after_rejected_edit();
                    self.set_save_status("正文变更未保存，请重试", true);
                }
                return;
            }
            let sync_result = self.sync_editor_session_from_view();
            if should_persist_after_editor_sync(sync_result) {
                self.save_current_note();
            } else if should_restore_after_editor_sync(sync_result) {
                self.restore_body_from_session_after_rejected_edit();
                self.set_save_status("正文变更未保存，请重试", true);
            }
        }
    }
    unsafe impl NSTextFieldDelegate for AppDelegate {}
    unsafe impl NSTextViewDelegate for AppDelegate {
        #[unsafe(method(textView:shouldChangeTextInRange:replacementString:))]
        fn text_view_should_change_text_in_range_replacement_string(
            &self,
            text_view: &NSTextView,
            affected_char_range: NSRange,
            replacement_string: Option<&NSString>,
        ) -> bool {
            self.capture_pending_editor_intent(
                text_view,
                affected_char_range,
                replacement_string,
            )
        }

        #[unsafe(method(textView:doCommandBySelector:))]
        unsafe fn text_view_do_command_by_selector(
            &self,
            _text_view: &NSTextView,
            command_selector: Sel,
        ) -> bool {
            if command_selector == sel!(paste:) {
                self.handle_paste(None);
                true
            } else {
                false
            }
        }

        #[unsafe(method(textViewDidChangeSelection:))]
        fn text_view_did_change_selection(&self, _notification: &NSNotification) {
            self.reapply_empty_carrier_for_selection();
            self.update_formatting_buttons();
        }

        #[unsafe(method(textViewDidChangeTypingAttributes:))]
        fn text_view_did_change_typing_attributes(&self, _notification: &NSNotification) {
            self.update_formatting_buttons();
        }
    }
    impl AppDelegate {
        #[unsafe(method(toggleBoldText:))]
    fn toggle_bold_text(&self, _sender: &NSObject) {
        self.apply_format(TextFormat::Bold);
    }

    #[unsafe(method(toggleItalicText:))]
    fn toggle_italic_text(&self, _sender: &NSObject) {
        self.apply_format(TextFormat::Italic);
    }

    #[unsafe(method(toggleUnderlineText:))]
    fn toggle_underline_text(&self, _sender: &NSObject) {
        self.apply_format(TextFormat::Underline);
    }

    #[unsafe(method(clearFormatting:))]
    fn clear_formatting(&self, _sender: &NSObject) {
        self.apply_format(TextFormat::Clear);
    }

    #[unsafe(method(paste:))]
    fn paste(&self, _sender: &NSObject) {
        self.handle_paste(None);
    }

    #[unsafe(method(undoText:))]
    fn undo_text(&self, _sender: &NSObject) {
        let changed = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| session.can_undo() && session.undo().is_ok());
        if changed {
            self.refresh_body_from_session();
            self.save_current_note();
        }
    }

    #[unsafe(method(redoText:))]
    fn redo_text(&self, _sender: &NSObject) {
        let changed = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| session.can_redo() && session.redo().is_ok());
        if changed {
            self.refresh_body_from_session();
            self.save_current_note();
        }
    }

    #[unsafe(method(newNote:))]
    fn new_note(&self, _sender: &NSObject) {
        let note = self.ivars().repository.create_note(CreateNote {
            title: String::new(),
            body: String::new(),
            is_draft: true,
        });
        match note {
            Ok(note) => {
                self.ivars()
                    .search_field
                    .get()
                    .unwrap()
                    .setStringValue(ns_string!(""));
                self.load_note(&note);
                self.refresh_notes();
                if let Some(window) = self.ivars().window.get() {
                    window.makeFirstResponder(Some(self.ivars().body_view.get().unwrap()));
                }
                println!("newNote: action triggered");
            }
            Err(error) => eprintln!("newNote: could not create note: {error}"),
        }
    }

    #[unsafe(method(selectNote:))]
    fn select_note(&self, sender: &NSButton) {
        let index = sender.tag();
        if index < 0 {
            return;
        }
        if let Some(note) = self.ivars().notes.borrow().get(index as usize).cloned() {
            self.load_note(&note);
        }
    }

    #[unsafe(method(searchNotes:))]
    fn search_notes_action(&self, sender: &NSSearchField) {
        self.search_notes(&sender.stringValue().to_string());
    }

    #[unsafe(method(deleteNote:))]
    fn delete_note(&self, _sender: &NSObject) {
        let Some(id) = self.ivars().current_note_id.borrow_mut().take() else {
            return;
        };
        if let Err(error) = self.ivars().repository.soft_delete(&id) {
            eprintln!("deleteNote: could not delete note: {error}");
            *self.ivars().current_note_id.borrow_mut() = Some(id);
            self.set_save_status("保存失败", true);
            return;
        }
        let query = self
            .ivars()
            .search_field
            .get()
            .map(|field| field.stringValue().to_string())
            .unwrap_or_default();
        self.search_notes(&query);
        if let Some(note) = self.ivars().notes.borrow().first().cloned() {
            self.load_note(&note);
        } else {
            self.clear_current_note();
        }
    }
    }
);

impl AppDelegate {
    fn clear_pending_editor_intent(&self) {
        self.ivars().pending_editor_intents.borrow_mut().clear();
        *self.ivars().pending_editor_composition.borrow_mut() = None;
        *self.ivars().pending_editor_intent_invalid.borrow_mut() = false;
    }

    fn capture_pending_editor_intent(
        &self,
        body: &NSTextView,
        range: NSRange,
        replacement: Option<&NSString>,
    ) -> bool {
        if *self.ivars().loading_guard.borrow() {
            self.clear_pending_editor_intent();
            return false;
        }
        let Some(storage) = (unsafe { body.textStorage() }) else {
            self.clear_pending_editor_intent();
            return false;
        };
        let source: &NSAttributedString = &storage;
        let old_view_text = source.string().to_string();
        let old_semantic_text = self
            .ivars()
            .editor_session
            .borrow()
            .as_ref()
            .and_then(|session| session.text_document().to_addressable_text().ok());
        let Some(old_semantic_text) = old_semantic_text else {
            self.clear_pending_editor_intent();
            return false;
        };
        let end = range.location.saturating_add(range.length);
        let covered_attachments = self
            .ivars()
            .projection_attachments
            .borrow()
            .iter()
            .filter(|attachment| {
                attachment.addressable_offset >= range.location
                    && attachment.addressable_offset < end
            })
            .cloned()
            .collect();
        let replacement = replacement.map(ToString::to_string).unwrap_or_default();
        let marked_range = if body.hasMarkedText() {
            Some(body.markedRange())
        } else {
            None
        };
        if marked_range.is_some() || self.ivars().pending_editor_composition.borrow().is_some() {
            let composition = self.ivars().pending_editor_composition.borrow().clone();
            let next = accumulate_marked_editor_intent(
                composition,
                &old_view_text,
                &old_semantic_text,
                range,
                &replacement,
                marked_range.unwrap_or(range),
                covered_attachments,
            );
            match next {
                Ok(next) => {
                    *self.ivars().pending_editor_composition.borrow_mut() = Some(next);
                    true
                }
                Err(_) => {
                    self.clear_pending_editor_intent();
                    false
                }
            }
        } else {
            if !self.ivars().pending_editor_intents.borrow().is_empty() {
                self.clear_pending_editor_intent();
                return false;
            }
            let intent = PendingEditorIntent {
                range,
                replacement,
                old_view_text,
                old_semantic_text,
                covered_attachments,
            };
            if preflight_pending_editor_intent(&intent).is_err() {
                self.clear_pending_editor_intent();
                return false;
            }
            self.ivars()
                .pending_editor_intents
                .borrow_mut()
                .push(intent);
            true
        }
    }

    fn take_pending_editor_intent(&self) -> (Option<PendingEditorIntent>, bool) {
        let intents = std::mem::take(&mut *self.ivars().pending_editor_intents.borrow_mut());
        let invalid = *self.ivars().pending_editor_intent_invalid.borrow();
        *self.ivars().pending_editor_intent_invalid.borrow_mut() = false;
        let exactly_one = intents.len() == 1;
        let intent = exactly_one.then(|| {
            intents
                .into_iter()
                .next()
                .expect("one pending editor intent")
        });
        (intent, invalid || !exactly_one)
    }

    fn install_rendered_document(
        &self,
        body: &NSTextView,
        rendered: &RenderedDocument,
        selection: NSRange,
    ) {
        self.clear_pending_editor_intent();
        if let Some(storage) = unsafe { body.textStorage() } {
            storage.setAttributedString(&rendered.attributed);
        }
        *self.ivars().projection_attachments.borrow_mut() = rendered.attachments.clone();
        *self.ivars().projection_empty_carriers.borrow_mut() =
            rendered.empty_block_carriers.clone();
        let max_length = rendered.attributed.string().length();
        let location = selection.location.min(max_length);
        let length = selection.length.min(max_length.saturating_sub(location));
        let exact_empty_carrier =
            exact_empty_block_carrier(rendered, NSRange::new(location, length));
        if let Some(carrier) = exact_empty_carrier {
            body.setDefaultParagraphStyle(Some(&carrier.paragraph));
            let typing = body.typingAttributes();
            let mutable = typing.mutableCopy();
            unsafe {
                mutable.insert(NSParagraphStyleAttributeName, &carrier.paragraph);
                body.setTypingAttributes(&mutable);
            }
        } else {
            let paragraph = if max_length > 0 {
                let probe = location.min(max_length - 1);
                let source: &NSAttributedString = &rendered.attributed;
                unsafe {
                    source
                        .attribute_atIndex_effectiveRange(
                            NSParagraphStyleAttributeName,
                            probe,
                            null_mut(),
                        )
                        .and_then(|value| value.downcast::<NSParagraphStyle>().ok())
                }
            } else {
                None
            };
            if let Some(paragraph) = paragraph {
                body.setDefaultParagraphStyle(Some(&paragraph));
                let typing = body.typingAttributes();
                let mutable = typing.mutableCopy();
                unsafe {
                    mutable.insert(NSParagraphStyleAttributeName, &paragraph);
                    body.setTypingAttributes(&mutable);
                }
            }
        }
        body.setSelectedRange(NSRange::new(location, length));
    }

    fn refresh_body_from_session(&self) {
        let Some(body) = self.ivars().body_view.get() else {
            return;
        };
        let selection = body.selectedRange();
        let rendered = {
            let session_guard = self.ivars().editor_session.borrow();
            let Some(session) = session_guard.as_ref() else {
                return;
            };
            render_session(
                session,
                |resource_id| {
                    self.ivars()
                        .repository
                        .get_resource(resource_id)
                        .ok()
                        .flatten()
                },
                text_container_available_width(body),
            )
        };
        let failures = rendered.missing_resources;
        let previous_loading_guard = *self.ivars().loading_guard.borrow();
        *self.ivars().loading_guard.borrow_mut() = true;
        self.install_rendered_document(body, &rendered, selection);
        *self.ivars().loading_guard.borrow_mut() = previous_loading_guard;
        if failures == 0 {
            self.set_save_status("已保存", false);
        } else {
            self.set_save_status("部分图片未恢复，已保留引用", true);
        }
        self.update_formatting_buttons();
    }

    fn restore_body_from_session_after_rejected_edit(&self) {
        self.refresh_body_from_session();
        self.clear_pending_editor_intent();
    }

    fn native_undo_state(&self) -> (bool, bool) {
        self.ivars()
            .editor_session
            .borrow()
            .as_ref()
            .map(|session| (session.can_undo(), session.can_redo()))
            .unwrap_or((false, false))
    }

    fn reapply_empty_carrier_for_selection(&self) {
        let Some(body) = self.ivars().body_view.get() else {
            return;
        };
        let selection = body.selectedRange();
        if selection.length != 0 {
            return;
        }
        let carrier = self
            .ivars()
            .projection_empty_carriers
            .borrow()
            .iter()
            .find(|carrier| carrier.addressable_offset == selection.location)
            .cloned();
        if let Some(carrier) = carrier {
            body.setDefaultParagraphStyle(Some(&carrier.paragraph));
            let typing = body.typingAttributes();
            let mutable = typing.mutableCopy();
            unsafe {
                mutable.insert(NSParagraphStyleAttributeName, &carrier.paragraph);
                body.setTypingAttributes(&mutable);
            }
            return;
        }

        let Some(storage) = (unsafe { body.textStorage() }) else {
            return;
        };
        let length = storage.length();
        if length == 0 {
            return;
        }
        let source: &NSAttributedString = &storage;
        let probe = selection.location.min(length - 1);
        let Some(paragraph) = (unsafe {
            source
                .attribute_atIndex_effectiveRange(NSParagraphStyleAttributeName, probe, null_mut())
                .and_then(|value| value.downcast::<NSParagraphStyle>().ok())
        }) else {
            return;
        };
        body.setDefaultParagraphStyle(Some(&paragraph));
        let typing = body.typingAttributes();
        let mutable = typing.mutableCopy();
        unsafe {
            mutable.insert(NSParagraphStyleAttributeName, &paragraph);
            body.setTypingAttributes(&mutable);
        }
    }

    #[allow(deprecated)]
    fn sync_editor_session_from_view(&self) -> EditorSessionSyncResult {
        if *self.ivars().loading_guard.borrow() {
            self.clear_pending_editor_intent();
            return EditorSessionSyncResult::Noop;
        }
        let Some(body) = self.ivars().body_view.get() else {
            self.clear_pending_editor_intent();
            return EditorSessionSyncResult::Rejected;
        };
        if body.hasMarkedText() {
            self.clear_pending_editor_intent();
            return EditorSessionSyncResult::Noop;
        }
        let Some(storage) = (unsafe { body.textStorage() }) else {
            self.clear_pending_editor_intent();
            return EditorSessionSyncResult::Rejected;
        };
        let source: &NSAttributedString = &storage;
        let new_text = source.string().to_string();
        let composition = self.ivars().pending_editor_composition.borrow().clone();
        let (decision, invalid) = if let Some(composition) = composition {
            self.clear_pending_editor_intent();
            (finish_marked_editor_intent(&composition, &new_text), false)
        } else {
            let (pending, invalid) = self.take_pending_editor_intent();
            let Some(intent) = pending else {
                // A live change is accepted only when AppKit gave us exactly one
                // pre-mutation intent. The old/new string diff remains a legacy
                // test and migration helper, never a live writeback path.
                return EditorSessionSyncResult::Rejected;
            };
            if invalid {
                return EditorSessionSyncResult::Rejected;
            }
            (decide_pending_editor_intent(&intent, &new_text), false)
        };
        if invalid {
            // A live change is accepted only when AppKit gave us exactly one
            // pre-mutation intent. The old/new string diff remains a legacy
            // test and migration helper, never a live writeback path.
            return EditorSessionSyncResult::Rejected;
        }
        let mut next_attachments = self.ivars().projection_attachments.borrow().clone();
        let result = match decision {
            PendingIntentDecision::Noop => return EditorSessionSyncResult::Noop,
            PendingIntentDecision::Reject => return EditorSessionSyncResult::Rejected,
            PendingIntentDecision::ApplyText { range, replacement } => {
                let replacement_length = NSString::from_str(&replacement).length();
                if !adjust_projection_attachments(
                    &mut next_attachments,
                    range,
                    replacement_length,
                    None,
                ) {
                    return EditorSessionSyncResult::Rejected;
                }
                let mut session_guard = self.ivars().editor_session.borrow_mut();
                let Some(session) = session_guard.as_mut() else {
                    return EditorSessionSyncResult::Rejected;
                };
                apply_committed_text_delta(session, range, &replacement).is_ok()
            }
            PendingIntentDecision::DeleteImage { range, resource_id } => {
                if !adjust_projection_attachments(
                    &mut next_attachments,
                    range,
                    0,
                    Some(&resource_id),
                ) {
                    return EditorSessionSyncResult::Rejected;
                }
                let mut session_guard = self.ivars().editor_session.borrow_mut();
                let Some(session) = session_guard.as_mut() else {
                    return EditorSessionSyncResult::Rejected;
                };
                delete_image_anchor_if_identity(session, range, Some(&resource_id)).is_ok()
            }
        };
        if result {
            *self.ivars().projection_attachments.borrow_mut() = next_attachments;
            EditorSessionSyncResult::Applied
        } else {
            // Semantic application is intentionally attempted only after all
            // range and projection checks pass. A failed live command is
            // therefore fail-closed and cannot save the stale model as if it
            // had accepted the view mutation.
            EditorSessionSyncResult::Rejected
        }
    }

    #[allow(deprecated)]
    fn run_native_undo_smoke(&self, image_path: &Path) {
        let Ok(bytes) = read_regular_image_file(image_path) else {
            eprintln!("native undo smoke image could not be read");
            return;
        };
        if !valid_image_bytes_for_mime(&bytes, "image/png") {
            eprintln!("native undo smoke image is not a PNG");
            return;
        }
        let Some(body) = self.ivars().body_view.get() else {
            eprintln!("native undo smoke body view unavailable");
            return;
        };
        body.setString(ns_string!("seed"));
        body.setSelectedRange(NSRange::new(4, 0));
        unsafe {
            body.insertText(&NSString::from_str("A") as &AnyObject);
            body.insertText(&NSString::from_str("B") as &AnyObject);
        }
        let sender = NSObject::new();
        self.undo_text(sel!(undoText:), &sender);
        self.save_current_note();

        let before_string = body.string().to_string();
        let before_selection = body.selectedRange();
        let (before_can_undo, before_can_redo) = self.native_undo_state();
        let smoke_resource = joplin_lite_native::core::StoredResource {
            id: "0123456789abcdef0123456789abcdef".into(),
            sha256: "0".repeat(64),
            size: bytes.len(),
            title: "smoke.png".into(),
            mime: "image/png".into(),
            file_extension: "png".into(),
            path: PathBuf::new(),
            bytes: bytes.clone(),
        };
        let Some(inline) = inline_attachment(&smoke_resource) else {
            eprintln!("native undo smoke attachment construction failed");
            return;
        };
        let failed = commit_live_image_insert(body, before_selection, &inline, || false);
        let after_failure_string = body.string().to_string();
        let after_failure_selection = body.selectedRange();
        let (after_failure_can_undo, after_failure_can_redo) = self.native_undo_state();
        println!(
            "nativeUndoSmoke failure result={} unchanged={} selection_unchanged={} undo_unchanged={} redo_unchanged={}",
            failed,
            before_string == after_failure_string,
            before_selection == after_failure_selection,
            before_can_undo == after_failure_can_undo,
            before_can_redo == after_failure_can_redo,
        );

        if bytes.len() > 32 {
            let truncated = bytes[..32].to_vec();
            let truncated_result = self.insert_image_data(&truncated, "truncated.png", "image/png");
            let after_truncated_string = body.string().to_string();
            let after_truncated_selection = body.selectedRange();
            let (after_truncated_can_undo, after_truncated_can_redo) = self.native_undo_state();
            println!(
                "nativeUndoSmoke truncated result={} unchanged={} selection_unchanged={} undo_unchanged={} redo_unchanged={}",
                truncated_result,
                before_string == after_truncated_string,
                before_selection == after_truncated_selection,
                before_can_undo == after_truncated_can_undo,
                before_can_redo == after_truncated_can_redo,
            );
        }

        let inserted = self.insert_image_data(&bytes, "smoke.png", "image/png");
        let inserted_has_attachment = body.string().to_string().contains('\u{fffc}');
        let sender = NSObject::new();
        self.undo_text(sel!(undoText:), &sender);
        let undone_has_attachment = body.string().to_string().contains('\u{fffc}');
        self.redo_text(sel!(redoText:), &sender);
        let redone_has_attachment = body.string().to_string().contains('\u{fffc}');
        let saved = self.save_current_note();
        println!(
            "nativeUndoSmoke success inserted={} attachment_after_insert={} attachment_after_cmd_z={} attachment_after_shift_cmd_z={} saved={}",
            inserted, inserted_has_attachment, undone_has_attachment, redone_has_attachment, saved,
        );
    }

    #[allow(deprecated)]
    fn run_resize_smoke(&self, image_path: &Path) {
        let Ok(bytes) = read_regular_image_file(image_path) else {
            eprintln!("resize smoke image could not be read");
            return;
        };
        if !valid_image_bytes_for_mime(&bytes, "image/png") {
            eprintln!("resize smoke image is not a PNG");
            return;
        }
        let Some(body) = self.ivars().body_view.get() else {
            eprintln!("resize smoke body view unavailable");
            return;
        };
        let resource = joplin_lite_native::core::StoredResource {
            id: "0123456789abcdef0123456789abcdef".into(),
            sha256: "0".repeat(64),
            size: bytes.len(),
            title: "resize.png".into(),
            mime: "image/png".into(),
            file_extension: "png".into(),
            path: PathBuf::new(),
            bytes,
        };
        let Some(inline) = inline_attachment(&resource) else {
            eprintln!("resize smoke attachment construction failed");
            return;
        };
        let previous_loading_guard = *self.ivars().loading_guard.borrow();
        *self.ivars().loading_guard.borrow_mut() = true;
        body.setString(ns_string!(""));
        self.layout_content(1400.0, 720.0);
        body.setSelectedRange(NSRange::new(0, 0));
        insert_inline_attachment(body, &inline);
        body.setSelectedRange(NSRange::new(1, 0));
        let before = first_attachment_bounds(body);
        let before_line = first_attachment_line_metrics(body);
        let before_selection = body.selectedRange();
        let before_undo = Some(self.native_undo_state());
        if let Some(window) = self.ivars().window.get() {
            window.setContentSize(NSSize::new(860.0, 560.0));
            window.displayIfNeeded();
        } else {
            self.layout_content(860.0, 560.0);
        }
        let after = first_attachment_bounds(body);
        let after_line = first_attachment_line_metrics(body);
        let after_selection = body.selectedRange();
        let after_undo = Some(self.native_undo_state());
        *self.ivars().loading_guard.borrow_mut() = previous_loading_guard;
        let aspect_preserved = before.zip(after).is_some_and(|(before, after)| {
            (before.size.width * after.size.height - after.size.width * before.size.height).abs()
                < 0.01
        });
        println!(
            "resizeSmoke before_width={:?} after_width={:?} after_height={:?} before_glyph_height={:?} after_glyph_height={:?} before_line_height={:?} after_line_height={:?} before_used_height={:?} after_used_height={:?} width_ok={} height_ok={} layout_height_ok={} layout_updated={} aspect_preserved={} selection_unchanged={} undo_unchanged={}",
            before.map(|bounds| bounds.size.width),
            after.map(|bounds| bounds.size.width),
            after.map(|bounds| bounds.size.height),
            before_line.map(|(glyph, _, _)| glyph.size.height),
            after_line.map(|(glyph, _, _)| glyph.size.height),
            before_line.map(|(_, line, _)| line.size.height),
            after_line.map(|(_, line, _)| line.size.height),
            before_line.map(|(_, _, used)| used.size.height),
            after_line.map(|(_, _, used)| used.size.height),
            after.is_some_and(|bounds| bounds.size.width <= 520.0),
            after.is_some_and(|bounds| bounds.size.height <= 640.0),
            after_line.is_some_and(|(glyph, line, used)| {
                glyph.size.height <= 520.0 && line.size.height <= 520.0 && used.size.height <= 520.0
            }),
            before_line.map(|(glyph, line, used)| {
                (glyph.size.height, line.size.height, used.size.height)
            }) != after_line.map(|(glyph, line, used)| {
                (glyph.size.height, line.size.height, used.size.height)
            }),
            aspect_preserved,
            before_selection == after_selection,
            before_undo == after_undo,
        );
    }

    fn make_format_button(
        mtm: MainThreadMarker,
        target: &AppDelegate,
        title: &str,
        action: Sel,
    ) -> Retained<NSButton> {
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(target),
                Some(action),
                mtm,
            )
        };
        button.setButtonType(NSButtonType::PushOnPushOff);
        button.setBezelStyle(NSBezelStyle::Toolbar);
        button
    }

    fn install_menu(application: &NSApplication, target: &AppDelegate, mtm: MainThreadMarker) {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("主菜单"));
        let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("应用"));
        let quit_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("退出"),
                Some(sel!(terminate:)),
                ns_string!("q"),
            )
        };
        unsafe {
            quit_item.setTarget(None);
        }
        quit_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        app_menu.addItem(&quit_item);
        let app_menu_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("应用"),
                None,
                ns_string!(""),
            )
        };
        app_menu_item.setSubmenu(Some(&app_menu));
        menu.addItem(&app_menu_item);

        let file_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("文件"));
        let new_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("新建笔记"),
                Some(sel!(newNote:)),
                ns_string!("n"),
            )
        };
        unsafe {
            new_item.setTarget(Some(target));
        }
        new_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        file_menu.addItem(&new_item);
        let file_menu_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("文件"),
                None,
                ns_string!(""),
            )
        };
        file_menu_item.setSubmenu(Some(&file_menu));
        menu.addItem(&file_menu_item);

        let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("编辑"));
        let undo_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("撤销"),
                Some(sel!(undoText:)),
                ns_string!("z"),
            )
        };
        let redo_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("重做"),
                Some(sel!(redoText:)),
                ns_string!("z"),
            )
        };
        unsafe {
            undo_item.setTarget(Some(target));
            redo_item.setTarget(Some(target));
        }
        undo_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        redo_item.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
        );
        edit_menu.addItem(&undo_item);
        edit_menu.addItem(&redo_item);
        edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
        for (title, key, action) in [
            ("剪切", "x", sel!(cut:)),
            ("拷贝", "c", sel!(copy:)),
            ("粘贴", "v", sel!(paste:)),
            ("全选", "a", sel!(selectAll:)),
        ] {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(key),
                )
            };
            unsafe {
                item.setTarget(if key == "v" { Some(target) } else { None });
            }
            item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
            edit_menu.addItem(&item);
        }
        let edit_menu_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("编辑"),
                None,
                ns_string!(""),
            )
        };
        edit_menu_item.setSubmenu(Some(&edit_menu));
        menu.addItem(&edit_menu_item);

        let format_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("格式"));
        for (title, key, action) in [
            ("粗体", "b", sel!(toggleBoldText:)),
            ("斜体", "i", sel!(toggleItalicText:)),
            ("下划线", "u", sel!(toggleUnderlineText:)),
        ] {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(key),
                )
            };
            unsafe {
                item.setTarget(Some(target));
            }
            item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
            format_menu.addItem(&item);
        }
        let format_menu_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("格式"),
                None,
                ns_string!(""),
            )
        };
        format_menu_item.setSubmenu(Some(&format_menu));
        menu.addItem(&format_menu_item);
        application.setMainMenu(Some(&menu));
    }

    fn layout_content(&self, width: f64, height: f64) {
        let layout = content_layout(width, height);
        if let Some(separator) = self.ivars().sidebar_separator.get() {
            separator.setFrame(NSRect::new(
                NSPoint::new(layout.sidebar.width - 1.0, 0.0),
                NSSize::new(1.0, layout.sidebar.height),
            ));
        }
        if let Some(background) = self.ivars().sidebar_background.get() {
            background.setFrame(layout.sidebar.ns_rect());
        }
        if let Some(list_scroll) = self.ivars().list_scroll.get() {
            list_scroll.setFrame(layout.list.ns_rect());
        }
        if let Some(list_stack) = self.ivars().list_stack.get() {
            // Keep the document view at least as tall as the viewport, then
            // position each row explicitly at the top of the reading rail.
            let stack_height =
                (self.ivars().notes.borrow().len() as f64 * 64.0 + 24.0).max(layout.list.height);
            list_stack.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(layout.list.width, stack_height),
            ));
            let row_width = (layout.list.width - 24.0).max(180.0);
            for (index, row) in self.ivars().note_rows.borrow().iter().enumerate() {
                row.setFrame(NSRect::new(
                    NSPoint::new(12.0, stack_height - 12.0 - ((index + 1) as f64 * 64.0)),
                    NSSize::new(row_width, 60.0),
                ));
            }
            for button in self.ivars().note_buttons.borrow().iter() {
                button.setFrame(NSRect::new(
                    NSPoint::new(8.0, 0.0),
                    NSSize::new((row_width - 16.0).max(164.0), 60.0),
                ));
            }
        }
        if let Some(label) = self.ivars().list_empty_label.get() {
            label.setFrame(NSRect::new(
                NSPoint::new(12.0, layout.list.y + (layout.list.height - 32.0) * 0.5),
                NSSize::new((layout.sidebar.width - 24.0).max(200.0), 32.0),
            ));
        }
        if let Some(button) = self.ivars().new_button.get() {
            button.setFrame(NSRect::new(
                NSPoint::new(16.0, height - 56.0),
                NSSize::new(108.0, 30.0),
            ));
        }
        if let Some(search) = self.ivars().search_field.get() {
            let x = 132.0;
            let search_width = (layout.sidebar.width - x - 14.0).max(108.0);
            search.setFrame(NSRect::new(
                NSPoint::new(x, height - 56.0),
                NSSize::new(search_width, 30.0),
            ));
        }
        if let Some(title) = self.ivars().title_field.get() {
            title.setFrame(layout.title.ns_rect());
        }
        for (button, x) in [
            (self.ivars().bold_button.get(), layout.toolbar.x),
            (self.ivars().italic_button.get(), layout.toolbar.x + 38.0),
            (self.ivars().underline_button.get(), layout.toolbar.x + 76.0),
        ] {
            if let Some(button) = button {
                button.setFrame(NSRect::new(
                    NSPoint::new(x, layout.toolbar.y),
                    NSSize::new(34.0, layout.toolbar.height),
                ));
            }
        }
        if let Some(button) = self.ivars().clear_button.get() {
            button.setFrame(NSRect::new(
                NSPoint::new(layout.toolbar.x + 114.0, layout.toolbar.y),
                NSSize::new(78.0, layout.toolbar.height),
            ));
        }
        if let Some(scroll) = self.ivars().body_scroll.get() {
            scroll.setFrame(layout.body.ns_rect());
        }
        if let Some(body) = self.ivars().body_view.get() {
            body.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(layout.body.width, layout.body.height),
            ));
            body.setMinSize(NSSize::new(layout.body.width, layout.body.height));
            body.setMaxSize(NSSize::new(layout.body.width, f64::MAX));
            let previous_loading_guard = *self.ivars().loading_guard.borrow();
            *self.ivars().loading_guard.borrow_mut() = true;
            resize_inline_attachments(body);
            *self.ivars().loading_guard.borrow_mut() = previous_loading_guard;
        }
        if let Some(button) = self.ivars().delete_button.get() {
            button.setFrame(layout.delete.ns_rect());
        }
        if let Some(status) = self.ivars().save_status.get() {
            status.setFrame(layout.status.ns_rect());
        }
        if let Some(label) = self.ivars().editor_empty_label.get() {
            label.setFrame(layout.empty_editor.ns_rect());
        }
    }

    #[allow(deprecated)]
    fn apply_format(&self, format: TextFormat) {
        let Some(body) = self.ivars().body_view.get() else {
            return;
        };
        if body.hasMarkedText() {
            return;
        }
        let range = body.selectedRange();
        let mut semantic_changed = false;
        if range.length > 0 {
            let command = match format {
                TextFormat::Bold => Some(InlineCommand::Bold),
                TextFormat::Italic => Some(InlineCommand::Italic),
                TextFormat::Underline => Some(InlineCommand::Underline),
                TextFormat::Clear => Some(InlineCommand::Clear),
            };
            if let Some(command) = command
                && let Some(session) = self.ivars().editor_session.borrow_mut().as_mut()
            {
                semantic_changed = apply_inline_command(session, range, command).is_ok();
            }
        }
        if range.length > 0 && semantic_changed {
            self.refresh_body_from_session();
            body.setSelectedRange(range);
            if let Some(window) = self.ivars().window.get() {
                window.makeFirstResponder(Some(body));
            }
            self.update_formatting_buttons();
            self.save_current_note();
            return;
        }
        let active = if range.length == 0 {
            self.typing_style_active(body, format)
        } else {
            self.selection_style_active(body, range, format)
        };
        let decision = format_decision(format, active);
        let font_key = unsafe { NSFontAttributeName };
        let underline_key = unsafe { NSUnderlineStyleAttributeName };
        if range.length == 0 {
            self.apply_typing_format(body, format, decision);
        } else if let Some(storage) = unsafe { body.textStorage() } {
            match (format, decision) {
                (TextFormat::Bold, FormatDecision::Add) => {
                    storage.applyFontTraits_range(NSFontTraitMask::BoldFontMask, range);
                }
                (TextFormat::Bold, FormatDecision::Remove) => {
                    storage.applyFontTraits_range(NSFontTraitMask::UnboldFontMask, range);
                }
                (TextFormat::Italic, FormatDecision::Add) => {
                    storage.applyFontTraits_range(NSFontTraitMask::ItalicFontMask, range);
                }
                (TextFormat::Italic, FormatDecision::Remove) => {
                    storage.applyFontTraits_range(NSFontTraitMask::UnitalicFontMask, range);
                }
                (TextFormat::Underline, FormatDecision::Add) => unsafe {
                    let value = NSNumber::numberWithInteger(NSUnderlineStyle::Single.0);
                    storage.addAttribute_value_range(underline_key, &value, range);
                },
                (TextFormat::Underline, FormatDecision::Remove) => {
                    storage.removeAttribute_range(underline_key, range);
                }
                (TextFormat::Clear, FormatDecision::Clear) => unsafe {
                    for key in [
                        NSFontAttributeName,
                        NSForegroundColorAttributeName,
                        NSBackgroundColorAttributeName,
                        NSUnderlineStyleAttributeName,
                        NSStrikethroughStyleAttributeName,
                        NSObliquenessAttributeName,
                        NSStrokeColorAttributeName,
                        NSStrokeWidthAttributeName,
                        NSShadowAttributeName,
                        NSKernAttributeName,
                        NSBaselineOffsetAttributeName,
                    ] {
                        storage.removeAttribute_range(key, range);
                    }
                    let font = NSFont::systemFontOfSize(17.0);
                    storage.addAttribute_value_range(font_key, &font, range);
                },
                (TextFormat::Clear, FormatDecision::Add | FormatDecision::Remove) => {}
                (_, FormatDecision::Clear) => {}
            }
        }
        body.setSelectedRange(range);
        if let Some(window) = self.ivars().window.get() {
            window.makeFirstResponder(Some(body));
        }
        self.update_formatting_buttons();
        self.save_current_note();
    }

    fn selection_style_active(
        &self,
        body: &NSTextView,
        range: NSRange,
        format: TextFormat,
    ) -> bool {
        if format == TextFormat::Clear || range.length == 0 {
            return false;
        }
        let Some(storage) = (unsafe { body.textStorage() }) else {
            return false;
        };
        let font_key = unsafe { NSFontAttributeName };
        let underline_key = unsafe { NSUnderlineStyleAttributeName };
        let manager = objc2_app_kit::NSFontManager::sharedFontManager(self.mtm());
        for index in range.location..range.location + range.length {
            let active = match format {
                TextFormat::Bold | TextFormat::Italic => unsafe {
                    storage
                        .attribute_atIndex_effectiveRange(font_key, index, null_mut())
                        .and_then(|value| value.downcast::<NSFont>().ok())
                        .map(|font| {
                            let trait_mask = if format == TextFormat::Bold {
                                NSFontTraitMask::BoldFontMask
                            } else {
                                NSFontTraitMask::ItalicFontMask
                            };
                            manager.traitsOfFont(&font).contains(trait_mask)
                        })
                        .unwrap_or(false)
                },
                TextFormat::Underline => unsafe {
                    storage
                        .attribute_atIndex_effectiveRange(underline_key, index, null_mut())
                        .and_then(|value| value.downcast::<NSNumber>().ok())
                        .map(|number| number.intValue() != 0)
                        .unwrap_or(false)
                },
                TextFormat::Clear => false,
            };
            if !active {
                return false;
            }
        }
        true
    }

    fn typing_style_active(&self, body: &NSTextView, format: TextFormat) -> bool {
        let attributes = body.typingAttributes();
        match format {
            TextFormat::Bold | TextFormat::Italic => {
                let font_key = unsafe { NSFontAttributeName };
                let trait_mask = if format == TextFormat::Bold {
                    NSFontTraitMask::BoldFontMask
                } else {
                    NSFontTraitMask::ItalicFontMask
                };
                attributes
                    .objectForKey(font_key)
                    .and_then(|value| value.downcast::<NSFont>().ok())
                    .map(|font| {
                        objc2_app_kit::NSFontManager::sharedFontManager(self.mtm())
                            .traitsOfFont(&font)
                            .contains(trait_mask)
                    })
                    .unwrap_or(false)
            }
            TextFormat::Underline => {
                let key = unsafe { NSUnderlineStyleAttributeName };
                attributes
                    .objectForKey(key)
                    .and_then(|value| value.downcast::<NSNumber>().ok())
                    .map(|number| number.intValue() != 0)
                    .unwrap_or(false)
            }
            TextFormat::Clear => false,
        }
    }

    #[allow(deprecated)]
    fn apply_typing_format(&self, body: &NSTextView, format: TextFormat, decision: FormatDecision) {
        let attributes = body.typingAttributes();
        let mutable = attributes.mutableCopy();
        let font_key = unsafe { NSFontAttributeName };
        let underline_key = unsafe { NSUnderlineStyleAttributeName };
        match (format, decision) {
            (TextFormat::Bold, FormatDecision::Add)
            | (TextFormat::Bold, FormatDecision::Remove)
            | (TextFormat::Italic, FormatDecision::Add)
            | (TextFormat::Italic, FormatDecision::Remove) => {
                let font = attributes
                    .objectForKey(font_key)
                    .and_then(|value| value.downcast::<NSFont>().ok())
                    .unwrap_or_else(|| NSFont::systemFontOfSize(17.0));
                let manager = objc2_app_kit::NSFontManager::sharedFontManager(self.mtm());
                let trait_mask = if format == TextFormat::Bold {
                    NSFontTraitMask::BoldFontMask
                } else {
                    NSFontTraitMask::ItalicFontMask
                };
                let converted = match typing_trait_operation(format, decision) {
                    FontTraitOperation::Have => manager.convertFont_toHaveTrait(&font, trait_mask),
                    FontTraitOperation::NotHave => {
                        manager.convertFont_toNotHaveTrait(&font, trait_mask)
                    }
                    FontTraitOperation::None => font.clone(),
                };
                mutable.insert(font_key, &converted);
            }
            (TextFormat::Underline, FormatDecision::Add) => {
                let value = NSNumber::numberWithInteger(NSUnderlineStyle::Single.0);
                mutable.insert(underline_key, &value);
            }
            (TextFormat::Underline, FormatDecision::Remove) => {
                mutable.removeObjectForKey(underline_key);
            }
            (TextFormat::Clear, FormatDecision::Clear) => {
                for key in [
                    font_key,
                    unsafe { NSForegroundColorAttributeName },
                    unsafe { NSBackgroundColorAttributeName },
                    underline_key,
                    unsafe { NSStrikethroughStyleAttributeName },
                    unsafe { NSObliquenessAttributeName },
                    unsafe { NSStrokeColorAttributeName },
                    unsafe { NSStrokeWidthAttributeName },
                    unsafe { NSShadowAttributeName },
                    unsafe { NSKernAttributeName },
                    unsafe { NSBaselineOffsetAttributeName },
                ] {
                    mutable.removeObjectForKey(key);
                }
                let font = NSFont::systemFontOfSize(17.0);
                mutable.insert(font_key, &font);
            }
            (TextFormat::Clear, FormatDecision::Add | FormatDecision::Remove) => {}
            (_, FormatDecision::Clear) => {}
        }
        unsafe {
            body.setTypingAttributes(&mutable);
        }
    }

    fn update_formatting_buttons(&self) {
        let Some(body) = self.ivars().body_view.get() else {
            return;
        };
        let range = body.selectedRange();
        let active = |format| {
            if range.length == 0 {
                self.typing_style_active(body, format)
            } else {
                self.selection_style_active(body, range, format)
            }
        };
        if let Some(button) = self.ivars().bold_button.get() {
            button.setState(if active(TextFormat::Bold) {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
        if let Some(button) = self.ivars().italic_button.get() {
            button.setState(if active(TextFormat::Italic) {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
        if let Some(button) = self.ivars().underline_button.get() {
            button.setState(if active(TextFormat::Underline) {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }

    fn handle_paste(&self, _sender: Option<&NSObject>) {
        let Some(window) = self.ivars().window.get() else {
            return;
        };
        let first_responder = window.firstResponder();
        let body = self.ivars().body_view.get();
        let body_is_first_responder = first_responder.as_ref().is_some_and(|first| {
            body.is_some_and(|body| {
                Retained::<NSResponder>::as_ptr(first)
                    == Retained::<NSTextView>::as_ptr(body) as *const NSResponder
            })
        });
        let route = paste_route(
            body_is_first_responder,
            self.ivars().current_note_id.borrow().is_some(),
        );
        if route == PasteRoute::NativeResponder {
            if body_is_first_responder {
                if let Some(body) = body {
                    unsafe { body.paste(None) };
                }
            } else if let Some(first_responder) = first_responder {
                unsafe {
                    first_responder.tryToPerform_with(sel!(paste:), None);
                }
            }
            return;
        }
        let Some(body) = body else {
            return;
        };
        match self.read_pasteboard_image() {
            PasteboardImage::NotImage => unsafe { body.paste(None) },
            PasteboardImage::Rejected(message) => self.set_save_status(message, true),
            PasteboardImage::Data { bytes, title, mime } => {
                let _ = self.insert_image_data(&bytes, &title, &mime);
            }
        }
    }

    fn accept_drag(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
        self.ivars().current_note_id.borrow().is_some()
            && read_drag_pasteboard(&sender.draggingPasteboard()).is_ok()
    }

    fn perform_drag(
        &self,
        body: &BodyTextView,
        sender: &ProtocolObject<dyn NSDraggingInfo>,
    ) -> bool {
        if self.ivars().current_note_id.borrow().is_none() {
            return false;
        }
        let Ok(PasteboardImage::Data { bytes, title, mime }) =
            read_drag_pasteboard(&sender.draggingPasteboard())
        else {
            return false;
        };
        let Some(attributed) = (unsafe { body.textStorage() }).map(|storage| {
            let source: &NSAttributedString = &storage;
            source.mutableCopy()
        }) else {
            return false;
        };
        let snapshot = EditorSnapshot {
            attributed,
            selection: body.selectedRange(),
        };
        let point = body.convertPoint_fromView(sender.draggingLocation(), None);
        let character_index = body.characterIndexForInsertionAtPoint(point);
        self.insert_image_data_from_snapshot(
            &bytes,
            &title,
            &mime,
            body,
            snapshot,
            NSRange::new(character_index, 0),
        )
    }

    fn read_pasteboard_image(&self) -> PasteboardImage {
        let pasteboard = NSPasteboard::generalPasteboard();
        read_pasteboard_image_from(&pasteboard)
    }
}

fn read_pasteboard_image_from(pasteboard: &NSPasteboard) -> PasteboardImage {
    let png_type = unsafe { NSPasteboardTypePNG };
    let tiff_type = unsafe { NSPasteboardTypeTIFF };
    let file_url_type = unsafe { NSPasteboardTypeFileURL };
    if pasteboard.types().as_ref().is_some_and(|types| {
        types.iter().any(|item| {
            let item: &NSString = item.as_ref();
            item == file_url_type
        })
    }) {
        return read_file_url_pasteboard_image(pasteboard);
    }
    if let Some(data) = pasteboard.dataForType(png_type) {
        return normalize_paste_image(data.to_vec(), "clipboard.png", "image/png");
    }
    if let Some(data) = pasteboard.dataForType(tiff_type) {
        return normalize_tiff(data.to_vec(), "clipboard.png");
    }
    let jpeg_type = NSString::from_str("public.jpeg");
    if let Some(data) = pasteboard.dataForType(&jpeg_type) {
        return normalize_paste_image(data.to_vec(), "clipboard.jpg", "image/jpeg");
    }
    PasteboardImage::NotImage
}

fn read_file_url_pasteboard_image(pasteboard: &NSPasteboard) -> PasteboardImage {
    let file_url_type = unsafe { NSPasteboardTypeFileURL };
    if pasteboard.types().as_ref().is_some_and(|types| {
        types.iter().any(|item| {
            let item: &NSString = item.as_ref();
            is_promised_pasteboard_type(item.to_string().as_str())
        })
    }) {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    }
    let Some(items) = pasteboard.pasteboardItems() else {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    };
    if items.len() != 1 {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    }
    let Some(url_text) = pasteboard.stringForType(file_url_type) else {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    };
    let Some(url) = NSURL::initWithString(NSURL::alloc(), &url_text) else {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    };
    let host = url.host().map(|host| host.to_string());
    if !url.isFileURL() || !is_local_file_url_host(host.as_deref()) {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    }
    let Some(path) = url.path() else {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    };
    let path = PathBuf::from(path.to_string());
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let mime = match extension.as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => return PasteboardImage::Rejected("图片未插入：格式不支持"),
    };
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("图片")
        .to_owned();
    let bytes = match read_regular_image_file(&path) {
        Ok(bytes) => bytes,
        Err(PasteFileError::TooLarge) => {
            return PasteboardImage::Rejected("图片未插入：超过 10 MB");
        }
        Err(PasteFileError::Invalid) => return PasteboardImage::Rejected("图片未插入：格式不支持"),
    };
    if !valid_image_bytes_for_mime(&bytes, mime) {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    }
    PasteboardImage::Data {
        bytes,
        title,
        mime: mime.to_owned(),
    }
}

impl AppDelegate {
    fn insert_image_data(&self, bytes: &[u8], title: &str, mime: &str) -> bool {
        self.insert_image_data_with_save(bytes, title, mime)
    }

    fn insert_image_data_with_save(&self, bytes: &[u8], title: &str, mime: &str) -> bool {
        if bytes.len() > MAX_IMAGE_BYTES {
            self.set_save_status("图片未插入：超过 10 MB", true);
            return false;
        }
        if !matches!(mime, "image/png" | "image/jpeg") || !valid_image_bytes_for_mime(bytes, mime) {
            self.set_save_status("图片未插入：格式不支持", true);
            return false;
        }
        let Some(body) = self.ivars().body_view.get() else {
            return false;
        };
        let Some(attributed) = (unsafe { body.textStorage() }).map(|storage| {
            let source: &NSAttributedString = &storage;
            source.mutableCopy()
        }) else {
            return false;
        };
        let snapshot = EditorSnapshot {
            attributed,
            selection: body.selectedRange(),
        };
        let insertion_range = snapshot.selection;
        self.insert_image_data_from_snapshot(bytes, title, mime, body, snapshot, insertion_range)
    }

    fn insert_image_data_from_snapshot(
        &self,
        bytes: &[u8],
        title: &str,
        mime: &str,
        body: &NSTextView,
        snapshot: EditorSnapshot,
        insertion_range: NSRange,
    ) -> bool {
        let Some(note_id) = self.ivars().current_note_id.borrow().clone() else {
            return false;
        };
        let extension = if mime == "image/png" { "png" } else { "jpg" };
        let stored = match self.ivars().repository.import_resource(ResourceImport {
            bytes,
            title,
            mime,
            file_extension: extension,
        }) {
            Ok(stored) => stored,
            Err(error) => {
                eprintln!("paste image import failed: {error}");
                self.set_save_status("图片未插入：格式不支持", true);
                return false;
            }
        };
        let Some(inline) = inline_attachment_with_width(
            &stored,
            &stored.title,
            text_container_available_width(body),
        ) else {
            self.set_save_status("图片未插入：格式不支持", true);
            return false;
        };
        let source: &NSAttributedString = &snapshot.attributed;
        if candidate_with_attachment(source, insertion_range, &inline).is_none() {
            self.set_save_status("图片未插入：格式不支持", true);
            return false;
        }
        let title = self
            .ivars()
            .title_field
            .get()
            .map(|field| field.stringValue().to_string())
            .unwrap_or_default();
        let prepared = {
            let mut session_guard = self.ivars().editor_session.borrow_mut();
            let Some(session) = session_guard.as_mut() else {
                self.set_save_status("编辑器状态不可用，未覆盖正文", true);
                return false;
            };
            if let Err(error) =
                insert_image_anchor(session, insertion_range, &stored.id, &stored.title, 1, 1)
            {
                eprintln!("native image anchor failed: {error}");
                self.set_save_status("图片未插入：编辑器状态不可用", true);
                return false;
            }
            match document_from_session(session) {
                Ok(document) => PreparedNoteContent {
                    update: NoteContentUpdate {
                        title,
                        body: serialize_html(&document),
                    },
                },
                Err(error) => {
                    let _ = session.undo();
                    eprintln!("native editor projection failed: {error}");
                    self.set_save_status("保存失败", true);
                    return false;
                }
            }
        };
        let previous_loading_guard = *self.ivars().loading_guard.borrow();
        *self.ivars().loading_guard.borrow_mut() = true;
        let inserted = commit_live_image_insert(body, insertion_range, &inline, || {
            self.persist_note_content(&note_id, prepared)
        });
        *self.ivars().loading_guard.borrow_mut() = previous_loading_guard;
        if !inserted && let Some(session) = self.ivars().editor_session.borrow_mut().as_mut() {
            let _ = session.undo();
        }
        if inserted {
            *self.ivars().projection_attachments.borrow_mut() =
                projection_attachments_from_storage(body);
        }
        inserted
    }

    fn load_note(&self, note: &Note) {
        self.clear_pending_editor_intent();
        *self.ivars().loading_guard.borrow_mut() = true;
        *self.ivars().current_note_id.borrow_mut() = Some(note.id.clone());
        self.ivars()
            .title_field
            .get()
            .unwrap()
            .setStringValue(&NSString::from_str(&note.title));
        let body = self.ivars().body_view.get().unwrap();
        let (status, is_error) = match parse_html(&note.body) {
            Ok(document) => match session_from_document(&document) {
                Ok(session) => {
                    let rendered = render_session(
                        &session,
                        |resource_id| {
                            self.ivars()
                                .repository
                                .get_resource(resource_id)
                                .ok()
                                .flatten()
                        },
                        text_container_available_width(body),
                    );
                    self.install_rendered_document(body, &rendered, NSRange::new(0, 0));
                    *self.ivars().editor_session.borrow_mut() = Some(session);
                    if rendered.missing_resources == 0 {
                        ("已保存", false)
                    } else {
                        ("部分图片未恢复，已保留引用", true)
                    }
                }
                Err(error) => {
                    eprintln!("native editor load failed: {error}");
                    *self.ivars().editor_session.borrow_mut() = None;
                    self.ivars().projection_attachments.borrow_mut().clear();
                    self.ivars().projection_empty_carriers.borrow_mut().clear();
                    body.setString(ns_string!("正文无法读取"));
                    ("正文无法读取", true)
                }
            },
            Err(_) => {
                *self.ivars().editor_session.borrow_mut() = None;
                self.ivars().projection_attachments.borrow_mut().clear();
                self.ivars().projection_empty_carriers.borrow_mut().clear();
                body.setString(ns_string!("正文无法读取"));
                ("正文无法读取", true)
            }
        };
        *self.ivars().loading_guard.borrow_mut() = false;
        self.set_save_status(status, is_error);
        self.update_editor_visibility();
        self.update_note_selection();
        self.update_formatting_buttons();
    }

    fn clear_current_note(&self) {
        self.clear_pending_editor_intent();
        *self.ivars().loading_guard.borrow_mut() = true;
        *self.ivars().current_note_id.borrow_mut() = None;
        *self.ivars().editor_session.borrow_mut() = None;
        self.ivars().projection_attachments.borrow_mut().clear();
        self.ivars().projection_empty_carriers.borrow_mut().clear();
        self.ivars()
            .title_field
            .get()
            .unwrap()
            .setStringValue(ns_string!(""));
        self.ivars()
            .body_view
            .get()
            .unwrap()
            .setString(ns_string!(""));
        *self.ivars().loading_guard.borrow_mut() = false;
        self.update_editor_visibility();
        self.update_note_selection();
    }

    fn update_editor_visibility(&self) {
        let has_note = self.ivars().current_note_id.borrow().is_some();
        if let Some(view) = self.ivars().title_field.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().bold_button.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().italic_button.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().underline_button.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().clear_button.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().body_scroll.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().delete_button.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().save_status.get() {
            view.setHidden(!has_note);
        }
        if let Some(label) = self.ivars().editor_empty_label.get() {
            label.setHidden(has_note);
        }
    }

    fn set_save_status(&self, status: &str, failed: bool) {
        if let Some(field) = self.ivars().save_status.get() {
            field.setStringValue(&NSString::from_str(status));
            let color = if failed {
                NSColor::systemRedColor()
            } else {
                NSColor::secondaryLabelColor()
            };
            field.setTextColor(Some(&color));
        }
    }

    fn refresh_notes(&self) {
        if let Ok(notes) = self.ivars().repository.list_notes() {
            self.replace_note_list(notes);
        }
    }

    fn search_notes(&self, query: &str) {
        let notes = if query.trim().is_empty() {
            self.ivars().repository.list_notes()
        } else {
            self.ivars().repository.search(query)
        };
        if let Ok(notes) = notes {
            self.replace_note_list(notes);
        }
    }

    fn replace_note_list(&self, notes: Vec<Note>) {
        let Some(stack) = self.ivars().list_stack.get() else {
            return;
        };
        for button in self.ivars().note_buttons.borrow_mut().drain(..) {
            button.removeFromSuperview();
        }
        for row in self.ivars().note_rows.borrow_mut().drain(..) {
            row.removeFromSuperview();
        }
        *self.ivars().notes.borrow_mut() = notes;
        let selected_id = self.ivars().current_note_id.borrow().clone();
        for (index, note) in self.ivars().notes.borrow().iter().enumerate() {
            let selected = selected_id.as_deref() == Some(note.id.as_str());
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!(""),
                    Some(self),
                    Some(sel!(selectNote:)),
                    self.mtm(),
                )
            };
            let row = NSBox::initWithFrame(
                NSBox::alloc(self.mtm()),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(220.0, 60.0)),
            );
            row.setBoxType(NSBoxType::Custom);
            row.setTransparent(false);
            row.setBorderWidth(0.0);
            row.setCornerRadius(8.0);
            let row_color = if selected {
                NSColor::selectedContentBackgroundColor()
            } else {
                NSColor::clearColor()
            };
            row.setFillColor(&row_color);
            button.setTag(index as isize);
            button.setBordered(false);
            button.setButtonType(NSButtonType::PushOnPushOff);
            button.setUsesSingleLineMode(false);
            button.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            button.setAlignment(NSTextAlignment::Left);
            button.setState(if selected {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            let color = if selected {
                NSColor::controlAccentColor()
            } else {
                NSColor::labelColor()
            };
            button.setContentTintColor(Some(&color));
            set_note_button_title(&button, note, selected);
            button.setFrame(NSRect::new(
                NSPoint::new(8.0, 0.0),
                NSSize::new(204.0, 60.0),
            ));
            row.addSubview(&button);
            stack.addSubview(&row);
            self.ivars().note_buttons.borrow_mut().push(button);
            self.ivars().note_rows.borrow_mut().push(row);
        }
        let is_empty = self.ivars().notes.borrow().is_empty();
        if let Some(label) = self.ivars().list_empty_label.get() {
            label.setHidden(!is_empty);
        }
        let (width, height) = self
            .ivars()
            .window
            .get()
            .and_then(|window| window.contentView())
            .map(|content| (content.frame().size.width, content.frame().size.height))
            .unwrap_or((1100.0, 720.0));
        self.layout_content(width, height);
    }

    fn update_note_selection(&self) {
        let selected_id = self.ivars().current_note_id.borrow().clone();
        for (index, button) in self.ivars().note_buttons.borrow().iter().enumerate() {
            let selected = self
                .ivars()
                .notes
                .borrow()
                .get(index)
                .map(|note| selected_id.as_deref() == Some(note.id.as_str()))
                .unwrap_or(false);
            button.setState(if selected {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            let color = if selected {
                NSColor::controlAccentColor()
            } else {
                NSColor::labelColor()
            };
            button.setContentTintColor(Some(&color));
            if let Some(row) = self.ivars().note_rows.borrow().get(index) {
                let row_color = if selected {
                    NSColor::selectedContentBackgroundColor()
                } else {
                    NSColor::clearColor()
                };
                row.setFillColor(&row_color);
            }
            if let Some(note) = self.ivars().notes.borrow().get(index) {
                set_note_button_title(button, note, selected);
            }
        }
    }

    #[allow(deprecated)]
    fn save_current_note(&self) -> bool {
        if *self.ivars().loading_guard.borrow()
            || self
                .ivars()
                .body_view
                .get()
                .is_some_and(|body| body.hasMarkedText())
        {
            return false;
        }
        self.save_current_note_unchecked()
    }

    fn save_current_note_unchecked(&self) -> bool {
        let Some(id) = self.ivars().current_note_id.borrow().clone() else {
            return false;
        };
        let title = self
            .ivars()
            .title_field
            .get()
            .map(|field| field.stringValue().to_string())
            .unwrap_or_default();
        let prepared = {
            let session_guard = self.ivars().editor_session.borrow();
            let Some(session) = session_guard.as_ref() else {
                self.set_save_status("编辑器状态不可用，未覆盖正文", true);
                return false;
            };
            match document_from_session(session) {
                Ok(document) => PreparedNoteContent {
                    update: NoteContentUpdate {
                        title,
                        body: serialize_html(&document),
                    },
                },
                Err(error) => {
                    eprintln!("native editor projection failed: {error}");
                    self.set_save_status("保存失败", true);
                    return false;
                }
            }
        };
        self.persist_note_content(&id, prepared)
    }

    fn persist_note_content(&self, id: &str, prepared: PreparedNoteContent) -> bool {
        let PreparedNoteContent { update } = prepared;
        match self.ivars().repository.update_note_content(id, update) {
            Ok(updated) => {
                let index = self
                    .ivars()
                    .notes
                    .borrow()
                    .iter()
                    .position(|note| note.id == updated.id);
                if let Some(index) = index {
                    self.ivars().notes.borrow_mut()[index] = updated.clone();
                    if let Some(button) = self.ivars().note_buttons.borrow().get(index) {
                        set_note_button_title(button, &updated, true);
                    }
                }
                self.set_save_status("已保存", false);
                true
            }
            Err(error) => {
                eprintln!("autosave failed: {error}");
                self.set_save_status("保存失败", true);
                false
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteFileError {
    TooLarge,
    Invalid,
}

fn read_regular_image_file(path: &Path) -> Result<Vec<u8>, PasteFileError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| PasteFileError::Invalid)?;
    let metadata = file.metadata().map_err(|_| PasteFileError::Invalid)?;
    if !metadata.is_file() {
        return Err(PasteFileError::Invalid);
    }
    if metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err(PasteFileError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_IMAGE_BYTES as u64) as usize);
    file.take((MAX_IMAGE_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PasteFileError::Invalid)?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(PasteFileError::TooLarge);
    }
    Ok(bytes)
}

fn read_drag_image_file(path: &Path) -> Result<PasteboardImage, PasteFileError> {
    let mime = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => return Err(PasteFileError::Invalid),
    };
    let bytes = read_regular_image_file(path)?;
    if !valid_image_bytes_for_mime(&bytes, mime) {
        return Err(PasteFileError::Invalid);
    }
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("图片")
        .to_owned();
    Ok(PasteboardImage::Data {
        bytes,
        title,
        mime: mime.to_owned(),
    })
}

fn read_drag_pasteboard(pasteboard: &NSPasteboard) -> Result<PasteboardImage, PasteFileError> {
    let file_url_type = unsafe { NSPasteboardTypeFileURL };
    let Some(types) = pasteboard.types() else {
        return Err(PasteFileError::Invalid);
    };
    if types.iter().any(|item| {
        let item: &NSString = item.as_ref();
        is_promised_pasteboard_type(item.to_string().as_str())
    }) {
        return Err(PasteFileError::Invalid);
    }
    if !types.iter().any(|item| {
        let item: &NSString = item.as_ref();
        item == file_url_type
    }) {
        return Err(PasteFileError::Invalid);
    }
    let Some(items) = pasteboard.pasteboardItems() else {
        return Err(PasteFileError::Invalid);
    };
    if items.len() != 1 {
        return Err(PasteFileError::Invalid);
    }
    let Some(url_text) = pasteboard.stringForType(file_url_type) else {
        return Err(PasteFileError::Invalid);
    };
    let Some(url) = NSURL::initWithString(NSURL::alloc(), &url_text) else {
        return Err(PasteFileError::Invalid);
    };
    let host = url.host().map(|host| host.to_string());
    if !url.isFileURL() || !is_local_file_url_host(host.as_deref()) {
        return Err(PasteFileError::Invalid);
    }
    let Some(path) = url.path() else {
        return Err(PasteFileError::Invalid);
    };
    read_drag_image_file(Path::new(path.to_string().as_str()))
}

enum PasteboardImage {
    NotImage,
    Rejected(&'static str),
    Data {
        bytes: Vec<u8>,
        title: String,
        mime: String,
    },
}

fn normalize_paste_image(bytes: Vec<u8>, title: &str, mime: &str) -> PasteboardImage {
    if bytes.len() > MAX_IMAGE_BYTES {
        PasteboardImage::Rejected("图片未插入：超过 10 MB")
    } else if valid_image_bytes_for_mime(&bytes, mime) {
        PasteboardImage::Data {
            bytes,
            title: title.to_owned(),
            mime: mime.to_owned(),
        }
    } else {
        PasteboardImage::Rejected("图片未插入：格式不支持")
    }
}

fn normalize_tiff(bytes: Vec<u8>, title: &str) -> PasteboardImage {
    if bytes.len() > MAX_IMAGE_BYTES {
        return PasteboardImage::Rejected("图片未插入：超过 10 MB");
    }
    let data = NSData::with_bytes(&bytes);
    let Some(rep) = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &data) else {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    };
    let empty_keys: [&NSString; 0] = [];
    let empty_values: [&AnyObject; 0] = [];
    let properties = NSDictionary::<NSString, AnyObject>::from_slices(&empty_keys, &empty_values);
    let Some(png) = (unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
    }) else {
        return PasteboardImage::Rejected("图片未插入：格式不支持");
    };
    normalize_paste_image(png.to_vec(), title, "image/png")
}

fn valid_image_bytes(bytes: &[u8]) -> bool {
    decoded_image(bytes).is_some()
}

fn decoded_image(bytes: &[u8]) -> Option<Retained<NSImage>> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let data = NSData::with_bytes(bytes);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    if !image.isValid() {
        return None;
    }
    // NSImage may be initialized lazily. Asking AppKit for a concrete TIFF
    // representation forces the underlying bitmap/image representation to be
    // decoded before the bytes are admitted to storage or rendering.
    image.TIFFRepresentation()?;
    Some(image)
}

fn image_signature_matches_mime(bytes: &[u8], mime: &str) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        _ => false,
    }
}

fn valid_image_bytes_for_mime(bytes: &[u8], mime: &str) -> bool {
    image_signature_matches_mime(bytes, mime) && valid_image_bytes(bytes)
}

fn is_local_file_url_host(host: Option<&str>) -> bool {
    host.is_none_or(|host| host.is_empty() || host.eq_ignore_ascii_case("localhost"))
}

fn is_promised_pasteboard_type(type_name: &str) -> bool {
    type_name.to_ascii_lowercase().contains("promise")
}

fn inline_attachment(
    resource: &joplin_lite_native::core::StoredResource,
) -> Option<Retained<NSMutableAttributedString>> {
    inline_attachment_with_alt(resource, &resource.title)
}

fn commit_after_persistence<P, A>(persist: P, apply: A) -> bool
where
    P: FnOnce() -> bool,
    A: FnOnce(),
{
    if !persist() {
        return false;
    }
    apply();
    true
}

fn commit_live_image_insert(
    body: &NSTextView,
    insertion_range: NSRange,
    inline: &NSMutableAttributedString,
    persist: impl FnOnce() -> bool,
) -> bool {
    commit_after_persistence(persist, || {
        body.setSelectedRange(insertion_range);
        insert_inline_attachment(body, inline);
    })
}

#[allow(deprecated)]
fn insert_inline_attachment(body: &NSTextView, inline: &NSMutableAttributedString) {
    unsafe { body.insertText(inline as &AnyObject) };
}

fn inline_attachment_with_alt(
    resource: &joplin_lite_native::core::StoredResource,
    alt: &str,
) -> Option<Retained<NSMutableAttributedString>> {
    inline_attachment_with_width(resource, alt, 640.0)
}

fn text_container_available_width(body: &NSTextView) -> f64 {
    let container_width = unsafe { body.textContainer() }
        .map(|container| {
            (container.containerSize().width - container.lineFragmentPadding() * 2.0).max(1.0)
        })
        .filter(|width| width.is_finite() && *width > 0.0);
    container_width.unwrap_or_else(|| {
        let inset = body.textContainerInset();
        (body.frame().size.width - inset.width * 2.0).max(1.0)
    })
}

fn inline_image_display_size(image_size: NSSize, available_width: f64) -> NSSize {
    let width = image_size.width.max(1.0);
    let height = image_size.height.max(1.0);
    let max_width = available_width.clamp(1.0, 640.0);
    let scale = (max_width / width).min(640.0 / height).min(1.0);
    NSSize::new(width * scale, height * scale)
}

fn resize_inline_attachments(body: &NSTextView) {
    let Some(storage) = (unsafe { body.textStorage() }) else {
        return;
    };
    let length = storage.length();
    if length == 0 {
        return;
    }
    let attachment_key = unsafe { NSAttachmentAttributeName };
    let available_width = text_container_available_width(body);
    let layout_manager = unsafe { body.layoutManager() };
    let mut location = 0;
    while location < length {
        let mut effective_range = NSRange::new(location, 0);
        let attributes = unsafe {
            storage.attributesAtIndex_longestEffectiveRange_inRange(
                location,
                &mut effective_range,
                NSRange::new(0, length),
            )
        };
        if let Some(value) = unsafe { attributes.objectForKey_unchecked(attachment_key) }
            && let Some(attachment) = value.downcast_ref::<NSTextAttachment>()
            && let Some(image) = attachment.image()
        {
            let bounds = attachment.bounds();
            let size = inline_image_display_size(image.size(), available_width);
            if bounds.size != size {
                attachment.setBounds(NSRect::new(bounds.origin, size));
                if let Some(layout_manager) = layout_manager.as_ref() {
                    let attachment_range = NSRange::new(location, 1);
                    unsafe {
                        layout_manager.invalidateLayoutForCharacterRange_actualCharacterRange(
                            attachment_range,
                            null_mut(),
                        );
                    }
                    layout_manager.invalidateDisplayForCharacterRange(attachment_range);
                }
            }
        }
        let next = effective_range.location + effective_range.length;
        if next <= location {
            break;
        }
        location = next;
    }
}

fn first_attachment_line_metrics(body: &NSTextView) -> Option<(NSRect, NSRect, NSRect)> {
    let storage = unsafe { body.textStorage() }?;
    let layout_manager = unsafe { body.layoutManager() }?;
    let container = unsafe { body.textContainer() }?;
    let length = storage.length();
    if length == 0 {
        return None;
    }
    let attachment_key = unsafe { NSAttachmentAttributeName };
    let mut location = 0;
    while location < length {
        let mut effective_range = NSRange::new(location, 0);
        let attributes = unsafe {
            storage.attributesAtIndex_longestEffectiveRange_inRange(
                location,
                &mut effective_range,
                NSRange::new(0, length),
            )
        };
        if unsafe { attributes.objectForKey_unchecked(attachment_key) }.is_some() {
            let glyph_range = unsafe {
                layout_manager.glyphRangeForCharacterRange_actualCharacterRange(
                    NSRange::new(location, 1),
                    null_mut(),
                )
            };
            let line_range = NSRange::new(glyph_range.location, glyph_range.length.max(1));
            let glyph_rect =
                layout_manager.boundingRectForGlyphRange_inTextContainer(line_range, &container);
            let line_rect = unsafe {
                layout_manager
                    .lineFragmentRectForGlyphAtIndex_effectiveRange(line_range.location, null_mut())
            };
            let used_rect = unsafe {
                layout_manager.lineFragmentUsedRectForGlyphAtIndex_effectiveRange(
                    line_range.location,
                    null_mut(),
                )
            };
            return Some((glyph_rect, line_rect, used_rect));
        }
        let next = effective_range.location + effective_range.length;
        if next <= location {
            break;
        }
        location = next;
    }
    None
}

fn first_attachment_bounds(body: &NSTextView) -> Option<NSRect> {
    let storage = unsafe { body.textStorage() }?;
    let length = storage.length();
    if length == 0 {
        return None;
    }
    let attachment_key = unsafe { NSAttachmentAttributeName };
    let mut location = 0;
    while location < length {
        let mut effective_range = NSRange::new(location, 0);
        let attributes = unsafe {
            storage.attributesAtIndex_longestEffectiveRange_inRange(
                location,
                &mut effective_range,
                NSRange::new(0, length),
            )
        };
        if let Some(value) = unsafe { attributes.objectForKey_unchecked(attachment_key) }
            && let Some(attachment) = value.downcast_ref::<NSTextAttachment>()
        {
            return Some(attachment.bounds());
        }
        let next = effective_range.location + effective_range.length;
        if next <= location {
            break;
        }
        location = next;
    }
    None
}

fn inline_attachment_with_width(
    resource: &joplin_lite_native::core::StoredResource,
    alt: &str,
    available_width: f64,
) -> Option<Retained<NSMutableAttributedString>> {
    let image = decoded_image(&resource.bytes)?;
    let data = NSData::with_bytes(&resource.bytes);
    let uti = NSString::from_str(if resource.mime == "image/jpeg" {
        "public.jpeg"
    } else {
        "public.png"
    });
    let attachment =
        NSTextAttachment::initWithData_ofType(NSTextAttachment::alloc(), Some(&data), Some(&uti));
    attachment.setImage(Some(&image));
    attachment.setBounds(NSRect::new(
        NSPoint::new(0.0, 0.0),
        inline_image_display_size(image.size(), available_width),
    ));
    let attributed = NSAttributedString::attributedStringWithAttachment(&attachment);
    let mutable = NSMutableAttributedString::from_attributed_nsstring(&attributed);
    let id = NSString::from_str(&resource.id);
    let alt = NSString::from_str(alt);
    let id_key = resource_id_attribute_key();
    let alt_key = resource_alt_attribute_key();
    unsafe {
        mutable.addAttribute_value_range(&id_key, &id, NSRange::new(0, 1));
        mutable.addAttribute_value_range(&alt_key, &alt, NSRange::new(0, 1));
    }
    Some(mutable)
}

fn set_note_button_title(button: &NSButton, note: &Note, selected: bool) {
    let title = note_list_title(note);
    let summary = note_list_summary(note);
    let label = format!("{title}\n{summary}");
    let attributed = NSMutableAttributedString::from_nsstring(&NSString::from_str(&label));
    let title_font = NSFont::systemFontOfSize_weight(15.0, if selected { 0.3 } else { 0.0 });
    let summary_font = NSFont::systemFontOfSize(12.0);
    let title_color = if selected {
        NSColor::whiteColor()
    } else {
        NSColor::labelColor()
    };
    let summary_color = if selected {
        NSColor::whiteColor()
    } else {
        NSColor::secondaryLabelColor()
    };
    let title_length = NSString::from_str(&title).length();
    let full_length = NSString::from_str(&label).length();
    unsafe {
        attributed.addAttribute_value_range(
            NSFontAttributeName,
            &title_font,
            NSRange::new(0, title_length),
        );
        attributed.addAttribute_value_range(
            NSForegroundColorAttributeName,
            &title_color,
            NSRange::new(0, title_length),
        );
        if full_length > title_length + 1 {
            let summary_range = NSRange::new(title_length + 1, full_length - title_length - 1);
            attributed.addAttribute_value_range(NSFontAttributeName, &summary_font, summary_range);
            attributed.addAttribute_value_range(
                NSForegroundColorAttributeName,
                &summary_color,
                summary_range,
            );
        }
    }
    button.setAttributedTitle(attributed.as_ref());
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum TextFormat {
    Bold,
    Italic,
    Underline,
    Clear,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FormatTarget {
    Selection,
    Typing,
}

#[cfg(test)]
fn format_target(range: NSRange) -> FormatTarget {
    if range.length == 0 {
        FormatTarget::Typing
    } else {
        FormatTarget::Selection
    }
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("AppKit must run on the main thread");
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let override_path = std::env::var_os("JOPLIN_LITE_NATIVE_DATA_DIR").map(PathBuf::from);
    let default_path = dirs::data_dir().map(|path| path.join("com.kevinhao.joplin-lite-native"));
    let data_dir = resolve_native_data_dir(override_path, default_path, dirs::home_dir())
        .unwrap_or_else(|error| panic!("could not resolve native data directory: {error:?}"));
    let data_path = data_dir.join("notes.sqlite");
    ensure_notes_database_file(&data_path)
        .unwrap_or_else(|error| panic!("could not validate notes database file: {error:?}"));
    let repository =
        Arc::new(NoteRepository::open(&data_path).expect("could not open notes database"));
    if let Err(failure) = migrate_legacy_notes_before_window(&repository) {
        show_legacy_migration_failure(mtm, &failure, &data_dir);
        return;
    }
    if let Err(error) = repository.cleanup_abandoned_drafts() {
        eprintln!("could not clean drafts: {error}");
    }
    let delegate = AppDelegate::new(mtm, repository);
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    application.run();
}

impl AppDelegate {
    fn new(mtm: MainThreadMarker, repository: Arc<NoteRepository>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars {
            window: OnceCell::new(),
            repository,
            current_note_id: RefCell::new(None),
            notes: RefCell::new(Vec::new()),
            loading_guard: RefCell::new(false),
            editor_session: RefCell::new(None),
            projection_attachments: RefCell::new(Vec::new()),
            projection_empty_carriers: RefCell::new(Vec::new()),
            pending_editor_intents: RefCell::new(Vec::new()),
            pending_editor_composition: RefCell::new(None),
            pending_editor_intent_invalid: RefCell::new(false),
            sidebar_background: OnceCell::new(),
            sidebar_separator: OnceCell::new(),
            list_scroll: OnceCell::new(),
            list_stack: OnceCell::new(),
            list_empty_label: OnceCell::new(),
            new_button: OnceCell::new(),
            search_field: OnceCell::new(),
            title_field: OnceCell::new(),
            bold_button: OnceCell::new(),
            italic_button: OnceCell::new(),
            underline_button: OnceCell::new(),
            clear_button: OnceCell::new(),
            body_scroll: OnceCell::new(),
            body_view: OnceCell::new(),
            delete_button: OnceCell::new(),
            save_status: OnceCell::new(),
            editor_empty_label: OnceCell::new(),
            note_buttons: RefCell::new(Vec::new()),
            note_rows: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContentLayout, DataDirError, DataFileError, FontTraitOperation, FormatDecision,
        FormatTarget, LegacyMigrationFailure, Note, PasteFileError, PasteRoute, PasteboardImage,
        TextFormat, candidate_with_attachment, choose_data_dir, commit_after_persistence,
        content_layout, display_note_title, document_from_attributed_string,
        ensure_notes_database_file, format_decision, format_target, image_signature_matches_mime,
        inline_image_display_size, is_local_file_url_host, is_promised_pasteboard_type,
        legacy_migration_recovery_message, note_list_summary, note_list_title, paste_route,
        read_drag_image_file, read_pasteboard_image_from, read_regular_image_file,
        render_document_to_attributed_string, typing_trait_operation, valid_image_bytes_for_mime,
        validate_canonical_data_dir,
    };
    use joplin_lite_native::core::{LegacyNoteForHtmlMigration, NoteRepository, StoredResource};
    use joplin_lite_native::html_body::{Block, Document, Inline, Marks, serialize_html};
    use objc2::{AnyThread, runtime::AnyObject};
    use objc2_app_kit::{
        NSAttributedStringAttachmentConveniences, NSBitmapImageFileType, NSBitmapImageRep,
        NSFontAttributeName, NSMutableParagraphStyle, NSPasteboard, NSPasteboardTypeFileURL,
        NSPasteboardTypePNG, NSPasteboardTypeTIFF, NSTextAttachment, NSUnderlineStyle,
        NSUnderlineStyleAttributeName,
    };
    use objc2_foundation::{
        NSAttributedString, NSData, NSDictionary, NSMutableAttributedString, NSRange, NSSize,
        NSString, NSURL,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    const TEST_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn paragraph(inlines: Vec<Inline>) -> Block {
        Block::Paragraph {
            style: Default::default(),
            inlines,
        }
    }

    #[test]
    fn attributed_document_codec_preserves_cjk_emoji_marks_and_paragraphs() {
        let source = NSMutableAttributedString::from_nsstring(&NSString::from_str(
            "中文😀\u{2028}第二段\n第三段",
        ));
        let bold_font = objc2_app_kit::NSFont::boldSystemFontOfSize(17.0);
        let underline = objc2_foundation::NSNumber::numberWithInteger(NSUnderlineStyle::Single.0);
        unsafe {
            source.addAttribute_value_range(
                NSFontAttributeName,
                &bold_font,
                NSRange::new(0, NSString::from_str("中文😀").length()),
            );
            source.addAttribute_value_range(
                NSUnderlineStyleAttributeName,
                &underline,
                NSRange::new(0, 2),
            );
        }
        let source_ref: &NSAttributedString = &source;
        let document = document_from_attributed_string(source_ref).unwrap();
        assert_eq!(
            serialize_html(&document),
            "<p><strong><u>中文</u></strong><strong>😀</strong><br>第二段</p><p>第三段</p>"
        );
        let rendered = render_document_to_attributed_string(&document, |_| None, 640.0).0;
        assert_eq!(
            rendered.string().to_string(),
            "中文😀\u{2028}第二段\n第三段"
        );
    }

    #[test]
    fn ime_marked_text_skips_semantic_sync_and_save_until_commit() {
        assert!(!super::should_sync_editor_change(true, false));
        assert!(!super::should_sync_editor_change(true, true));
        assert!(!super::should_sync_editor_change(false, true));
        assert!(super::should_sync_editor_change(false, false));
    }

    #[test]
    fn semantic_sync_equal_text_is_noop_and_attachment_sentinel_is_rejected() {
        assert_eq!(
            super::classify_editor_text_change("same", "same"),
            super::EditorTextSyncDecision::Noop
        );
        assert_eq!(
            super::classify_editor_text_change("same", "\u{fffc}"),
            super::EditorTextSyncDecision::Reject
        );
        assert_eq!(
            super::classify_editor_text_change("a\u{fffc}b", "ab"),
            super::EditorTextSyncDecision::DeleteImage
        );
        assert_eq!(
            super::classify_editor_text_change("a\u{fffc}b", "a\u{fffc}Xb"),
            super::EditorTextSyncDecision::ApplyDelta
        );
        assert_eq!(
            super::classify_editor_text_change("ab", "a\u{fffc}b"),
            super::EditorTextSyncDecision::Reject
        );
        assert_eq!(
            super::editor_text_delta("😀x", "前😀x"),
            Some((NSRange::new(0, 0), "前".into()))
        );
    }

    #[test]
    fn attachment_string_diff_is_ambiguous_without_pending_appkit_intent() {
        assert_eq!(
            super::editor_text_delta("\u{fffc}\u{fffc}", "\u{fffc}"),
            None
        );
    }

    #[test]
    fn pending_appkit_intent_preserves_attachment_identity_and_rejects_stale_views() {
        let intent = |location: usize, resource_id: &str| super::PendingEditorIntent {
            range: NSRange::new(location, 1),
            replacement: String::new(),
            old_view_text: "\u{fffc}\u{fffc}".into(),
            old_semantic_text: "\u{fffc}\u{fffc}".into(),
            covered_attachments: vec![super::RenderedAttachment {
                addressable_offset: location,
                resource_id: resource_id.into(),
            }],
        };

        assert!(matches!(
            super::decide_pending_editor_intent(&intent(0, "image-a"), "\u{fffc}"),
            super::PendingIntentDecision::DeleteImage { range, resource_id }
                if range == NSRange::new(0, 1) && resource_id == "image-a"
        ));
        assert!(matches!(
            super::decide_pending_editor_intent(&intent(1, "image-b"), "\u{fffc}"),
            super::PendingIntentDecision::DeleteImage { range, resource_id }
                if range == NSRange::new(1, 1) && resource_id == "image-b"
        ));

        let mut stale = intent(0, "image-a");
        stale.old_view_text = "\u{fffc}".into();
        assert_eq!(
            super::decide_pending_editor_intent(&stale, ""),
            super::PendingIntentDecision::Reject
        );
    }

    #[test]
    fn pending_appkit_intent_accepts_zero_offset_utf16_text_insertion() {
        let intent = super::PendingEditorIntent {
            range: NSRange::new(0, 0),
            replacement: "前".into(),
            old_view_text: "😀x".into(),
            old_semantic_text: "😀x".into(),
            covered_attachments: Vec::new(),
        };
        assert!(matches!(
            super::decide_pending_editor_intent(&intent, "前😀x"),
            super::PendingIntentDecision::ApplyText { range, replacement }
                if range == NSRange::new(0, 0) && replacement == "前"
        ));
        assert_eq!(
            super::apply_utf16_intent_to_text("😀x", NSRange::new(1, 0), "前"),
            None
        );
    }

    #[test]
    fn composition_accumulator_replays_real_ime_trace_and_commits_once() {
        let mut composition = None;
        composition = Some(
            super::accumulate_marked_editor_intent(
                composition,
                "A",
                "A",
                NSRange::new(1, 0),
                "n",
                NSRange::new(1, 0),
                Vec::new(),
            )
            .unwrap(),
        );
        assert_eq!(composition.as_ref().unwrap().current_view_text, "An");
        composition = Some(
            super::accumulate_marked_editor_intent(
                composition,
                "An",
                "A",
                NSRange::new(1, 1),
                "ni",
                NSRange::new(1, 1),
                Vec::new(),
            )
            .unwrap(),
        );
        assert_eq!(composition.as_ref().unwrap().current_view_text, "Ani");
        composition = Some(
            super::accumulate_marked_editor_intent(
                composition,
                "Ani",
                "A",
                NSRange::new(1, 2),
                "你",
                NSRange::new(1, 2),
                Vec::new(),
            )
            .unwrap(),
        );
        assert_eq!(composition.as_ref().unwrap().current_view_text, "A你");
        assert_eq!(
            super::finish_marked_editor_intent(composition.as_ref().unwrap(), "A你"),
            super::PendingIntentDecision::ApplyText {
                range: NSRange::new(1, 0),
                replacement: "你".into(),
            }
        );
    }

    #[test]
    fn composition_accumulator_handles_emoji_selection_and_cancel() {
        let composition = super::accumulate_marked_editor_intent(
            None,
            "😀A",
            "😀A",
            NSRange::new(2, 1),
            "n",
            NSRange::new(2, 1),
            Vec::new(),
        )
        .unwrap();
        let composition = super::accumulate_marked_editor_intent(
            Some(composition),
            "😀n",
            "😀A",
            NSRange::new(2, 1),
            "你",
            NSRange::new(2, 1),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            super::finish_marked_editor_intent(&composition, "😀你"),
            super::PendingIntentDecision::ApplyText {
                range: NSRange::new(2, 1),
                replacement: "你".into(),
            }
        );

        let cancelled = super::accumulate_marked_editor_intent(
            None,
            "A",
            "A",
            NSRange::new(1, 0),
            "n",
            NSRange::new(1, 0),
            Vec::new(),
        )
        .unwrap();
        let cancelled = super::accumulate_marked_editor_intent(
            Some(cancelled),
            "An",
            "A",
            NSRange::new(1, 1),
            "",
            NSRange::new(1, 1),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            super::finish_marked_editor_intent(&cancelled, "A"),
            super::PendingIntentDecision::Noop
        );
        assert!(
            super::accumulate_marked_editor_intent(
                Some(cancelled),
                "stale",
                "A",
                NSRange::new(1, 0),
                "你",
                NSRange::new(1, 0),
                Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn production_pending_decision_rejects_multi_attachment_delete_and_all_aaa_positions() {
        let attachments = |count: usize| {
            (0..count)
                .map(|index| super::RenderedAttachment {
                    addressable_offset: index,
                    resource_id: format!("image-{index}"),
                })
                .collect::<Vec<_>>()
        };
        let multi = super::PendingEditorIntent {
            range: NSRange::new(0, 2),
            replacement: String::new(),
            old_view_text: "\u{fffc}\u{fffc}".into(),
            old_semantic_text: "\u{fffc}\u{fffc}".into(),
            covered_attachments: attachments(2),
        };
        assert_eq!(
            super::preflight_pending_editor_intent(&multi),
            Err(super::PendingIntentRejection::AmbiguousAttachment)
        );
        let stale = super::PendingEditorIntent {
            old_view_text: "old".into(),
            old_semantic_text: "new".into(),
            range: NSRange::new(0, 0),
            replacement: "x".into(),
            covered_attachments: Vec::new(),
        };
        assert_eq!(
            super::preflight_pending_editor_intent(&stale),
            Err(super::PendingIntentRejection::StaleBaseline)
        );

        for (location, id) in [(0, "image-a"), (1, "image-a2"), (2, "image-b")] {
            let intent = super::PendingEditorIntent {
                range: NSRange::new(location, 1),
                replacement: String::new(),
                old_view_text: "\u{fffc}\u{fffc}\u{fffc}".into(),
                old_semantic_text: "\u{fffc}\u{fffc}\u{fffc}".into(),
                covered_attachments: vec![super::RenderedAttachment {
                    addressable_offset: location,
                    resource_id: id.into(),
                }],
            };
            assert_eq!(super::preflight_pending_editor_intent(&intent), Ok(()));
            let new_view = super::apply_utf16_intent_to_text(
                "\u{fffc}\u{fffc}\u{fffc}",
                NSRange::new(location, 1),
                "",
            )
            .unwrap();
            assert!(matches!(
                super::decide_pending_editor_intent(&intent, &new_view),
                super::PendingIntentDecision::DeleteImage { resource_id, .. }
                    if resource_id == id
            ));
        }
    }

    #[test]
    fn empty_carrier_install_decision_requires_collapsed_exact_caret() {
        let rendered = super::RenderedDocument {
            attributed: NSMutableAttributedString::from_nsstring(&NSString::from_str("")),
            missing_resources: 0,
            empty_block_carriers: vec![super::EmptyBlockCarrier {
                addressable_offset: 0,
                paragraph: NSMutableParagraphStyle::new(),
            }],
            attachments: Vec::new(),
        };
        assert!(super::exact_empty_block_carrier(&rendered, NSRange::new(0, 0)).is_some());
        assert!(super::exact_empty_block_carrier(&rendered, NSRange::new(0, 1)).is_none());
        assert!(super::exact_empty_block_carrier(&rendered, NSRange::new(1, 0)).is_none());
    }

    #[test]
    fn rejected_live_sync_never_enters_save_path() {
        assert!(super::should_persist_after_editor_sync(
            super::EditorSessionSyncResult::Noop
        ));
        assert!(super::should_persist_after_editor_sync(
            super::EditorSessionSyncResult::Applied
        ));
        assert!(!super::should_persist_after_editor_sync(
            super::EditorSessionSyncResult::Rejected
        ));
        assert!(super::should_restore_after_editor_sync(
            super::EditorSessionSyncResult::Rejected
        ));
        assert!(!super::should_restore_after_editor_sync(
            super::EditorSessionSyncResult::Applied
        ));
    }

    #[test]
    fn projection_attachment_mapping_keeps_adjacent_identity_after_deletion() {
        let mut attachments = vec![
            super::RenderedAttachment {
                addressable_offset: 0,
                resource_id: "image-a".into(),
            },
            super::RenderedAttachment {
                addressable_offset: 1,
                resource_id: "image-b".into(),
            },
        ];
        assert!(super::adjust_projection_attachments(
            &mut attachments,
            NSRange::new(0, 1),
            0,
            Some("image-a")
        ));
        assert_eq!(
            attachments,
            vec![super::RenderedAttachment {
                addressable_offset: 0,
                resource_id: "image-b".into(),
            }]
        );

        let mut reverse = vec![
            super::RenderedAttachment {
                addressable_offset: 0,
                resource_id: "image-a".into(),
            },
            super::RenderedAttachment {
                addressable_offset: 1,
                resource_id: "image-b".into(),
            },
        ];
        assert!(super::adjust_projection_attachments(
            &mut reverse,
            NSRange::new(1, 1),
            0,
            Some("image-b")
        ));
        assert_eq!(
            reverse,
            vec![super::RenderedAttachment {
                addressable_offset: 0,
                resource_id: "image-a".into(),
            }]
        );
    }

    #[test]
    fn attributed_document_codec_normalizes_crlf_without_extra_paragraph() {
        let source = NSMutableAttributedString::from_nsstring(&NSString::from_str("前\r\n后"));
        let source_ref: &NSAttributedString = &source;
        let document = document_from_attributed_string(source_ref).unwrap();
        assert_eq!(serialize_html(&document), "<p>前</p><p>后</p>");
    }

    #[test]
    fn legacy_migration_decoder_accepts_empty_rtf_as_old_body() {
        let legacy = LegacyNoteForHtmlMigration {
            id: "legacy-empty".into(),
            title: "旧笔记".into(),
            body: "旧正文😀".into(),
            body_rtf: Vec::new(),
            is_draft: false,
            created_time: 1,
            updated_time: 2,
            deleted_time: 0,
        };
        let decoded = super::decode_legacy_rtf_for_html_migration(&legacy).unwrap();
        assert_eq!(decoded.string().to_string(), legacy.body);
    }

    #[test]
    fn legacy_migration_decoder_rejects_rtf_visible_body_mismatch() {
        let legacy = LegacyNoteForHtmlMigration {
            id: "legacy-mismatch".into(),
            title: "旧笔记".into(),
            body: "数据库正文".into(),
            body_rtf: br"{\rtf1\ansi decoded}".to_vec(),
            is_draft: false,
            created_time: 1,
            updated_time: 2,
            deleted_time: 0,
        };
        let error = super::decode_legacy_rtf_for_html_migration(&legacy).unwrap_err();
        assert_eq!(error.note_id, "legacy-mismatch");
        assert_eq!(error.reason, "RTF 正文与旧正文不一致");
    }

    #[test]
    fn legacy_migration_decoder_preserves_rtf_bold_italic_underline_runs() {
        let legacy = LegacyNoteForHtmlMigration {
            id: "legacy-marks".into(),
            title: "旧格式".into(),
            body: "BoldItalicUnderline".into(),
            body_rtf: br"{\rtf1\ansi\b Bold\b0\i Italic\i0\ul Underline\ul0}".to_vec(),
            is_draft: false,
            created_time: 1,
            updated_time: 2,
            deleted_time: 0,
        };
        let decoded = super::decode_legacy_rtf_for_html_migration(&legacy).unwrap();
        let source: &NSAttributedString = &decoded;
        let document = document_from_attributed_string(source).unwrap();
        assert_eq!(
            serialize_html(&document),
            "<p><strong>Bold</strong><em>Italic</em><u>Underline</u></p>"
        );
    }

    #[test]
    fn legacy_marker_migration_overlays_resources_before_html_conversion() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("notes.sqlite");
        let repository = NoteRepository::open(&database).unwrap();
        let resource = repository
            .import_resource(joplin_lite_native::core::ResourceImport {
                bytes: TEST_PNG,
                title: "截图.png",
                mime: "image/png",
                file_extension: "png",
            })
            .unwrap();
        let note = repository
            .create_note(joplin_lite_native::core::CreateNote {
                title: "旧标题".into(),
                body: "占位".into(),
                is_draft: false,
            })
            .unwrap();
        let marker_body = format!("前文\n![截图.png](:/{})\n后文", resource.id);
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE notes SET body = ?1, body_text = ?1, body_rtf = X'', markup_language = 1 WHERE id = ?2",
                rusqlite::params![marker_body, note.id],
            )
            .unwrap();
        drop(connection);

        let legacy = repository
            .list_legacy_notes_for_html_migration()
            .unwrap()
            .pop()
            .unwrap();
        let conversion = super::migrate_legacy_note_to_conversion(&repository, &legacy).unwrap();
        assert_eq!(conversion.body_text, "前文\n截图.png\n后文");
        assert_eq!(conversion.resource_ids, vec![resource.id.clone()]);
        assert!(
            conversion
                .body
                .contains(&format!("src=\":/{}\"", resource.id))
        );
        assert!(!conversion.body.contains("[截图.png]"));
    }

    #[test]
    fn attributed_document_codec_downgrades_unknown_attachments_without_payload() {
        let bytes = NSData::with_bytes(TEST_PNG);
        let attachment = NSTextAttachment::initWithData_ofType(
            NSTextAttachment::alloc(),
            Some(&bytes),
            Some(&NSString::from_str("public.png")),
        );
        let source = NSMutableAttributedString::from_nsstring(&NSString::from_str("前"));
        source.appendAttributedString(&NSAttributedString::attributedStringWithAttachment(
            &attachment,
        ));
        source.appendAttributedString(&NSAttributedString::initWithString(
            NSAttributedString::alloc(),
            &NSString::from_str("后"),
        ));
        let source_ref: &NSAttributedString = &source;
        let document = document_from_attributed_string(source_ref).unwrap();
        assert_eq!(serialize_html(&document), "<p>前[图片]后</p>");
        assert!(matches!(
            document.blocks[0],
            Block::Paragraph { ref inlines, .. }
                if inlines.iter().any(|inline| matches!(inline, Inline::Text { text, .. } if text.contains("[图片]")))
        ));
    }

    #[test]
    fn html_document_codec_preserves_known_image_position_and_alt() {
        let document = Document::from_blocks(vec![paragraph(vec![
            Inline::Text {
                text: "前".into(),
                marks: Marks {
                    bold: true,
                    ..Marks::default()
                },
            },
            Inline::Image {
                resource_id: "0123456789abcdef0123456789abcdef".into(),
                alt: "截图".into(),
            },
            Inline::Text {
                text: "后".into(),
                marks: Marks::default(),
            },
        ])]);
        let rendered = render_document_to_attributed_string(
            &document,
            |resource_id| {
                assert_eq!(resource_id, "0123456789abcdef0123456789abcdef");
                Some(StoredResource {
                    id: resource_id.into(),
                    sha256: "0".repeat(64),
                    size: TEST_PNG.len(),
                    title: "截图".into(),
                    mime: "image/png".into(),
                    file_extension: "png".into(),
                    path: PathBuf::new(),
                    bytes: TEST_PNG.to_vec(),
                })
            },
            640.0,
        )
        .0;
        assert_eq!(rendered.string().to_string(), "前\u{fffc}后");
        let source_ref: &NSAttributedString = &rendered;
        let round_trip = document_from_attributed_string(source_ref).unwrap();
        assert_eq!(serialize_html(&round_trip), serialize_html(&document));
    }

    #[test]
    fn missing_image_placeholder_preserves_reference_on_editor_round_trip() {
        let document = Document::from_blocks(vec![paragraph(vec![
            Inline::Text {
                text: "前".into(),
                marks: Marks::default(),
            },
            Inline::Image {
                resource_id: "0123456789abcdef0123456789abcdef".into(),
                alt: "截图".into(),
            },
            Inline::Image {
                resource_id: "fedcba9876543210fedcba9876543210".into(),
                alt: String::new(),
            },
            Inline::Text {
                text: "后".into(),
                marks: Marks::default(),
            },
        ])]);
        let (rendered, failures) = render_document_to_attributed_string(&document, |_| None, 640.0);
        assert_eq!(failures, 2);
        assert_eq!(rendered.string().to_string(), "前[图片：截图][图片]后");
        let source: &NSAttributedString = &rendered;
        let round_trip = document_from_attributed_string(source).unwrap();
        assert_eq!(serialize_html(&round_trip), serialize_html(&document));
    }

    #[test]
    fn content_layout_keeps_editor_regions_disjoint_at_supported_sizes() {
        for (width, height) in [(1100.0, 720.0), (860.0, 560.0)] {
            let layout: ContentLayout = content_layout(width, height);
            assert!(layout.sidebar.width >= 240.0);
            assert!(layout.body.width >= 400.0);
            assert!(layout.body.y + layout.body.height <= layout.toolbar.y);
            assert!(layout.body.y >= 0.0);
            assert!(layout.body.y + layout.body.height <= height);
            assert!(layout.title.x >= layout.sidebar.x + layout.sidebar.width);
            assert_eq!(layout.empty_editor.height, 44.0);
            let expected_center = layout.body.y + layout.body.height * 0.45;
            let actual_center = layout.empty_editor.y + layout.empty_editor.height * 0.5;
            assert!((actual_center - expected_center).abs() < f64::EPSILON);
        }
        let wide = content_layout(1800.0, 900.0);
        assert_eq!(wide.body.width, 720.0);
        assert_eq!(
            wide.body.x + wide.body.width,
            wide.delete.x + wide.delete.width
        );
        assert_eq!(
            wide.body.x + wide.body.width,
            wide.status.x + wide.status.width + 10.0
        );
    }

    #[test]
    fn inline_image_display_size_respects_column_and_preserves_aspect_ratio() {
        let size = inline_image_display_size(NSSize::new(1600.0, 800.0), 530.0);
        assert_eq!(size, NSSize::new(530.0, 265.0));
        let small = inline_image_display_size(NSSize::new(320.0, 160.0), 530.0);
        assert_eq!(small, NSSize::new(320.0, 160.0));
        let capped = inline_image_display_size(NSSize::new(1600.0, 800.0), 900.0);
        assert_eq!(capped, NSSize::new(640.0, 320.0));
        let tall = inline_image_display_size(NSSize::new(320.0, 1600.0), 900.0);
        assert_eq!(tall, NSSize::new(128.0, 640.0));
    }

    #[test]
    fn blank_title_uses_body_first_line_only_for_list_display() {
        assert_eq!(display_note_title("", "  正文首行\n第二行"), "正文首行");
        assert_eq!(
            display_note_title("我的真实标题", "正文首行"),
            "我的真实标题"
        );
        assert_eq!(display_note_title("", ""), "无标题笔记");
    }

    #[test]
    fn note_list_projection_uses_search_text_instead_of_canonical_html() {
        let titled = Note {
            id: "titled".into(),
            title: "项目标题".into(),
            body: "<p>正文不应直接显示</p>".into(),
            body_text: "正文不应直接显示".into(),
            markup_language: 2,
            is_draft: false,
            created_time: 1,
            updated_time: 1,
            deleted_time: 0,
        };
        assert_eq!(note_list_title(&titled), "项目标题");
        assert_eq!(note_list_summary(&titled), "正文不应直接显示");
        assert!(!note_list_summary(&titled).contains('<'));

        let untitled = Note {
            title: String::new(),
            body: "<p>首行正文</p><p>第二段</p>".into(),
            body_text: "首行正文\n第二段".into(),
            ..titled
        };
        assert_eq!(note_list_title(&untitled), "首行正文");
        assert_eq!(note_list_summary(&untitled), "首行正文");
        assert!(!note_list_title(&untitled).contains('<'));
    }

    #[test]
    fn legacy_migration_message_describes_profile_not_existing_backup() {
        let failure = LegacyMigrationFailure {
            note_id: "note-1".into(),
            reason: "RTF 解码失败".into(),
        };
        let message = legacy_migration_recovery_message(&failure, Path::new("/tmp/profile"));
        assert!(message.contains("数据目录：/tmp/profile"));
        assert!(message.contains("备份可能位于该目录"));
        assert!(!message.contains("备份位置：/tmp/profile"));
    }

    #[test]
    fn format_decision_toggles_existing_style_and_preserves_clear_text() {
        assert_eq!(
            format_decision(TextFormat::Bold, true),
            FormatDecision::Remove,
        );
        assert_eq!(
            format_decision(TextFormat::Italic, false),
            FormatDecision::Add,
        );
        assert_eq!(
            format_decision(TextFormat::Underline, true),
            FormatDecision::Remove,
        );
        assert_eq!(
            format_decision(TextFormat::Clear, true),
            FormatDecision::Clear,
        );
    }

    #[test]
    fn format_target_distinguishes_selection_from_typing() {
        assert_eq!(format_target(NSRange::new(4, 3)), FormatTarget::Selection);
        assert_eq!(format_target(NSRange::new(4, 0)), FormatTarget::Typing);
    }

    #[test]
    fn paste_route_only_imports_for_body_with_current_note() {
        assert_eq!(paste_route(true, true), PasteRoute::BodyImporter);
        assert_eq!(paste_route(false, true), PasteRoute::NativeResponder);
        assert_eq!(paste_route(true, false), PasteRoute::NativeResponder);
    }

    #[test]
    fn bounded_file_reader_rejects_large_fifo_and_symlink_inputs() {
        let temp = tempdir().unwrap();
        let large = temp.path().join("large.png");
        fs::write(
            &large,
            vec![0u8; joplin_lite_native::resource_store::MAX_IMAGE_BYTES + 1],
        )
        .unwrap();
        assert_eq!(
            read_regular_image_file(&large),
            Err(PasteFileError::TooLarge)
        );

        let regular = temp.path().join("regular.png");
        fs::write(&regular, b"not-an-image").unwrap();
        let link = temp.path().join("link.png");
        std::os::unix::fs::symlink(&regular, &link).unwrap();
        assert_eq!(read_regular_image_file(&link), Err(PasteFileError::Invalid));

        let fifo = temp.path().join("pipe.png");
        let fifo_c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert_eq!(read_regular_image_file(&fifo), Err(PasteFileError::Invalid));
    }

    #[test]
    fn drag_file_reader_accepts_only_local_png_or_jpeg_images() {
        const TINY_PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00,
            0x00, 0xb5, 0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0x64, 0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let temp = tempdir().unwrap();
        let png = temp.path().join("drop.png");
        fs::write(&png, TINY_PNG).unwrap();
        assert!(matches!(
            read_drag_image_file(&png),
            Ok(PasteboardImage::Data { mime, .. }) if mime == "image/png"
        ));

        let wrong_extension = temp.path().join("drop.tiff");
        fs::write(&wrong_extension, TINY_PNG).unwrap();
        assert!(matches!(
            read_drag_image_file(&wrong_extension),
            Err(PasteFileError::Invalid)
        ));
    }

    #[test]
    fn file_url_wins_over_finder_icon_previews_on_the_pasteboard() {
        const ICON_PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00,
            0x00, 0xb5, 0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0x64, 0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let temp = tempdir().unwrap();
        let source = temp.path().join("original.jpg");
        let rep = NSBitmapImageRep::initWithData(
            NSBitmapImageRep::alloc(),
            &NSData::with_bytes(ICON_PNG),
        )
        .unwrap();
        let empty_keys: [&NSString; 0] = [];
        let empty_values: [&AnyObject; 0] = [];
        let properties =
            NSDictionary::<NSString, AnyObject>::from_slices(&empty_keys, &empty_values);
        let source_data = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::JPEG, &properties)
        }
        .unwrap();
        fs::write(&source, source_data.to_vec()).unwrap();
        let source_bytes = fs::read(&source).unwrap();
        let icon_tiff = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::TIFF, &properties)
        }
        .unwrap();
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        pasteboard.clearContents();
        assert!(
            pasteboard.setData_forType(Some(&NSData::with_bytes(ICON_PNG)), unsafe {
                NSPasteboardTypePNG
            },)
        );
        assert!(pasteboard.setData_forType(Some(&icon_tiff), unsafe { NSPasteboardTypeTIFF },));
        let source_url = NSURL::fileURLWithPath(&NSString::from_str(source.to_str().unwrap()));
        let source_url_string = source_url.absoluteString().unwrap();
        assert!(
            pasteboard.setString_forType(&source_url_string, unsafe { NSPasteboardTypeFileURL },)
        );

        match read_pasteboard_image_from(&pasteboard) {
            PasteboardImage::Data { bytes, title, mime } => {
                assert_eq!(bytes, source_bytes);
                assert_eq!(title, "original.jpg");
                assert_eq!(mime, "image/jpeg");
            }
            PasteboardImage::NotImage => panic!("expected source file, got no image"),
            PasteboardImage::Rejected(message) => {
                panic!("expected source file, got rejection: {message}")
            }
        }
    }

    #[test]
    fn invalid_file_url_rejects_without_falling_back_to_icon_pixels() {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        pasteboard.clearContents();
        assert!(
            pasteboard.setData_forType(Some(&NSData::with_bytes(TEST_PNG)), unsafe {
                NSPasteboardTypePNG
            },)
        );
        let remote_url = NSString::from_str("file://remote.example/image.png");
        assert!(pasteboard.setString_forType(&remote_url, unsafe { NSPasteboardTypeFileURL },));

        assert!(matches!(
            read_pasteboard_image_from(&pasteboard),
            PasteboardImage::Rejected("图片未插入：格式不支持")
        ));
    }

    #[test]
    fn pixel_png_without_file_url_remains_supported() {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        pasteboard.clearContents();
        assert!(
            pasteboard.setData_forType(Some(&NSData::with_bytes(TEST_PNG)), unsafe {
                NSPasteboardTypePNG
            },)
        );

        match read_pasteboard_image_from(&pasteboard) {
            PasteboardImage::Data { bytes, title, mime } => {
                assert_eq!(bytes, TEST_PNG);
                assert_eq!(title, "clipboard.png");
                assert_eq!(mime, "image/png");
            }
            PasteboardImage::NotImage => panic!("expected pixel clipboard image"),
            PasteboardImage::Rejected(message) => panic!("unexpected rejection: {message}"),
        }
    }

    #[test]
    fn image_signatures_match_only_the_declared_png_or_jpeg_mime() {
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
        const JPEG: &[u8] = b"\xff\xd8\xff\xe0";
        const GIF: &[u8] = b"GIF89a";
        const TIFF: &[u8] = b"II*\0";
        assert!(image_signature_matches_mime(PNG, "image/png"));
        assert!(image_signature_matches_mime(JPEG, "image/jpeg"));
        assert!(!image_signature_matches_mime(PNG, "image/jpeg"));
        assert!(!image_signature_matches_mime(JPEG, "image/png"));
        assert!(!image_signature_matches_mime(GIF, "image/png"));
        assert!(!image_signature_matches_mime(TIFF, "image/jpeg"));

        const TINY_PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00,
            0x00, 0xb5, 0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0x64, 0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let rep = NSBitmapImageRep::initWithData(
            NSBitmapImageRep::alloc(),
            &NSData::with_bytes(TINY_PNG),
        )
        .unwrap();
        let empty_keys: [&NSString; 0] = [];
        let empty_values: [&AnyObject; 0] = [];
        let properties =
            NSDictionary::<NSString, AnyObject>::from_slices(&empty_keys, &empty_values);
        let jpeg = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::JPEG, &properties)
        }
        .unwrap();
        assert!(valid_image_bytes_for_mime(&jpeg.to_vec(), "image/jpeg"));
        let truncated_png = TINY_PNG[..32].to_vec();
        let truncated_jpeg = jpeg.to_vec()[..32].to_vec();
        assert!(image_signature_matches_mime(&truncated_png, "image/png"));
        assert!(image_signature_matches_mime(&truncated_jpeg, "image/jpeg"));
        assert!(!valid_image_bytes_for_mime(&truncated_png, "image/png"));
        assert!(!valid_image_bytes_for_mime(&truncated_jpeg, "image/jpeg"));
        assert!(!valid_image_bytes_for_mime(GIF, "image/png"));
        assert!(!valid_image_bytes_for_mime(TIFF, "image/jpeg"));
    }

    #[test]
    fn drag_url_and_promise_guards_reject_remote_and_promised_payloads() {
        assert!(is_local_file_url_host(None));
        assert!(is_local_file_url_host(Some("")));
        assert!(is_local_file_url_host(Some("localhost")));
        assert!(!is_local_file_url_host(Some("remote.example")));
        assert!(is_promised_pasteboard_type("NSFilesPromisePboardType"));
        assert!(is_promised_pasteboard_type(
            "com.apple.pasteboard.promised-file-url"
        ));
        assert!(!is_promised_pasteboard_type("public.file-url"));
    }

    #[test]
    fn image_candidate_is_built_off_view_without_mutating_source() {
        let source = NSMutableAttributedString::from_nsstring(&NSString::from_str("hello"));
        let inline = NSMutableAttributedString::from_nsstring(&NSString::from_str("[图片]"));
        let source_ref: &NSAttributedString = &source;
        let candidate = candidate_with_attachment(source_ref, NSRange::new(1, 1), &inline).unwrap();
        assert_eq!(source.string().to_string(), "hello");
        assert_eq!(candidate.string().to_string(), "h[图片]llo");
    }

    #[test]
    fn persistence_gate_skips_live_apply_when_persistence_fails() {
        let mut apply_count = 0;
        assert!(!commit_after_persistence(
            || false,
            || {
                apply_count += 1;
            },
        ));
        assert_eq!(apply_count, 0);
    }

    #[test]
    fn data_directory_override_must_be_absolute_and_default_must_exist() {
        let default = PathBuf::from("/tmp/joplin-lite-native-default");
        assert_eq!(
            choose_data_dir(Some(Path::new("relative-profile")), Some(&default)),
            Err(DataDirError::OverrideMustBeAbsolute),
        );
        assert_eq!(
            choose_data_dir(None, None),
            Err(DataDirError::DefaultDirectoryUnavailable),
        );
    }

    #[test]
    fn canonical_data_directory_rejects_official_profile_and_children() {
        let temp = tempdir().unwrap();
        let official = temp.path().join("Joplin").join("Joplin Desktop");
        fs::create_dir_all(&official).unwrap();
        let alias = temp.path().join("profile-alias");
        std::os::unix::fs::symlink(&official, &alias).unwrap();
        let canonical_official = fs::canonicalize(&official).unwrap();
        let canonical_alias = fs::canonicalize(&alias).unwrap();
        assert_eq!(
            validate_canonical_data_dir(
                &canonical_alias,
                std::slice::from_ref(&canonical_official),
            ),
            Err(DataDirError::OfficialJoplinProfile),
        );
        let child = canonical_official.join("native-profile");
        assert_eq!(
            validate_canonical_data_dir(&child, &[canonical_official]),
            Err(DataDirError::OfficialJoplinProfile),
        );

        let mac_joplin_root = temp
            .path()
            .join("Library")
            .join("Application Support")
            .join("Joplin");
        fs::create_dir_all(mac_joplin_root.join("Joplin Desktop")).unwrap();
        let canonical_mac_root = fs::canonicalize(&mac_joplin_root).unwrap();
        assert_eq!(
            validate_canonical_data_dir(
                &canonical_mac_root.join("native-profile"),
                std::slice::from_ref(&canonical_mac_root),
            ),
            Err(DataDirError::OfficialJoplinProfile),
        );
    }

    #[test]
    fn notes_database_rejects_a_real_symlink() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("target.sqlite");
        fs::write(&target, b"not a database").unwrap();
        let link = temp.path().join("notes.sqlite");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            ensure_notes_database_file(&link),
            Err(DataFileError::Symlink)
        );
    }

    #[test]
    fn notes_database_missing_file_is_exclusively_created() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("notes.sqlite");
        ensure_notes_database_file(&path).unwrap();
        assert!(fs::symlink_metadata(&path).unwrap().is_file());
        assert_eq!(ensure_notes_database_file(&path), Ok(()));
    }

    #[test]
    fn typing_format_removal_uses_not_have_trait() {
        assert_eq!(
            typing_trait_operation(TextFormat::Bold, FormatDecision::Remove),
            FontTraitOperation::NotHave,
        );
        assert_eq!(
            typing_trait_operation(TextFormat::Italic, FormatDecision::Add),
            FontTraitOperation::Have,
        );
    }
}
