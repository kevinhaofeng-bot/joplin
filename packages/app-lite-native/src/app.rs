use joplin_lite_native::body::{markdown_marker, marker_spans};
use joplin_lite_native::core::{
    CreateNote, HtmlNoteConversion, LegacyNoteForHtmlMigration, Note, NoteContentUpdate,
    NoteListItem, NoteRepository, ResourceImport,
};
use joplin_lite_native::html_body::{
    Alignment, Block, Document, HtmlBodyError, Inline, Marks, parse_html, resource_ids,
    search_text, serialize_html,
};
#[cfg(test)]
use joplin_lite_native::native_editor::LinkSelectionState;
use joplin_lite_native::native_editor::{
    BlockCommand, EditorCodecError as NativeEditorCodecError, EmptyBlockCarrier, InlineCommand,
    NativeEditorSession, ParagraphCommand, RenderedAttachment, RenderedDocument,
    RenderedProjectionPrefix, SelectionState, apply_block_command, apply_clear_formatting,
    apply_committed_text_delta, apply_inline_command, apply_link, apply_paragraph_command,
    delete_image_anchor_if_identity, document_from_session, editor_attachment_image,
    effective_typing_format_at, image_paragraph_tail_indent, insert_image_block_anchor,
    projection_range_from_semantic, query_block_state, query_clear_state,
    query_inline_applicability, query_inline_state, query_link_selection,
    query_paragraph_command_state, render_session, semantic_range_from_projection,
    semantic_text_from_projection, session_from_document,
};
use joplin_lite_native::native_note_browser::{
    PreviewListUpdate, ThumbnailCache, ThumbnailKey, ThumbnailRequest, ThumbnailRequestLedger,
    configure_note_card, make_note_collection_view, preview_list_update,
    restore_selection_after_failed_switch, selected_index_for_id, thumbnail_display_size,
};
use joplin_lite_native::note_preview::{NotePreview, preview_from_list_item};
use joplin_lite_native::resource_store::MAX_IMAGE_BYTES;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSAttachmentAttributeName,
    NSAttributedStringAppKitDocumentFormats, NSAttributedStringAttachmentConveniences,
    NSBackgroundColorAttributeName, NSBackingStoreType, NSBezelStyle, NSBitmapImageFileType,
    NSBitmapImageRep, NSBorderType, NSBox, NSBoxType, NSButton, NSButtonType, NSCellImagePosition,
    NSCollectionView, NSCollectionViewDataSource, NSCollectionViewDelegate,
    NSCollectionViewFlowLayout, NSCollectionViewItem, NSColor, NSControlStateValueMixed,
    NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate, NSDragOperation,
    NSDraggingDestination, NSDraggingInfo, NSEventModifierFlags, NSFont, NSFontAttributeName,
    NSImage, NSIndexPathNSCollectionViewAdditions, NSLayoutManager, NSLineBreakMode,
    NSLinkAttributeName, NSMenu, NSMenuItem, NSModalResponseOK, NSMutableParagraphStyle,
    NSOpenPanel, NSParagraphStyle, NSParagraphStyleAttributeName, NSPasteboard,
    NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    NSResponder, NSScrollView, NSSearchField, NSSearchFieldDelegate,
    NSStrikethroughStyleAttributeName, NSText, NSTextAlignment, NSTextAttachment, NSTextDelegate,
    NSTextField, NSTextFieldDelegate, NSTextInputClient, NSTextStorage, NSTextView,
    NSTextViewDelegate, NSUnderlineStyle, NSUnderlineStyleAttributeName, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSData, NSDictionary,
    NSIndexPath, NSMutableAttributedString, NSMutableCopying, NSNotification, NSNumber, NSObject,
    NSObjectProtocol, NSPoint, NSRange, NSRect, NSSet, NSSize, NSString, NSURL, ns_string,
};
use objc2_image_io::{
    CGImageSource, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceThumbnailMaxPixelSize,
};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::{NonNull, null_mut};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use text_document::TextFormat as NativeTextFormat;

const RESOURCE_ID_ATTRIBUTE: &str = "com.kevinhao.joplin-lite.resource-id";
const RESOURCE_ALT_ATTRIBUTE: &str = "com.kevinhao.joplin-lite.resource-alt";
const MISSING_RESOURCE_ATTRIBUTE: &str = "com.kevinhao.joplin-lite.missing-resource";

struct PreparedNoteContent {
    update: NoteContentUpdate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingEditorIntent {
    range: NSRange,
    semantic_range: Option<NSRange>,
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
    baseline_semantic_range: Option<NSRange>,
    current_view_text: String,
    current_marked_range: NSRange,
    replacement: String,
    covered_attachments: Vec<RenderedAttachment>,
}

const THUMBNAIL_QUEUE_CAPACITY: usize = 32;
const THUMBNAIL_WORKER_COUNT: usize = 2;

struct ThumbnailJob {
    key: ThumbnailKey,
    repository: Arc<NoteRepository>,
}

struct ThumbnailCompletion {
    key: ThumbnailKey,
    image: Option<CFRetained<CGImage>>,
    pixels: Option<(usize, usize)>,
}

struct ThumbnailRuntime {
    jobs: SyncSender<ThumbnailJob>,
    completions: Arc<Mutex<VecDeque<ThumbnailCompletion>>>,
}

static THUMBNAIL_RUNTIME: OnceLock<ThumbnailRuntime> = OnceLock::new();

fn thumbnail_runtime() -> &'static ThumbnailRuntime {
    THUMBNAIL_RUNTIME.get_or_init(|| {
        let (jobs, receiver) = sync_channel::<ThumbnailJob>(THUMBNAIL_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let completions = Arc::new(Mutex::new(VecDeque::new()));
        for worker in 0..THUMBNAIL_WORKER_COUNT {
            let receiver = Arc::clone(&receiver);
            let completions = Arc::clone(&completions);
            thread::Builder::new()
                .name(format!("joplin-thumbnail-{worker}"))
                .spawn(move || {
                    loop {
                        let job = receiver.lock().expect("thumbnail queue poisoned").recv();
                        let Ok(job) = job else {
                            break;
                        };
                        let image = load_downsampled_thumbnail(&job.repository, &job.key);
                        let pixels = image.as_ref().map(|image| {
                            (CGImage::width(Some(image)), CGImage::height(Some(image)))
                        });
                        completions
                            .lock()
                            .expect("thumbnail completion queue poisoned")
                            .push_back(ThumbnailCompletion {
                                key: job.key,
                                image,
                                pixels,
                            });
                    }
                })
                .expect("thumbnail worker thread must start");
        }
        ThumbnailRuntime { jobs, completions }
    })
}

fn submit_thumbnail_job(job: ThumbnailJob) -> bool {
    match thumbnail_runtime().jobs.try_send(job) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
    }
}

fn downsampled_thumbnail_image(bytes: &[u8], max_pixel_size: usize) -> Option<CFRetained<CGImage>> {
    if bytes.is_empty() || max_pixel_size == 0 {
        return None;
    }
    let data = CFData::from_bytes(bytes);
    let always: CFRetained<CFType> = CFBoolean::new(true).into();
    let transform: CFRetained<CFType> = CFBoolean::new(true).into();
    let max_size: CFRetained<CFType> = CFNumber::new_isize(max_pixel_size as isize).into();
    let keys: [&CFString; 3] = unsafe {
        [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceThumbnailMaxPixelSize,
            kCGImageSourceCreateThumbnailWithTransform,
        ]
    };
    let values: [&CFType; 3] = [always.as_ref(), max_size.as_ref(), transform.as_ref()];
    let options = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    let options: &CFDictionary = unsafe { options.cast_unchecked() };
    let source = unsafe { CGImageSource::with_data(&data, Some(options)) }?;
    let image = unsafe { source.thumbnail_at_index(0, Some(options)) }?;
    let pixels = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    thumbnail_pixels_within_bound(pixels, max_pixel_size).then_some(image)
}

fn load_downsampled_thumbnail(
    repository: &NoteRepository,
    key: &ThumbnailKey,
) -> Option<CFRetained<CGImage>> {
    let resource = repository.get_resource(&key.resource_id).ok().flatten()?;
    if !matches!(resource.mime.as_str(), "image/png" | "image/jpeg")
        || !image_signature_matches_mime(&resource.bytes, &resource.mime)
    {
        return None;
    }
    downsampled_thumbnail_image(&resource.bytes, key.target_size as usize)
}

fn thumbnail_pixels_within_bound(pixels: (usize, usize), max_pixel_size: usize) -> bool {
    pixels.0 > 0 && pixels.1 > 0 && pixels.0 <= max_pixel_size && pixels.1 <= max_pixel_size
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyPasteDispatch {
    DirectTextInsertion,
    ResponderPaste,
}

fn paste_route(body_is_first_responder: bool, has_current_note: bool) -> PasteRoute {
    if body_is_first_responder && has_current_note {
        PasteRoute::BodyImporter
    } else {
        PasteRoute::NativeResponder
    }
}

fn body_paste_dispatch(body_is_first_responder: bool) -> BodyPasteDispatch {
    if body_is_first_responder {
        BodyPasteDispatch::DirectTextInsertion
    } else {
        BodyPasteDispatch::ResponderPaste
    }
}

fn should_relayout_editor_after_preview_update(
    update: &PreviewListUpdate,
    preserve_editor_geometry: bool,
) -> bool {
    !preserve_editor_geometry && matches!(update, PreviewListUpdate::ReloadAll)
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
    let semantic_range = intent.semantic_range.unwrap_or(intent.range);
    let Some((start, end)) = utf16_scalar_range(&intent.old_semantic_text, semantic_range) else {
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
                range: semantic_range,
                resource_id: intent.covered_attachments[0].resource_id.clone(),
            };
        }
        return PendingIntentDecision::Reject;
    }
    if old_slice == intent.replacement {
        PendingIntentDecision::Noop
    } else {
        PendingIntentDecision::ApplyText {
            range: semantic_range,
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

fn preflight_projected_editor_intent(
    intent: &PendingEditorIntent,
    prefixes: &[RenderedProjectionPrefix],
) -> Result<(), PendingIntentRejection> {
    if semantic_text_from_projection(&intent.old_view_text, prefixes).as_deref()
        != Some(intent.old_semantic_text.as_str())
    {
        return Err(PendingIntentRejection::StaleBaseline);
    }
    let semantic_range = intent.semantic_range.unwrap_or(intent.range);
    let mut semantic_intent = intent.clone();
    semantic_intent.range = semantic_range;
    semantic_intent.semantic_range = None;
    semantic_intent.old_view_text = semantic_intent.old_semantic_text.clone();
    preflight_pending_editor_intent(&semantic_intent)
}

fn append_pending_editor_intent(
    intents: &mut Vec<PendingEditorIntent>,
    intent: PendingEditorIntent,
) -> bool {
    if !intents.is_empty() {
        if intents.last() == Some(&intent) {
            return true;
        }
        intents.clear();
        return false;
    }
    intents.push(intent);
    true
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
                semantic_range: None,
                replacement: replacement.to_owned(),
                old_view_text: old_view_text.to_owned(),
                old_semantic_text: old_semantic_text.to_owned(),
                covered_attachments,
            };
            let mut semantic_intent = intent.clone();
            semantic_intent.old_view_text = semantic_intent.old_semantic_text.clone();
            preflight_pending_editor_intent(&semantic_intent)?;
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
                baseline_semantic_range: None,
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
        semantic_range: composition.baseline_semantic_range,
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

fn adjust_projection_prefixes_with_semantic(
    prefixes: &mut [RenderedProjectionPrefix],
    projection_range: NSRange,
    semantic_range: NSRange,
    replacement_length: usize,
) -> bool {
    let range = projection_range;
    let Some(end) = range.location.checked_add(range.length) else {
        return false;
    };
    if prefixes.iter().any(|prefix| {
        range.length > 0
            && range.location < prefix.projected_range.location + prefix.projected_range.length
            && end > prefix.projected_range.location
    }) {
        return false;
    }
    let delta = replacement_length as isize - range.length as isize;
    let semantic_end = semantic_range
        .location
        .saturating_add(semantic_range.length);
    let semantic_delta = replacement_length as isize - semantic_range.length as isize;
    for prefix in prefixes.iter_mut() {
        if prefix.projected_range.location >= end {
            let shifted = prefix.projected_range.location as isize + delta;
            if shifted < 0 {
                return false;
            }
            prefix.projected_range.location = shifted as usize;
            if prefix.semantic_offset >= semantic_end {
                let semantic_shifted = prefix.semantic_offset as isize + semantic_delta;
                if semantic_shifted < 0 {
                    return false;
                }
                prefix.semantic_offset = semantic_shifted as usize;
            }
        }
    }
    true
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
    fn right(self) -> f64 {
        self.x + self.width
    }

    fn top(self) -> f64 {
        self.y + self.height
    }

    fn ns_rect(self) -> NSRect {
        NSRect::new(
            NSPoint::new(self.x, self.y),
            NSSize::new(self.width, self.height),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellVisibility {
    Default,
    BrowserCollapsed,
    Focus,
    BrowserOnly,
}

fn toggle_focus_visibility(
    current: ShellVisibility,
    restore: Option<ShellVisibility>,
) -> (ShellVisibility, Option<ShellVisibility>) {
    match current {
        ShellVisibility::Focus => (restore.unwrap_or(ShellVisibility::Default), None),
        ShellVisibility::BrowserOnly => (
            ShellVisibility::Focus,
            restore.or(Some(ShellVisibility::Default)),
        ),
        _ => (ShellVisibility::Focus, Some(current)),
    }
}

fn search_focus_visibility(
    current: ShellVisibility,
    restore: Option<ShellVisibility>,
) -> (ShellVisibility, Option<ShellVisibility>) {
    match current {
        ShellVisibility::Focus => (
            match restore {
                Some(visible @ (ShellVisibility::Default | ShellVisibility::BrowserCollapsed)) => {
                    visible
                }
                _ => ShellVisibility::Default,
            },
            None,
        ),
        ShellVisibility::BrowserOnly => (ShellVisibility::Default, None),
        _ => (current, restore),
    }
}

fn toggle_browser_visibility(current: ShellVisibility) -> ShellVisibility {
    match current {
        ShellVisibility::Default => ShellVisibility::BrowserCollapsed,
        ShellVisibility::BrowserCollapsed => ShellVisibility::Default,
        ShellVisibility::Focus => ShellVisibility::BrowserOnly,
        ShellVisibility::BrowserOnly => ShellVisibility::Focus,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ShellLayout {
    navigation: LayoutRect,
    browser: LayoutRect,
    editor: LayoutRect,
    sheet: LayoutRect,
    breadcrumb: LayoutRect,
    title: LayoutRect,
    updated: LayoutRect,
    toolbar: LayoutRect,
    body: LayoutRect,
    status: LayoutRect,
    empty_editor: LayoutRect,
    document_measure: f64,
    body_bottom_inset: f64,
}

fn shell_layout(width: f64, height: f64, visibility: ShellVisibility) -> ShellLayout {
    let width = width.max(1.0);
    let height = height.max(1.0);
    let navigation_width = match visibility {
        ShellVisibility::Focus | ShellVisibility::BrowserOnly => 0.0,
        _ => 192.0,
    };
    let browser_width = match visibility {
        ShellVisibility::Default => 384.0,
        ShellVisibility::BrowserCollapsed | ShellVisibility::Focus => 0.0,
        ShellVisibility::BrowserOnly => 384.0,
    };
    let editor = LayoutRect {
        x: navigation_width + browser_width,
        y: 0.0,
        width: (width - navigation_width - browser_width).max(0.0),
        height,
    };
    let navigation = LayoutRect {
        x: 0.0,
        y: 0.0,
        width: navigation_width,
        height,
    };
    let browser = LayoutRect {
        x: navigation.right(),
        y: 0.0,
        width: browser_width,
        height,
    };
    let sheet_margin = 24.0_f64.min((editor.width / 2.0).max(0.0));
    let sheet = LayoutRect {
        x: editor.x + sheet_margin,
        y: 16.0_f64.min(height / 2.0),
        width: (editor.width - sheet_margin * 2.0).max(0.0),
        height: (height - 32.0).max(0.0),
    };
    let document_measure = 680.0_f64.min((sheet.width - 64.0).max(0.0));
    let content_x = sheet.x + ((sheet.width - document_measure) / 2.0).max(0.0);
    let content_width = document_measure;
    let breadcrumb = LayoutRect {
        x: content_x,
        y: sheet.top() - 42.0,
        width: (content_width - 140.0).max(0.0),
        height: 18.0,
    };
    let title = LayoutRect {
        x: content_x,
        y: breadcrumb.y - 42.0,
        width: content_width,
        height: 34.0,
    };
    let updated = LayoutRect {
        x: content_x,
        y: title.y - 24.0,
        width: content_width,
        height: 14.0,
    };
    let toolbar = LayoutRect {
        x: content_x,
        y: title.y - 58.0,
        width: content_width,
        height: 30.0,
    };
    let status = LayoutRect {
        x: (content_x + content_width - 174.0).max(sheet.x + 12.0),
        y: sheet.y + 14.0,
        width: 164.0_f64.min(sheet.width.max(0.0)),
        height: 18.0,
    };
    let body_bottom_inset = (height * 0.30).clamp(96.0, 260.0);
    let body = LayoutRect {
        x: content_x,
        y: sheet.y + 48.0,
        width: content_width,
        height: (toolbar.y - (sheet.y + 48.0) - 16.0).max(120.0),
    };
    let empty_editor = LayoutRect {
        x: body.x,
        y: body.y + body.height * 0.45 - 22.0,
        width: body.width,
        height: 44.0,
    };
    ShellLayout {
        navigation,
        browser,
        editor,
        sheet,
        breadcrumb,
        title,
        updated,
        toolbar,
        body,
        status,
        empty_editor,
        document_measure,
        body_bottom_inset,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum EditorAction {
    InsertImage,
    Undo,
    Redo,
    BlockStyle,
    Bold,
    Italic,
    Underline,
    Highlight,
    BulletList,
    OrderedList,
    Checklist,
    More,
    Link,
    AlignLeft,
    AlignCenter,
    AlignRight,
    IncreaseIndent,
    DecreaseIndent,
    Strikethrough,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorActionGroup {
    Insert,
    History,
    Block,
    Inline,
    Lists,
    More,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorActionKind {
    Momentary,
    Toggle,
    Popup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EditorActionDescriptor {
    action: EditorAction,
    label: &'static str,
    group: EditorActionGroup,
    fixed: bool,
    wide_only: bool,
}

impl EditorActionDescriptor {
    fn kind(self) -> EditorActionKind {
        editor_action_kind(self.action)
    }
}

const EDITOR_ACTION_CATALOGUE: &[EditorActionDescriptor] = &[
    EditorActionDescriptor {
        action: EditorAction::InsertImage,
        label: "插入图片",
        group: EditorActionGroup::Insert,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::Undo,
        label: "撤销",
        group: EditorActionGroup::History,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::Redo,
        label: "重做",
        group: EditorActionGroup::History,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::BlockStyle,
        label: "正文/标题",
        group: EditorActionGroup::Block,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::Bold,
        label: "粗体",
        group: EditorActionGroup::Inline,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::Italic,
        label: "斜体",
        group: EditorActionGroup::Inline,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::Underline,
        label: "下划线",
        group: EditorActionGroup::Inline,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::Highlight,
        label: "高亮",
        group: EditorActionGroup::Inline,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::BulletList,
        label: "项目符号",
        group: EditorActionGroup::Lists,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::OrderedList,
        label: "编号列表",
        group: EditorActionGroup::Lists,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::Checklist,
        label: "清单",
        group: EditorActionGroup::Lists,
        fixed: true,
        wide_only: false,
    },
    EditorActionDescriptor {
        action: EditorAction::Link,
        label: "链接",
        group: EditorActionGroup::More,
        fixed: false,
        wide_only: true,
    },
    EditorActionDescriptor {
        action: EditorAction::AlignLeft,
        label: "左对齐",
        group: EditorActionGroup::More,
        fixed: false,
        wide_only: true,
    },
    EditorActionDescriptor {
        action: EditorAction::AlignCenter,
        label: "居中",
        group: EditorActionGroup::More,
        fixed: false,
        wide_only: true,
    },
    EditorActionDescriptor {
        action: EditorAction::AlignRight,
        label: "右对齐",
        group: EditorActionGroup::More,
        fixed: false,
        wide_only: true,
    },
    EditorActionDescriptor {
        action: EditorAction::IncreaseIndent,
        label: "增加缩进",
        group: EditorActionGroup::More,
        fixed: false,
        wide_only: true,
    },
    EditorActionDescriptor {
        action: EditorAction::DecreaseIndent,
        label: "减少缩进",
        group: EditorActionGroup::More,
        fixed: false,
        wide_only: true,
    },
    EditorActionDescriptor {
        action: EditorAction::Strikethrough,
        label: "删除线",
        group: EditorActionGroup::More,
        fixed: false,
        wide_only: true,
    },
    EditorActionDescriptor {
        action: EditorAction::Clear,
        label: "清除格式",
        group: EditorActionGroup::More,
        fixed: false,
        wide_only: true,
    },
    EditorActionDescriptor {
        action: EditorAction::More,
        label: "更多",
        group: EditorActionGroup::More,
        fixed: true,
        wide_only: false,
    },
];

fn editor_action_catalogue() -> &'static [EditorActionDescriptor] {
    EDITOR_ACTION_CATALOGUE
}

fn editor_action_kind(action: EditorAction) -> EditorActionKind {
    match action {
        EditorAction::Bold
        | EditorAction::Italic
        | EditorAction::Underline
        | EditorAction::Highlight
        | EditorAction::BulletList
        | EditorAction::OrderedList
        | EditorAction::Checklist
        | EditorAction::Strikethrough => EditorActionKind::Toggle,
        EditorAction::BlockStyle | EditorAction::Link => EditorActionKind::Popup,
        EditorAction::InsertImage
        | EditorAction::Undo
        | EditorAction::Redo
        | EditorAction::More
        | EditorAction::AlignLeft
        | EditorAction::AlignCenter
        | EditorAction::AlignRight
        | EditorAction::IncreaseIndent
        | EditorAction::DecreaseIndent
        | EditorAction::Clear => EditorActionKind::Momentary,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EditorActionPresentation {
    action: EditorAction,
    kind: EditorActionKind,
    label: &'static str,
    state: SelectionState,
    enabled: bool,
}

fn semantic_action_presentation(
    action: EditorAction,
    has_note: bool,
    session: Option<&NativeEditorSession>,
    selection: NSRange,
) -> EditorActionPresentation {
    let kind = editor_action_kind(action);
    let mut presentation = EditorActionPresentation {
        action,
        kind,
        label: compact_toolbar_label(action),
        state: SelectionState::Inactive,
        enabled: has_note && session.is_some(),
    };
    if !has_note {
        return presentation;
    }
    let inline_state = |command| {
        session
            .and_then(|session| query_inline_state(session, selection, command).ok())
            .unwrap_or(SelectionState::Inactive)
    };
    let block_state = |command| {
        session
            .and_then(|session| query_block_state(session, selection, command).ok())
            .unwrap_or(SelectionState::Inactive)
    };
    let paragraph_state = |command| {
        session
            .and_then(|session| query_paragraph_command_state(session, selection, command).ok())
            .unwrap_or(SelectionState::Inactive)
    };
    let inline_applicable = session
        .and_then(|session| query_inline_applicability(session, selection).ok())
        .unwrap_or(false);
    match action {
        EditorAction::Undo => {
            presentation.enabled = session.is_some_and(NativeEditorSession::can_undo);
        }
        EditorAction::Redo => {
            presentation.enabled = session.is_some_and(NativeEditorSession::can_redo);
        }
        EditorAction::BlockStyle => {
            presentation.label = session
                .map(|session| block_style_label(session, selection))
                .unwrap_or("正文");
            presentation.state = block_state(BlockCommand::Paragraph);
            presentation.enabled = session.is_some();
        }
        EditorAction::Bold => {
            presentation.state = inline_state(InlineCommand::Bold);
            presentation.enabled = inline_applicable;
        }
        EditorAction::Italic => {
            presentation.state = inline_state(InlineCommand::Italic);
            presentation.enabled = inline_applicable;
        }
        EditorAction::Underline => {
            presentation.state = inline_state(InlineCommand::Underline);
            presentation.enabled = inline_applicable;
        }
        EditorAction::Highlight => {
            presentation.state = inline_state(InlineCommand::Highlight);
            presentation.enabled = inline_applicable;
        }
        EditorAction::Strikethrough => {
            presentation.state = inline_state(InlineCommand::Strikethrough);
            presentation.enabled = inline_applicable;
        }
        EditorAction::BulletList => presentation.state = block_state(BlockCommand::UnorderedList),
        EditorAction::OrderedList => presentation.state = block_state(BlockCommand::OrderedList),
        EditorAction::Checklist => presentation.state = block_state(BlockCommand::Checklist),
        EditorAction::Link => {
            if let Some(link_selection) =
                session.and_then(|session| query_link_selection(session, selection).ok())
            {
                presentation.state = link_selection.state;
                presentation.enabled = link_selection.has_linkable_text;
            } else {
                presentation.enabled = false;
            }
        }
        EditorAction::AlignLeft => {
            presentation.state = paragraph_state(ParagraphCommand::Align(Alignment::Left));
            presentation.enabled = session.is_some();
        }
        EditorAction::AlignCenter => {
            presentation.state = paragraph_state(ParagraphCommand::Align(Alignment::Center));
            presentation.enabled = session.is_some();
        }
        EditorAction::AlignRight => {
            presentation.state = paragraph_state(ParagraphCommand::Align(Alignment::Right));
            presentation.enabled = session.is_some();
        }
        EditorAction::IncreaseIndent => {
            presentation.state = paragraph_state(ParagraphCommand::IncreaseIndent);
            presentation.enabled = presentation.state != SelectionState::Active;
        }
        EditorAction::DecreaseIndent => {
            presentation.state = paragraph_state(ParagraphCommand::DecreaseIndent);
            presentation.enabled = presentation.state != SelectionState::Active;
        }
        EditorAction::Clear => {
            presentation.state = session
                .and_then(|session| query_clear_state(session, selection).ok())
                .unwrap_or(SelectionState::Inactive);
            presentation.enabled = presentation.state != SelectionState::Inactive;
        }
        EditorAction::InsertImage | EditorAction::More => {}
    }
    // Boundary-specific branches may derive an enabled state from an
    // inactive query.  A live semantic session remains the common
    // prerequisite for every catalogue action.
    presentation.enabled = presentation.enabled && session.is_some();
    presentation
}

fn more_has_enabled_child(
    actions: &[EditorAction],
    has_note: bool,
    session: Option<&NativeEditorSession>,
    selection: NSRange,
) -> bool {
    !actions.is_empty()
        && actions.iter().any(|action| {
            semantic_action_presentation(*action, has_note, session, selection).enabled
        })
}

fn toolbar_actions_for_width(width: f64) -> Vec<EditorAction> {
    let mut visible = Vec::new();
    for descriptor in EDITOR_ACTION_CATALOGUE
        .iter()
        .filter(|descriptor| descriptor.action != EditorAction::More)
    {
        if !(descriptor.fixed || (descriptor.wide_only && width >= 680.0)) {
            continue;
        }
        let mut candidate = visible.clone();
        candidate.push(descriptor.action);
        candidate.push(EditorAction::More);
        if toolbar_required_width(&candidate) <= width.max(0.0) || visible.is_empty() {
            visible.push(descriptor.action);
        }
    }
    visible.push(EditorAction::More);
    visible
}

fn toolbar_required_width(actions: &[EditorAction]) -> f64 {
    if actions.is_empty() {
        return 0.0;
    }
    let group_gaps = actions
        .windows(2)
        .filter(|pair| action_group(pair[0]) != action_group(pair[1]))
        .count() as f64
        * 4.0;
    actions.len() as f64 * 32.0 + group_gaps
}

fn toolbar_overflow_actions_for_width(width: f64) -> Vec<EditorAction> {
    let visible = toolbar_actions_for_width(width);
    EDITOR_ACTION_CATALOGUE
        .iter()
        .filter(|descriptor| !descriptor.fixed && !visible.contains(&descriptor.action))
        .map(|descriptor| descriptor.action)
        .collect()
}

fn toolbar_more_enabled_for_layout(
    _width: f64,
    _height: f64,
    _visibility: ShellVisibility,
    has_note: bool,
) -> bool {
    // More always contains the destructive "Delete note" item when a note is
    // selected. It must therefore remain reachable even when every formatting
    // action fits in the current toolbar (wide and focus layouts).
    has_note
}

fn should_sync_caret_context(
    selection_sync_guard: bool,
    loading_guard: bool,
    body_marked: bool,
    pending_composition: bool,
) -> bool {
    !selection_sync_guard && !loading_guard && !body_marked && !pending_composition
}

fn should_sync_caret_context_after_delegate(
    selection_sync_guard: bool,
    loading_guard: bool,
    body_marked: bool,
    pending_intent: bool,
    pending_composition: bool,
) -> bool {
    should_sync_caret_context(
        selection_sync_guard,
        loading_guard,
        body_marked,
        pending_composition,
    ) && !pending_intent
}

fn command_selection_is_available(
    current_note_id: Option<&str>,
    selection_note_id: Option<&str>,
    non_body_focus: bool,
) -> bool {
    !non_body_focus && current_note_id.is_some() && current_note_id == selection_note_id
}

fn sync_caret_after_editor_event(
    session: &mut NativeEditorSession,
    result: EditorSessionSyncResult,
    selection: NSRange,
    body_marked: bool,
    loading_guard: bool,
) {
    if matches!(
        result,
        EditorSessionSyncResult::Noop | EditorSessionSyncResult::Applied
    ) && should_sync_caret_context(false, loading_guard, body_marked, false)
    {
        session.sync_caret_context(selection);
    }
}

fn autosave_timer_defer_allowed(marked_text: bool, loading_guard: bool) -> bool {
    marked_text && !loading_guard
}

fn compact_toolbar_label(action: EditorAction) -> &'static str {
    match action {
        EditorAction::InsertImage => "图",
        EditorAction::Undo => "撤",
        EditorAction::Redo => "重",
        EditorAction::BlockStyle => "正文",
        EditorAction::Bold => "B",
        EditorAction::Italic => "I",
        EditorAction::Underline => "U",
        EditorAction::Highlight => "高亮",
        EditorAction::BulletList => "•",
        EditorAction::OrderedList => "1.",
        EditorAction::Checklist => "☑",
        EditorAction::More => "…",
        EditorAction::Link => "链",
        EditorAction::AlignLeft => "左",
        EditorAction::AlignCenter => "中",
        EditorAction::AlignRight => "右",
        EditorAction::IncreaseIndent => ">",
        EditorAction::DecreaseIndent => "<",
        EditorAction::Strikethrough => "删",
        EditorAction::Clear => "清",
    }
}

fn toolbar_symbol_name(action: EditorAction) -> Option<&'static str> {
    Some(match action {
        EditorAction::InsertImage => "photo",
        EditorAction::Undo => "arrow.uturn.backward",
        EditorAction::Redo => "arrow.uturn.forward",
        EditorAction::Bold => "bold",
        EditorAction::Italic => "italic",
        EditorAction::Underline => "underline",
        EditorAction::Highlight => "highlighter",
        EditorAction::BulletList => "list.bullet",
        EditorAction::OrderedList => "list.number",
        EditorAction::Checklist => "checklist",
        EditorAction::More => "ellipsis",
        EditorAction::Link => "link",
        EditorAction::AlignLeft => "text.alignleft",
        EditorAction::AlignCenter => "text.aligncenter",
        EditorAction::AlignRight => "text.alignright",
        EditorAction::IncreaseIndent => "increase.indent",
        EditorAction::DecreaseIndent => "decrease.indent",
        EditorAction::Strikethrough => "strikethrough",
        EditorAction::Clear => "textformat",
        EditorAction::BlockStyle => return None,
    })
}

fn action_group(action: EditorAction) -> EditorActionGroup {
    EDITOR_ACTION_CATALOGUE
        .iter()
        .find(|descriptor| descriptor.action == action)
        .map(|descriptor| descriptor.group)
        .unwrap_or(EditorActionGroup::More)
}

fn block_style_label(session: &NativeEditorSession, selection: NSRange) -> &'static str {
    for (command, label) in [
        (
            BlockCommand::Heading(joplin_lite_native::html_body::HeadingLevel::One),
            "H1",
        ),
        (
            BlockCommand::Heading(joplin_lite_native::html_body::HeadingLevel::Two),
            "H2",
        ),
        (
            BlockCommand::Heading(joplin_lite_native::html_body::HeadingLevel::Three),
            "H3",
        ),
        (BlockCommand::Paragraph, "正文"),
    ] {
        if query_block_state(session, selection, command).ok() == Some(SelectionState::Active) {
            return label;
        }
    }
    "混合"
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AutosaveDecision {
    Stale,
    Noop,
    Persist {
        generation: u64,
        title: String,
        html: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AutosaveState {
    note_id: Option<String>,
    epoch: u64,
    persisted_title: String,
    persisted_html: String,
    pending_title: String,
    pending_html: String,
    generation: u64,
    scheduled_generation: Option<u64>,
    scheduled_epoch: Option<u64>,
    deferred_generation: Option<u64>,
    deferred_epoch: Option<u64>,
    retry_generation: Option<u64>,
    retry_epoch: Option<u64>,
    retry_attempt: u8,
    dirty: bool,
}

const AUTOSAVE_MAX_RETRIES: u8 = 5;
const AUTOSAVE_BASE_DELAY_SECONDS: f64 = 0.3;

impl AutosaveState {
    fn loaded(note_id: &str, title: &str, html: &str) -> Self {
        Self {
            note_id: Some(note_id.to_owned()),
            epoch: 1,
            persisted_title: title.to_owned(),
            persisted_html: html.to_owned(),
            pending_title: title.to_owned(),
            pending_html: html.to_owned(),
            generation: 0,
            scheduled_generation: None,
            scheduled_epoch: None,
            deferred_generation: None,
            deferred_epoch: None,
            retry_generation: None,
            retry_epoch: None,
            retry_attempt: 0,
            dirty: false,
        }
    }

    fn empty() -> Self {
        Self {
            note_id: None,
            epoch: 0,
            persisted_title: String::new(),
            persisted_html: String::new(),
            pending_title: String::new(),
            pending_html: String::new(),
            generation: 0,
            scheduled_generation: None,
            scheduled_epoch: None,
            deferred_generation: None,
            deferred_epoch: None,
            retry_generation: None,
            retry_epoch: None,
            retry_attempt: 0,
            dirty: false,
        }
    }

    fn reset(&mut self, note_id: &str, title: &str, html: &str) {
        let epoch = self.epoch.saturating_add(1);
        *self = Self::loaded(note_id, title, html);
        self.epoch = epoch;
    }

    fn clear(&mut self) {
        let epoch = self.epoch.saturating_add(1);
        *self = Self::empty();
        self.epoch = epoch;
    }

    fn mark_dirty(&mut self, note_id: &str, title: &str, html: &str) -> Option<u64> {
        if self.note_id.as_deref() != Some(note_id) {
            self.reset(note_id, title, html);
            return None;
        }
        if self.persisted_title == title && self.persisted_html == html {
            self.pending_title = title.to_owned();
            self.pending_html = html.to_owned();
            self.dirty = false;
            self.scheduled_generation = None;
            self.scheduled_epoch = None;
            self.deferred_generation = None;
            self.deferred_epoch = None;
            self.retry_generation = None;
            self.retry_epoch = None;
            self.retry_attempt = 0;
            return None;
        }
        if self.dirty && self.pending_title == title && self.pending_html == html {
            return self.scheduled_generation;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending_title = title.to_owned();
        self.pending_html = html.to_owned();
        self.scheduled_generation = Some(self.generation);
        self.scheduled_epoch = Some(self.epoch);
        self.deferred_generation = None;
        self.deferred_epoch = None;
        self.retry_generation = None;
        self.retry_epoch = None;
        self.retry_attempt = 0;
        self.dirty = true;
        Some(self.generation)
    }

    #[cfg(test)]
    fn timer_decision(&self, note_id: &str, generation: u64) -> AutosaveDecision {
        self.timer_decision_with_epoch(note_id, self.epoch, generation)
    }

    fn timer_decision_with_epoch(
        &self,
        note_id: &str,
        epoch: u64,
        generation: u64,
    ) -> AutosaveDecision {
        if self.note_id.as_deref() != Some(note_id)
            || self.scheduled_epoch != Some(epoch)
            || self.scheduled_generation != Some(generation)
            || !self.dirty
        {
            return AutosaveDecision::Stale;
        }
        if self.persisted_title == self.pending_title && self.persisted_html == self.pending_html {
            return AutosaveDecision::Noop;
        }
        AutosaveDecision::Persist {
            generation,
            title: self.pending_title.clone(),
            html: self.pending_html.clone(),
        }
    }

    fn defer_timer_with_epoch(&mut self, note_id: &str, epoch: u64, generation: u64) -> bool {
        if self.note_id.as_deref() != Some(note_id)
            || self.epoch != epoch
            || self.scheduled_epoch != Some(epoch)
            || self.scheduled_generation != Some(generation)
            || !self.dirty
        {
            return false;
        }
        self.scheduled_generation = None;
        self.scheduled_epoch = None;
        self.deferred_generation = Some(generation);
        self.deferred_epoch = Some(epoch);
        true
    }

    fn resume_deferred_timer_with_epoch(
        &mut self,
        note_id: &str,
        epoch: u64,
        generation: u64,
    ) -> Option<u64> {
        if self.note_id.as_deref() != Some(note_id)
            || self.epoch != epoch
            || self.deferred_epoch != Some(epoch)
            || self.deferred_generation != Some(generation)
            || !self.dirty
        {
            return None;
        }
        self.deferred_generation = None;
        self.deferred_epoch = None;
        self.scheduled_generation = Some(generation);
        self.scheduled_epoch = Some(epoch);
        Some(generation)
    }

    fn flush_decision(&self, note_id: &str) -> AutosaveDecision {
        if self.note_id.as_deref() != Some(note_id) || !self.is_dirty() {
            return AutosaveDecision::Noop;
        }
        AutosaveDecision::Persist {
            generation: self.generation,
            title: self.pending_title.clone(),
            html: self.pending_html.clone(),
        }
    }

    fn mark_saved(&mut self, generation: u64) {
        if generation != self.generation {
            return;
        }
        self.persisted_title = self.pending_title.clone();
        self.persisted_html = self.pending_html.clone();
        self.scheduled_generation = None;
        self.scheduled_epoch = None;
        self.deferred_generation = None;
        self.deferred_epoch = None;
        self.retry_generation = None;
        self.retry_epoch = None;
        self.retry_attempt = 0;
        self.dirty = false;
    }

    #[cfg(test)]
    fn mark_failed(&mut self, generation: u64) -> Option<f64> {
        self.mark_failed_with_epoch(self.epoch, generation)
    }

    fn mark_failed_with_epoch(&mut self, epoch: u64, generation: u64) -> Option<f64> {
        if epoch == self.epoch && generation == self.generation {
            if self.retry_attempt >= AUTOSAVE_MAX_RETRIES {
                self.scheduled_generation = None;
                self.scheduled_epoch = None;
                self.deferred_generation = None;
                self.deferred_epoch = None;
                self.retry_generation = None;
                self.retry_epoch = None;
                self.dirty = true;
                return None;
            }
            self.scheduled_generation = None;
            self.scheduled_epoch = None;
            self.deferred_generation = None;
            self.deferred_epoch = None;
            self.retry_generation = Some(generation);
            self.retry_epoch = Some(epoch);
            self.retry_attempt = self.retry_attempt.saturating_add(1);
            self.dirty = true;
            return self.retry_delay_with_epoch(epoch, generation);
        }
        None
    }

    #[cfg(test)]
    fn retry_delay(&self, generation: u64) -> Option<f64> {
        self.retry_delay_with_epoch(self.epoch, generation)
    }

    fn retry_delay_with_epoch(&self, epoch: u64, generation: u64) -> Option<f64> {
        if self.retry_epoch != Some(epoch)
            || self.retry_generation != Some(generation)
            || !self.dirty
        {
            return None;
        }
        let exponent = self.retry_attempt.saturating_sub(1) as i32;
        Some((AUTOSAVE_BASE_DELAY_SECONDS * 2_f64.powi(exponent)).min(4.8))
    }

    #[cfg(test)]
    fn mark_retry_scheduled(&mut self, generation: u64) {
        self.mark_retry_scheduled_with_epoch(self.epoch, generation);
    }

    fn mark_retry_scheduled_with_epoch(&mut self, epoch: u64, generation: u64) {
        if self.retry_epoch == Some(epoch)
            && self.retry_generation == Some(generation)
            && self.dirty
        {
            self.retry_generation = None;
            self.retry_epoch = None;
            self.scheduled_generation = Some(generation);
            self.scheduled_epoch = Some(epoch);
        }
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }
}

fn should_defer_persistence(body_marked: bool, title_marked: bool, loading: bool) -> bool {
    loading || body_marked || title_marked
}

#[cfg(test)]
fn display_note_title(title: &str, _body: &str) -> String {
    if !title.trim().is_empty() {
        return title.trim().chars().take(120).collect();
    }
    "无标题笔记".to_string()
}

#[cfg(test)]
fn note_list_title(note: &Note) -> String {
    display_note_title(&note.title, &note.body_text)
}

#[cfg(test)]
fn note_list_summary(note: &Note) -> String {
    note.body_text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(52).collect::<String>())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "暂无正文".to_string())
}

fn current_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn format_updated_time(updated_time: i64, now: i64) -> String {
    let elapsed = now.saturating_sub(updated_time);
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
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FormatDecision {
    Add,
    Remove,
    Clear,
}

#[cfg(test)]
fn format_decision(format: TextFormat, active: bool) -> FormatDecision {
    if format == TextFormat::Clear {
        FormatDecision::Clear
    } else if active {
        FormatDecision::Remove
    } else {
        FormatDecision::Add
    }
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FontTraitOperation {
    Have,
    NotHave,
    None,
}

#[cfg(test)]
fn typing_trait_operation(format: TextFormat, decision: FormatDecision) -> FontTraitOperation {
    match (format, decision) {
        (TextFormat::Bold | TextFormat::Italic, FormatDecision::Add) => FontTraitOperation::Have,
        (TextFormat::Bold | TextFormat::Italic, FormatDecision::Remove) => {
            FontTraitOperation::NotHave
        }
        _ => FontTraitOperation::None,
    }
}

#[cfg(test)]
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

fn prepare_image_insert_candidate(
    live: &NativeEditorSession,
    insertion_range: NSRange,
    resource_id: &str,
    alt: &str,
    title: String,
) -> Result<(NativeEditorSession, PreparedNoteContent, NSRange), NativeEditorCodecError> {
    let document = document_from_session(live)?;
    let mut candidate = session_from_document(&document)?;
    let selection =
        insert_image_block_anchor(&mut candidate, insertion_range, resource_id, alt, 1, 1)?;
    let candidate_document = document_from_session(&candidate)?;
    Ok((
        candidate,
        PreparedNoteContent {
            update: NoteContentUpdate {
                title,
                body: serialize_html(&candidate_document),
            },
        },
        selection,
    ))
}

fn toolbar_state_title(action: EditorAction, label: &str) -> Option<&str> {
    (action == EditorAction::BlockStyle).then_some(label)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TypingFormatProjection {
    font_bold: Option<bool>,
    font_italic: Option<bool>,
    font_underline: Option<bool>,
    font_strikeout: Option<bool>,
    has_background: bool,
    clear_background: bool,
    clear_link: bool,
}

fn typing_format_projection(format: &NativeTextFormat) -> TypingFormatProjection {
    TypingFormatProjection {
        font_bold: format.font_bold,
        font_italic: format.font_italic,
        font_underline: format.font_underline,
        font_strikeout: format.font_strikeout,
        has_background: format.background_color.is_some_and(|color| color.alpha > 0),
        clear_background: format.background_color.is_none_or(|color| color.alpha == 0),
        clear_link: format.clear_link || format.anchor_href.is_none(),
    }
}

fn sync_typing_attributes_with_format(body: &NSTextView, format: &NativeTextFormat) {
    let projection = typing_format_projection(format);
    let typing = body.typingAttributes();
    let mutable = typing.mutableCopy();
    let font_key = unsafe { NSFontAttributeName };
    if let Some(font) = unsafe { typing.objectForKey_unchecked(font_key) }
        .and_then(|value| value.downcast_ref::<NSFont>())
    {
        let descriptor = font.fontDescriptor();
        let mut traits = descriptor.symbolicTraits();
        if projection.font_bold == Some(true) {
            traits.insert(objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitBold);
        } else {
            traits.remove(objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitBold);
        }
        if projection.font_italic == Some(true) {
            traits.insert(objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitItalic);
        } else {
            traits.remove(objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitItalic);
        }
        if let Some(font) = NSFont::fontWithDescriptor_size(
            &descriptor.fontDescriptorWithSymbolicTraits(traits),
            font.pointSize(),
        ) {
            mutable.insert(font_key, &font);
        }
    }
    let underline_key = unsafe { NSUnderlineStyleAttributeName };
    if projection.font_underline == Some(true) {
        let value = NSNumber::numberWithInteger(NSUnderlineStyle::Single.0);
        mutable.insert(underline_key, &value);
    } else {
        mutable.removeObjectForKey(underline_key);
    }
    let strike_key = unsafe { NSStrikethroughStyleAttributeName };
    if projection.font_strikeout == Some(true) {
        let value = NSNumber::numberWithInteger(NSUnderlineStyle::Single.0);
        mutable.insert(strike_key, &value);
    } else {
        mutable.removeObjectForKey(strike_key);
    }
    if let Some(color) = format.background_color {
        if color.alpha > 0 {
            let value = NSColor::colorWithRed_green_blue_alpha(
                f64::from(color.red) / 255.0,
                f64::from(color.green) / 255.0,
                f64::from(color.blue) / 255.0,
                f64::from(color.alpha) / 255.0,
            );
            mutable.insert(unsafe { NSBackgroundColorAttributeName }, &value);
        } else {
            mutable.removeObjectForKey(unsafe { NSBackgroundColorAttributeName });
        }
    } else if projection.clear_background {
        mutable.removeObjectForKey(unsafe { NSBackgroundColorAttributeName });
    }
    if let Some(href) = format.anchor_href.as_deref().filter(|_| !format.clear_link) {
        if let Some(value) = NSURL::initWithString(NSURL::alloc(), &NSString::from_str(href)) {
            mutable.insert(unsafe { NSLinkAttributeName }, &value);
        } else {
            mutable.removeObjectForKey(unsafe { NSLinkAttributeName });
        }
    } else if projection.clear_link {
        mutable.removeObjectForKey(unsafe { NSLinkAttributeName });
    }
    unsafe { body.setTypingAttributes(&mutable) };
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
    note_previews: RefCell<Vec<NotePreview>>,
    note_filter_query: RefCell<String>,
    thumbnail_cache: RefCell<ThumbnailCache<Retained<NSImage>>>,
    thumbnail_decode_count: Cell<usize>,
    thumbnail_decode_attempts: Cell<usize>,
    thumbnail_negative_count: Cell<usize>,
    thumbnail_requests: RefCell<ThumbnailRequestLedger>,
    thumbnail_drain_scheduled: Cell<bool>,
    loading_guard: RefCell<bool>,
    autosave: RefCell<AutosaveState>,
    shell_visibility: RefCell<ShellVisibility>,
    focus_restore_visibility: RefCell<Option<ShellVisibility>>,
    last_body_selection: RefCell<NSRange>,
    last_body_selection_note_id: RefCell<Option<String>>,
    selection_sync_guard: RefCell<bool>,
    collection_selection_guard: Cell<bool>,
    editor_session: RefCell<Option<NativeEditorSession>>,
    projection_attachments: RefCell<Vec<RenderedAttachment>>,
    projection_prefixes: RefCell<Vec<RenderedProjectionPrefix>>,
    projection_empty_carriers: RefCell<Vec<EmptyBlockCarrier>>,
    pending_editor_intents: RefCell<Vec<PendingEditorIntent>>,
    pending_editor_composition: RefCell<Option<PendingEditorComposition>>,
    pending_editor_intent_invalid: RefCell<bool>,
    sidebar_background: OnceCell<Retained<NSBox>>,
    browser_background: OnceCell<Retained<NSBox>>,
    editor_background: OnceCell<Retained<NSBox>>,
    sidebar_separator: OnceCell<Retained<NSBox>>,
    browser_scroll: OnceCell<Retained<NSScrollView>>,
    note_collection: OnceCell<Retained<NSCollectionView>>,
    browser_title: OnceCell<Retained<NSTextField>>,
    browser_count: OnceCell<Retained<NSTextField>>,
    breadcrumb_label: OnceCell<Retained<NSTextField>>,
    updated_label: OnceCell<Retained<NSTextField>>,
    list_empty_label: OnceCell<Retained<NSTextField>>,
    library_label: OnceCell<Retained<NSTextField>>,
    new_button: OnceCell<Retained<NSButton>>,
    search_field: OnceCell<Retained<NSSearchField>>,
    title_field: OnceCell<Retained<NSTextField>>,
    focus_button: OnceCell<Retained<NSButton>>,
    browser_toggle_button: OnceCell<Retained<NSButton>>,
    body_scroll: OnceCell<Retained<NSScrollView>>,
    body_view: OnceCell<Retained<NSTextView>>,
    delete_button: OnceCell<Retained<NSButton>>,
    save_status: OnceCell<Retained<NSTextField>>,
    editor_empty_label: OnceCell<Retained<NSTextField>>,
    toolbar_buttons: RefCell<Vec<(EditorAction, Retained<NSButton>)>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct AppDelegate;
    unsafe impl NSObjectProtocol for AppDelegate {}
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminate:))]
        fn application_should_terminate(
            &self,
            _sender: &NSApplication,
        ) -> NSApplicationTerminateReply {
            if self.save_current_note() {
                NSApplicationTerminateReply::TerminateNow
            } else {
                NSApplicationTerminateReply::TerminateCancel
            }
        }

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
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1380.0, 820.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Miniaturizable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            window.setContentMinSize(NSSize::new(1100.0, 700.0));
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

            let browser_background = NSBox::initWithFrame(
                NSBox::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            );
            browser_background.setBoxType(NSBoxType::Custom);
            browser_background.setTransparent(false);
            browser_background.setFillColor(&NSColor::controlBackgroundColor());
            browser_background.setBorderWidth(0.0);
            content.addSubview(&browser_background);

            let editor_background = NSBox::initWithFrame(
                NSBox::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            );
            editor_background.setBoxType(NSBoxType::Custom);
            editor_background.setTransparent(false);
            editor_background.setFillColor(&NSColor::textBackgroundColor());
            editor_background.setBorderColor(&NSColor::separatorColor());
            editor_background.setBorderWidth(1.0);
            content.addSubview(&editor_background);

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

            let browser_scroll = NSScrollView::initWithFrame(
                NSScrollView::alloc(mtm),
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            browser_scroll.setHasVerticalScroller(true);
            browser_scroll.setAutohidesScrollers(true);
            browser_scroll.setBorderType(NSBorderType::NoBorder);
            browser_scroll.setDrawsBackground(false);
            let note_collection = make_note_collection_view(
                mtm,
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
                .ns_rect(),
            );
            browser_scroll.setDocumentView(Some(&note_collection));
            content.addSubview(&browser_scroll);

            let browser_title = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            );
            browser_title.setStringValue(ns_string!("笔记"));
            browser_title.setBezeled(false);
            browser_title.setDrawsBackground(false);
            browser_title.setEditable(false);
            browser_title.setFont(Some(&NSFont::systemFontOfSize_weight(22.0, 0.5)));
            content.addSubview(&browser_title);

            let browser_count = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            );
            browser_count.setStringValue(ns_string!("0"));
            browser_count.setBezeled(false);
            browser_count.setDrawsBackground(false);
            browser_count.setEditable(false);
            browser_count.setAlignment(NSTextAlignment::Right);
            browser_count.setFont(Some(&NSFont::systemFontOfSize(13.0)));
            browser_count.setTextColor(Some(&NSColor::secondaryLabelColor()));
            content.addSubview(&browser_count);

            let library_label = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            );
            library_label.setStringValue(ns_string!("Joplin Lite"));
            library_label.setBezeled(false);
            library_label.setDrawsBackground(false);
            library_label.setEditable(false);
            library_label.setFont(Some(&NSFont::systemFontOfSize_weight(22.0, 0.5)));
            content.addSubview(&library_label);

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
            // Evernote-style single primary action: a stable green accent is
            // easier to find than the system-blue default and remains clear
            // in the native disabled/pressed states.
            let evernote_green = NSColor::colorWithSRGBRed_green_blue_alpha(
                0.0,
                0.66,
                0.18,
                1.0,
            );
            new_button.setBezelColor(Some(&evernote_green));
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
                search_field.setDelegate(Some(ProtocolObject::from_ref(self)));
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
            title_field.setFont(Some(&NSFont::systemFontOfSize_weight(30.0, 0.5)));
            title_field.setTextColor(Some(&NSColor::labelColor()));
            title_field.setMaximumNumberOfLines(1);
            title_field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            content.addSubview(&title_field);

            let breadcrumb_label = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            );
            breadcrumb_label.setStringValue(ns_string!("本地资料库 · 笔记"));
            breadcrumb_label.setBezeled(false);
            breadcrumb_label.setDrawsBackground(false);
            breadcrumb_label.setEditable(false);
            breadcrumb_label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
            breadcrumb_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            breadcrumb_label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            content.addSubview(&breadcrumb_label);

            let updated_label = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            );
            updated_label.setStringValue(ns_string!(""));
            updated_label.setBezeled(false);
            updated_label.setDrawsBackground(false);
            updated_label.setEditable(false);
            updated_label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            updated_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            updated_label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            content.addSubview(&updated_label);

            let focus_button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("↙↗"),
                    Some(self),
                    Some(sel!(toggleFocusMode:)),
                    mtm,
                )
            };
            focus_button.setBezelStyle(NSBezelStyle::Toolbar);
            focus_button.setBordered(false);
            focus_button.setToolTip(Some(ns_string!("展开或收起导航与浏览（专注模式）")));
            content.addSubview(&focus_button);

            let browser_toggle_button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("↗"),
                    Some(self),
                    Some(sel!(toggleBrowser:)),
                    mtm,
                )
            };
            browser_toggle_button.setBezelStyle(NSBezelStyle::Toolbar);
            browser_toggle_button.setBordered(false);
            browser_toggle_button.setToolTip(Some(ns_string!("显示或隐藏笔记浏览栏")));
            content.addSubview(&browser_toggle_button);

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
            // Deletion is intentionally exposed only from the overflow menu.
            // Keeping this action object lets the existing selector and smoke
            // path remain stable without reserving a second bottom-right
            // control that can collide with the save status.
            delete_button.setHidden(true);

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
            editor_empty_label.setStringValue(ns_string!("开始写作"));
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

            Self::install_toolbar_buttons(mtm, self, &content);

            self.ivars().window.set(window.clone()).unwrap();
            self.ivars()
                .sidebar_background
                .set(sidebar_background)
                .unwrap();
            self.ivars()
                .browser_background
                .set(browser_background)
                .unwrap();
            self.ivars()
                .editor_background
                .set(editor_background)
                .unwrap();
            self.ivars().sidebar_separator.set(separator).unwrap();
            self.ivars().browser_scroll.set(browser_scroll).unwrap();
            note_collection.setDataSource(Some(ProtocolObject::from_ref(self)));
            note_collection.setDelegate(Some(ProtocolObject::from_ref(self)));
            self.ivars().note_collection.set(note_collection).unwrap();
            self.ivars().browser_title.set(browser_title).unwrap();
            self.ivars().browser_count.set(browser_count).unwrap();
            self.ivars().breadcrumb_label.set(breadcrumb_label).unwrap();
            self.ivars().updated_label.set(updated_label).unwrap();
            self.ivars().library_label.set(library_label).unwrap();
            self.ivars().list_empty_label.set(list_empty_label).unwrap();
            self.ivars().new_button.set(new_button).unwrap();
            self.ivars().search_field.set(search_field).unwrap();
            self.ivars().title_field.set(title_field.clone()).unwrap();
            self.ivars().focus_button.set(focus_button).unwrap();
            self.ivars()
                .browser_toggle_button
                .set(browser_toggle_button)
                .unwrap();
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
            if let Some(id) = self.ivars().note_previews.borrow().first().map(|p| p.note_id.clone())
                && let Ok(Some(note)) = self.ivars().repository.get_note(&id)
            {
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
                    self.ivars().note_previews.borrow().len()
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
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _window: &NSWindow) -> bool {
            self.save_current_note()
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if self.save_current_note() {
                NSApplication::sharedApplication(self.mtm()).terminate(None);
            }
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
        #[unsafe(method(controlTextDidBeginEditing:))]
        fn control_text_did_begin_editing(&self, _notification: &NSNotification) {
            self.update_formatting_buttons();
        }

        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.mark_current_note_dirty();
            self.resume_deferred_autosave_if_ready();
            self.update_formatting_buttons();
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, _notification: &NSNotification) {
            self.update_formatting_buttons();
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
            let selection = body.selectedRange();
            if let Some(session) = self.ivars().editor_session.borrow_mut().as_mut() {
                sync_caret_after_editor_event(
                    session,
                    sync_result,
                    selection,
                    body.hasMarkedText(),
                    loading_guard,
                );
            }
            self.refresh_search_highlights(body, false);
            self.update_formatting_buttons();
            if should_persist_after_editor_sync(sync_result) {
                if matches!(sync_result, EditorSessionSyncResult::Applied) {
                    self.mark_current_note_dirty();
                }
                self.resume_deferred_autosave_if_ready();
            } else if should_restore_after_editor_sync(sync_result) {
                self.restore_body_from_session_after_rejected_edit();
                self.set_save_status("正文变更未保存，请重试", true);
            }
        }
    }
    unsafe impl NSTextFieldDelegate for AppDelegate {}
    unsafe impl NSSearchFieldDelegate for AppDelegate {}
    #[allow(non_snake_case)]
    unsafe impl NSCollectionViewDataSource for AppDelegate {
        #[unsafe(method(collectionView:numberOfItemsInSection:))]
        fn collectionView_numberOfItemsInSection(
            &self,
            _collection_view: &NSCollectionView,
            _section: isize,
        ) -> isize {
            self.ivars().note_previews.borrow().len() as isize
        }

        #[unsafe(method_id(collectionView:itemForRepresentedObjectAtIndexPath:))]
        fn collectionView_itemForRepresentedObjectAtIndexPath(
            &self,
            collection_view: &NSCollectionView,
            index_path: &NSIndexPath,
        ) -> Retained<NSCollectionViewItem> {
            let identifier = NSString::from_str(
                joplin_lite_native::native_note_browser::NOTE_CARD_IDENTIFIER,
            );
            let item = collection_view.makeItemWithIdentifier_forIndexPath(&identifier, index_path);
            let index = index_path.item() as usize;
            if let Some(preview) = self.ivars().note_previews.borrow().get(index).cloned() {
                let selected = self
                    .ivars()
                    .current_note_id
                    .borrow()
                    .as_deref()
                    == Some(preview.note_id.as_str());
                let image = self.thumbnail_for_preview(&preview);
                let query = self.ivars().note_filter_query.borrow().clone();
                configure_note_card(
                    &item,
                    &preview,
                    &query,
                    image.as_deref(),
                    selected,
                    joplin_lite_native::native_note_browser::browser_metrics(
                        collection_view.frame().size.width,
                    ),
                    self.mtm(),
                );
            }
            item
        }
    }
    #[allow(non_snake_case)]
    unsafe impl NSCollectionViewDelegate for AppDelegate {
        #[unsafe(method(collectionView:didSelectItemsAtIndexPaths:))]
        fn collectionView_didSelectItemsAtIndexPaths(
            &self,
            _collection_view: &NSCollectionView,
            index_paths: &NSSet<NSIndexPath>,
        ) {
            if self.ivars().collection_selection_guard.get() {
                return;
            }
            let Some(index_path) = (unsafe { index_paths.anyObject_unchecked() }) else {
                return;
            };
            self.select_note_index(index_path.item() as usize);
        }
    }
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
            if let Some(body) = self.ivars().body_view.get() {
                let projected_selection = body.selectedRange();
                *self.ivars().last_body_selection.borrow_mut() = projected_selection;
                *self.ivars().last_body_selection_note_id.borrow_mut() =
                    self.ivars().current_note_id.borrow().clone();
                let selection = semantic_range_from_projection(
                    projected_selection,
                    &self.ivars().projection_prefixes.borrow(),
                );
                let should_sync = should_sync_caret_context_after_delegate(
                    *self.ivars().selection_sync_guard.borrow(),
                    *self.ivars().loading_guard.borrow(),
                    body.hasMarkedText(),
                    !self.ivars().pending_editor_intents.borrow().is_empty(),
                    self.ivars().pending_editor_composition.borrow().is_some(),
                );
                if should_sync
                    && let Some(selection) = selection
                    && let Some(session) = self.ivars().editor_session.borrow_mut().as_mut()
                {
                    session.sync_caret_context(selection);
                }
            }
            self.reapply_empty_carrier_for_selection();
            self.update_formatting_buttons();
            self.resume_deferred_autosave_if_ready();
        }

        #[unsafe(method(textViewDidChangeTypingAttributes:))]
        fn text_view_did_change_typing_attributes(&self, _notification: &NSNotification) {
            self.update_formatting_buttons();
        }
    }
    impl AppDelegate {
        #[unsafe(method(drainThumbnailQueue:))]
        fn drain_thumbnail_queue(&self, _sender: &NSObject) {
            self.ivars().thumbnail_drain_scheduled.set(false);
            let completions = {
                let runtime = thumbnail_runtime();
                let mut queue = runtime
                    .completions
                    .lock()
                    .expect("thumbnail completion queue poisoned");
                queue.drain(..).collect::<Vec<_>>()
            };
            for completion in completions {
                let display_size = completion.pixels.and_then(|pixels| {
                    thumbnail_pixels_within_bound(pixels, completion.key.target_size as usize)
                        .then(|| {
                            thumbnail_display_size(pixels, completion.key.target_size as usize)
                        })
                        .flatten()
                });
                let pixels_ok = completion
                    .pixels
                    .is_some_and(|pixels| {
                        thumbnail_pixels_within_bound(pixels, completion.key.target_size as usize)
                    });
                let success = completion.image.is_some() && pixels_ok && display_size.is_some();
                self.ivars()
                    .thumbnail_requests
                    .borrow_mut()
                    .complete(&completion.key, success);
                if success {
                    if let Some(image) = completion.image {
                        let image = NSImage::initWithCGImage_size(
                            NSImage::alloc(),
                            &image,
                            NSSize::new(display_size.unwrap().0, display_size.unwrap().1),
                        );
                        self.ivars()
                            .thumbnail_cache
                            .borrow_mut()
                            .insert(completion.key.clone(), image);
                        self.ivars().thumbnail_decode_count.set(
                            self.ivars().thumbnail_decode_count.get().saturating_add(1),
                        );
                    }
                    self.reload_visible_thumbnail_card(&completion.key);
                } else {
                    self.ivars().thumbnail_negative_count.set(
                        self.ivars().thumbnail_negative_count.get().saturating_add(1),
                    );
                }
            }
            // A worker completion frees one bounded queue slot. Revisit all
            // visible previews so deferred keys can now be submitted, while
            // only the matching card is actually reloaded above.
            self.request_visible_thumbnails();
            if self.ivars().thumbnail_requests.borrow().in_flight_len() > 0 {
                self.schedule_thumbnail_drain();
            }
        }

        #[unsafe(method(runAutosave:))]
        fn run_autosave(&self, sender: &NSObject) {
            let Some(token) = sender.downcast_ref::<NSString>() else {
                return;
            };
            let token_string = token.to_string();
            let Some((token_note_id, token_epoch, generation)) = token_string
                .split_once('\u{1f}')
                .and_then(|(note_id, token)| {
                    token.split_once('\u{1f}').and_then(|(epoch, generation)| {
                        Some((
                            note_id,
                            epoch.parse::<u64>().ok()?,
                            generation.parse::<u64>().ok()?,
                        ))
                    })
                })
            else {
                return;
            };
            let Some(note_id) = self.ivars().current_note_id.borrow().clone() else {
                return;
            };
            if token_note_id != note_id {
                return;
            }
            let marked_text = self
                .ivars()
                .body_view
                .get()
                .is_some_and(|body| body.hasMarkedText())
                || self.title_field_has_marked_text();
            let loading_guard = *self.ivars().loading_guard.borrow();
            if autosave_timer_defer_allowed(marked_text, loading_guard) {
                self.ivars()
                    .autosave
                    .borrow_mut()
                    .defer_timer_with_epoch(&note_id, token_epoch, generation);
                return;
            }
            if loading_guard {
                return;
            }
            let decision = self
                .ivars()
                .autosave
                .borrow()
                .timer_decision_with_epoch(&note_id, token_epoch, generation);
            match decision {
                AutosaveDecision::Stale => {}
                AutosaveDecision::Noop => {
                    self.ivars().autosave.borrow_mut().mark_saved(generation);
                    self.set_save_status("已保存", false);
                }
                AutosaveDecision::Persist { title, html, .. } => {
                    let ok = self.persist_note_content(
                        &note_id,
                        PreparedNoteContent {
                            update: NoteContentUpdate { title, body: html },
                        },
                    );
                    if ok {
                        self.ivars().autosave.borrow_mut().mark_saved(generation);
                    }
                }
            }
        }

        #[unsafe(method(insertImage:))]
        #[allow(deprecated)]
        fn insert_image_action(&self, _sender: &NSObject) {
            if self.ivars().current_note_id.borrow().is_some() && !self.save_current_note() {
                return;
            }
            let panel = NSOpenPanel::openPanel(self.mtm());
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(false);
            let png = NSString::from_str("png");
            let jpg = NSString::from_str("jpg");
            let jpeg = NSString::from_str("jpeg");
            let allowed: Retained<NSArray<NSString>> =
                NSArray::from_slice(&[&*png, &*jpg, &*jpeg]);
            panel.setAllowedFileTypes(Some(&allowed));
            panel.setTitle(Some(ns_string!("插入图片")));
            if panel.runModal() != NSModalResponseOK {
                return;
            }
            let Some(url) = panel.URLs().firstObject() else {
                return;
            };
            let Some(path) = url.path() else {
                self.set_save_status("图片未插入：格式不支持", true);
                return;
            };
            let path = PathBuf::from(path.to_string());
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase);
            let mime = match extension.as_deref() {
                Some("png") => "image/png",
                Some("jpg") | Some("jpeg") => "image/jpeg",
                _ => {
                    self.set_save_status("图片未插入：格式不支持", true);
                    return;
                }
            };
            let bytes = match read_regular_image_file(&path) {
                Ok(bytes) if valid_image_bytes_for_mime(&bytes, mime) => bytes,
                _ => {
                    self.set_save_status("图片未插入：格式不支持", true);
                    return;
                }
            };
            let title = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("图片");
            let _ = self.insert_image_data(&bytes, title, mime);
        }

        #[unsafe(method(showBlockStyle:))]
        fn show_block_style(&self, sender: &NSButton) {
            if self.ivars().current_note_id.borrow().is_none() {
                return;
            }
            let menu = NSMenu::initWithTitle(NSMenu::alloc(self.mtm()), ns_string!("块样式"));
            let selection = self.command_selection().unwrap_or(NSRange::new(0, 0));
            for (title, action) in [
                ("正文", sel!(setParagraphStyle:)),
                ("标题 1", sel!(setHeadingOne:)),
                ("标题 2", sel!(setHeadingTwo:)),
                ("标题 3", sel!(setHeadingThree:)),
            ] {
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(self.mtm()),
                        &NSString::from_str(title),
                        Some(action),
                        ns_string!(""),
                    )
                };
                unsafe { item.setTarget(Some(self)) };
                let state = self
                    .ivars()
                    .editor_session
                    .borrow()
                    .as_ref()
                    .and_then(|session| {
                        let command = match title {
                            "正文" => BlockCommand::Paragraph,
                            "标题 1" => BlockCommand::Heading(
                                joplin_lite_native::html_body::HeadingLevel::One,
                            ),
                            "标题 2" => BlockCommand::Heading(
                                joplin_lite_native::html_body::HeadingLevel::Two,
                            ),
                            _ => BlockCommand::Heading(
                                joplin_lite_native::html_body::HeadingLevel::Three,
                            ),
                        };
                        query_block_state(session, selection, command).ok()
                    })
                    .unwrap_or(SelectionState::Inactive);
                item.setState(match state {
                    SelectionState::Active => NSControlStateValueOn,
                    SelectionState::Mixed => NSControlStateValueMixed,
                    SelectionState::Inactive => NSControlStateValueOff,
                });
                item.setEnabled(self.current_action_presentation(EditorAction::BlockStyle).enabled);
                menu.addItem(&item);
            }
            menu.popUpMenuPositioningItem_atLocation_inView(
                None,
                NSPoint::new(0.0, 0.0),
                Some(sender),
            );
        }

        #[unsafe(method(setParagraphStyle:))]
        fn set_paragraph_style(&self, _sender: &NSObject) {
            self.apply_block_command(BlockCommand::Paragraph);
        }

        #[unsafe(method(setHeadingOne:))]
        fn set_heading_one(&self, _sender: &NSObject) {
            self.apply_block_command(BlockCommand::Heading(
                joplin_lite_native::html_body::HeadingLevel::One,
            ));
        }

        #[unsafe(method(setHeadingTwo:))]
        fn set_heading_two(&self, _sender: &NSObject) {
            self.apply_block_command(BlockCommand::Heading(
                joplin_lite_native::html_body::HeadingLevel::Two,
            ));
        }

        #[unsafe(method(setHeadingThree:))]
        fn set_heading_three(&self, _sender: &NSObject) {
            self.apply_block_command(BlockCommand::Heading(
                joplin_lite_native::html_body::HeadingLevel::Three,
            ));
        }

        #[unsafe(method(toggleHighlight:))]
        fn toggle_highlight(&self, _sender: &NSObject) {
            self.apply_inline_command(InlineCommand::Highlight);
        }

        #[unsafe(method(toggleBulletList:))]
        fn toggle_bullet_list(&self, _sender: &NSObject) {
            self.apply_block_command(BlockCommand::UnorderedList);
        }

        #[unsafe(method(toggleOrderedList:))]
        fn toggle_ordered_list(&self, _sender: &NSObject) {
            self.apply_block_command(BlockCommand::OrderedList);
        }

        #[unsafe(method(toggleChecklist:))]
        fn toggle_checklist(&self, _sender: &NSObject) {
            self.apply_block_command(BlockCommand::Checklist);
        }

        #[unsafe(method(showMore:))]
        fn show_more(&self, sender: &NSButton) {
            if self.ivars().current_note_id.borrow().is_none() {
                return;
            }
            let menu = NSMenu::initWithTitle(NSMenu::alloc(self.mtm()), ns_string!("更多"));
            let toolbar_width = self
                .ivars()
                .window
                .get()
                .and_then(|window| window.contentView())
                .map(|content| {
                    shell_layout(
                        content.frame().size.width,
                        content.frame().size.height,
                        *self.ivars().shell_visibility.borrow(),
                    )
                    .toolbar
                    .width
                })
                .unwrap_or(0.0);
            let mut previous_group = None;
            for action in toolbar_overflow_actions_for_width(toolbar_width) {
                let Some(descriptor) = editor_action_catalogue()
                    .iter()
                    .find(|descriptor| descriptor.action == action)
                else {
                    continue;
                };
                if previous_group.is_some_and(|group| group != descriptor.group) {
                    menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
                }
                previous_group = Some(descriptor.group);
                let Some(action) = Self::toolbar_selector(descriptor.action) else {
                    continue;
                };
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(self.mtm()),
                        &NSString::from_str(descriptor.label),
                        Some(action),
                        ns_string!(""),
                    )
                };
                unsafe { item.setTarget(Some(self)) };
                let presentation = self.current_action_presentation(descriptor.action);
                item.setState(match presentation.state {
                    SelectionState::Active => NSControlStateValueOn,
                    SelectionState::Mixed => NSControlStateValueMixed,
                    SelectionState::Inactive => NSControlStateValueOff,
                });
                item.setEnabled(presentation.enabled);
                menu.addItem(&item);
            }
            // Destructive note actions belong in the explicit overflow menu,
            // never in the editor's persistent status area.
            menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
            let delete_item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm()),
                    ns_string!("删除笔记"),
                    Some(sel!(deleteNote:)),
                    ns_string!(""),
                )
            };
            unsafe { delete_item.setTarget(Some(self)) };
            delete_item.setEnabled(self.ivars().current_note_id.borrow().is_some());
            menu.addItem(&delete_item);
            menu.popUpMenuPositioningItem_atLocation_inView(
                None,
                NSPoint::new(0.0, 0.0),
                Some(sender),
            );
        }

        #[unsafe(method(toggleFocusMode:))]
        fn toggle_focus_mode(&self, _sender: &NSObject) {
            let current = *self.ivars().shell_visibility.borrow();
            let restore = *self.ivars().focus_restore_visibility.borrow();
            let (next, next_restore) = toggle_focus_visibility(current, restore);
            *self.ivars().shell_visibility.borrow_mut() = next;
            *self.ivars().focus_restore_visibility.borrow_mut() = next_restore;
            self.relayout_window();
        }

        #[unsafe(method(toggleBrowser:))]
        fn toggle_browser(&self, _sender: &NSObject) {
            let current = *self.ivars().shell_visibility.borrow();
            let next = toggle_browser_visibility(current);
            *self.ivars().shell_visibility.borrow_mut() = next;
            self.relayout_window();
        }

        #[unsafe(method(applyLink:))]
        fn apply_link_action(&self, _sender: &NSObject) {
            self.show_link_editor();
        }

        #[unsafe(method(alignLeft:))]
        fn align_left(&self, _sender: &NSObject) {
            self.apply_paragraph_command(ParagraphCommand::Align(
                joplin_lite_native::html_body::Alignment::Left,
            ));
        }

        #[unsafe(method(alignCenter:))]
        fn align_center(&self, _sender: &NSObject) {
            self.apply_paragraph_command(ParagraphCommand::Align(
                joplin_lite_native::html_body::Alignment::Center,
            ));
        }

        #[unsafe(method(alignRight:))]
        fn align_right(&self, _sender: &NSObject) {
            self.apply_paragraph_command(ParagraphCommand::Align(
                joplin_lite_native::html_body::Alignment::Right,
            ));
        }

        #[unsafe(method(increaseIndent:))]
        fn increase_indent(&self, _sender: &NSObject) {
            self.apply_paragraph_command(ParagraphCommand::IncreaseIndent);
        }

        #[unsafe(method(decreaseIndent:))]
        fn decrease_indent(&self, _sender: &NSObject) {
            self.apply_paragraph_command(ParagraphCommand::DecreaseIndent);
        }

        #[unsafe(method(toggleStrikethrough:))]
        fn toggle_strikethrough(&self, _sender: &NSObject) {
            self.apply_inline_command(InlineCommand::Strikethrough);
        }

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
        if self.non_body_text_focus() {
            if let Some(window) = self.ivars().window.get()
                && let Some(first_responder) = window.firstResponder()
            {
                unsafe { first_responder.tryToPerform_with(sel!(undo:), None) };
            }
            self.update_formatting_buttons();
            return;
        }
        let Some(selection) = self.command_selection() else {
            self.update_formatting_buttons();
            return;
        };
        let changed = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| session.can_undo() && session.undo().is_ok());
        if changed {
            self.refresh_body_from_session();
            self.restore_command_selection(selection);
            self.save_current_note();
        } else {
            self.restore_command_selection(selection);
        }
    }

    #[unsafe(method(redoText:))]
    fn redo_text(&self, _sender: &NSObject) {
        if self.non_body_text_focus() {
            if let Some(window) = self.ivars().window.get()
                && let Some(first_responder) = window.firstResponder()
            {
                unsafe { first_responder.tryToPerform_with(sel!(redo:), None) };
            }
            self.update_formatting_buttons();
            return;
        }
        let Some(selection) = self.command_selection() else {
            self.update_formatting_buttons();
            return;
        };
        let changed = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| session.can_redo() && session.redo().is_ok());
        if changed {
            self.refresh_body_from_session();
            self.restore_command_selection(selection);
            self.save_current_note();
        } else {
            self.restore_command_selection(selection);
        }
    }

    #[unsafe(method(newNote:))]
    fn new_note(&self, _sender: &NSObject) {
        if self.ivars().current_note_id.borrow().is_some() && !self.save_current_note() {
            return;
        }
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
                self.ivars().note_filter_query.borrow_mut().clear();
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

    #[unsafe(method(searchNotes:))]
    fn search_notes_action(&self, sender: &NSSearchField) {
        self.search_notes(&sender.stringValue().to_string());
    }

    #[unsafe(method(focusSearch:))]
    fn focus_search(&self, _sender: &NSObject) {
        let current = *self.ivars().shell_visibility.borrow();
        let restore = *self.ivars().focus_restore_visibility.borrow();
        let (visibility, restore) = search_focus_visibility(current, restore);
        if visibility != current || restore != *self.ivars().focus_restore_visibility.borrow() {
            *self.ivars().shell_visibility.borrow_mut() = visibility;
            *self.ivars().focus_restore_visibility.borrow_mut() = restore;
            if let Some(content) = self
                .ivars()
                .window
                .get()
                .and_then(|window| window.contentView())
            {
                let frame = content.frame();
                self.layout_content(frame.size.width, frame.size.height);
            }
        }
        let Some(search) = self.ivars().search_field.get() else {
            return;
        };
        let Some(window) = self.ivars().window.get() else {
            return;
        };
        if window.makeFirstResponder(Some(search)) {
            unsafe { search.selectText(None) };
            self.update_formatting_buttons();
        }
    }

    #[unsafe(method(deleteNote:))]
    fn delete_note(&self, _sender: &NSObject) {
        if self.ivars().current_note_id.borrow().is_some() && !self.save_current_note() {
            return;
        }
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
        if let Some(id) = self.ivars().note_previews.borrow().first().map(|p| p.note_id.clone())
            && let Ok(Some(note)) = self.ivars().repository.get_note(&id)
        {
            self.load_note(&note);
        } else {
            self.clear_current_note();
        }
    }
    }
);

impl AppDelegate {
    fn thumbnail_for_preview(&self, preview: &NotePreview) -> Option<Retained<NSImage>> {
        let resource_id = preview.first_image_id.as_deref()?;
        let key = ThumbnailKey {
            resource_id: resource_id.to_owned(),
            target_size: 112,
        };
        if let Some(image) = self.ivars().thumbnail_cache.borrow_mut().get(&key) {
            return Some(image);
        }
        let request = self.ivars().thumbnail_requests.borrow_mut().request(&key);
        if !matches!(request, ThumbnailRequest::Start) {
            return None;
        }
        let job = ThumbnailJob {
            key: key.clone(),
            repository: Arc::clone(&self.ivars().repository),
        };
        if !submit_thumbnail_job(job) {
            self.ivars().thumbnail_requests.borrow_mut().cancel(&key);
            return None;
        }
        self.ivars().thumbnail_decode_attempts.set(
            self.ivars()
                .thumbnail_decode_attempts
                .get()
                .saturating_add(1),
        );
        self.schedule_thumbnail_drain();
        None
    }

    fn schedule_thumbnail_drain(&self) {
        if self.ivars().thumbnail_drain_scheduled.replace(true) {
            return;
        }
        let token = NSObject::new();
        unsafe {
            let _: () = msg_send![
                self,
                performSelector: sel!(drainThumbnailQueue:),
                withObject: &*token,
                afterDelay: 0.05
            ];
        }
    }

    fn request_visible_thumbnails(&self) {
        let Some(collection) = self.ivars().note_collection.get() else {
            return;
        };
        for item in collection.visibleItems().iter() {
            let Some(index_path) = collection.indexPathForItem(&item) else {
                continue;
            };
            let index = index_path.item() as usize;
            let Some(preview) = self.ivars().note_previews.borrow().get(index).cloned() else {
                continue;
            };
            let _ = self.thumbnail_for_preview(&preview);
        }
    }

    fn reload_visible_thumbnail_card(&self, key: &ThumbnailKey) {
        let Some(collection) = self.ivars().note_collection.get() else {
            return;
        };
        let mut paths = Vec::new();
        for item in collection.visibleItems().iter() {
            let Some(index_path) = collection.indexPathForItem(&item) else {
                continue;
            };
            let index = index_path.item() as usize;
            let preview = self.ivars().note_previews.borrow().get(index).cloned();
            let Some(preview) = preview else {
                continue;
            };
            if preview.first_image_id.as_deref() == Some(key.resource_id.as_str()) {
                paths.push(index_path);
            }
        }
        if !paths.is_empty() {
            let refs = paths.iter().map(|path| &**path).collect::<Vec<_>>();
            collection.reloadItemsAtIndexPaths(&NSSet::from_slice(&refs));
        }
    }

    fn select_note_index(&self, index: usize) {
        let Some(note_id) = self
            .ivars()
            .note_previews
            .borrow()
            .get(index)
            .map(|preview| preview.note_id.clone())
        else {
            return;
        };
        if self.ivars().current_note_id.borrow().as_deref() == Some(note_id.as_str()) {
            return;
        }
        let ids = self
            .ivars()
            .note_previews
            .borrow()
            .iter()
            .map(|preview| preview.note_id.clone())
            .collect::<Vec<_>>();
        let previous_id = self.ivars().current_note_id.borrow().clone();
        if !self.save_current_note() {
            let previous_index = selected_index_for_id(&ids, previous_id.as_deref());
            self.restore_selection_after_failed_switch(
                previous_id.as_deref(),
                previous_index,
                index,
            );
            return;
        }
        let latest_ids = self
            .ivars()
            .note_previews
            .borrow()
            .iter()
            .map(|preview| preview.note_id.clone())
            .collect::<Vec<_>>();
        let previous_index = selected_index_for_id(&latest_ids, previous_id.as_deref());
        let target_index = selected_index_for_id(&latest_ids, Some(note_id.as_str()));
        let Some(target_index) = target_index else {
            self.restore_selection_after_failed_switch(
                previous_id.as_deref(),
                previous_index,
                index,
            );
            return;
        };
        let Ok(Some(note)) = self.ivars().repository.get_note(&note_id) else {
            self.restore_selection_after_failed_switch(
                previous_id.as_deref(),
                previous_index,
                target_index,
            );
            return;
        };
        self.load_note(&note);
        self.update_note_selection();
        self.reload_collection_items(previous_index, Some(target_index));
    }

    fn restore_selection_after_failed_switch(
        &self,
        previous_id: Option<&str>,
        previous_index: Option<usize>,
        attempted_index: usize,
    ) {
        let ids = self
            .ivars()
            .note_previews
            .borrow()
            .iter()
            .map(|preview| preview.note_id.clone())
            .collect::<Vec<_>>();
        let restored = restore_selection_after_failed_switch(&ids, previous_id);
        self.update_note_selection();
        self.reload_collection_items(restored.or(previous_index), Some(attempted_index));
    }

    fn reload_collection_items(&self, previous_index: Option<usize>, next_index: Option<usize>) {
        let Some(collection) = self.ivars().note_collection.get() else {
            return;
        };
        let paths = [previous_index, next_index]
            .into_iter()
            .flatten()
            .filter(|index| *index < self.ivars().note_previews.borrow().len())
            .map(|index| NSIndexPath::indexPathForItem_inSection(index as isize, 0))
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return;
        }
        let refs = paths.iter().map(|path| &**path).collect::<Vec<_>>();
        let index_paths = NSSet::from_slice(&refs);
        collection.reloadItemsAtIndexPaths(&index_paths);
    }

    fn set_selected_range_programmatically(&self, body: &NSTextView, selection: NSRange) {
        let previous = *self.ivars().selection_sync_guard.borrow();
        *self.ivars().selection_sync_guard.borrow_mut() = true;
        body.setSelectedRange(selection);
        *self.ivars().selection_sync_guard.borrow_mut() = previous;
    }

    fn relayout_window(&self) {
        let Some(content) = self
            .ivars()
            .window
            .get()
            .and_then(|window| window.contentView())
        else {
            return;
        };
        self.layout_content(content.frame().size.width, content.frame().size.height);
        // Copy the range before calling into AppKit. `setSelectedRange` can
        // synchronously re-enter textViewDidChangeSelection, which mutably
        // updates this RefCell. Keeping the Ref borrow in the call expression
        // would make that legitimate delegate re-entry panic.
        let selection = selection_snapshot_for_reentrant_appkit(&self.ivars().last_body_selection);
        if let Some(body) = self.ivars().body_view.get() {
            self.set_selected_range_programmatically(body, selection);
        }
    }

    fn toolbar_selector(action: EditorAction) -> Option<Sel> {
        Some(match action {
            EditorAction::InsertImage => sel!(insertImage:),
            EditorAction::Undo => sel!(undoText:),
            EditorAction::Redo => sel!(redoText:),
            EditorAction::BlockStyle => sel!(showBlockStyle:),
            EditorAction::Bold => sel!(toggleBoldText:),
            EditorAction::Italic => sel!(toggleItalicText:),
            EditorAction::Underline => sel!(toggleUnderlineText:),
            EditorAction::Highlight => sel!(toggleHighlight:),
            EditorAction::BulletList => sel!(toggleBulletList:),
            EditorAction::OrderedList => sel!(toggleOrderedList:),
            EditorAction::Checklist => sel!(toggleChecklist:),
            EditorAction::More => sel!(showMore:),
            EditorAction::Link => sel!(applyLink:),
            EditorAction::AlignLeft => sel!(alignLeft:),
            EditorAction::AlignCenter => sel!(alignCenter:),
            EditorAction::AlignRight => sel!(alignRight:),
            EditorAction::IncreaseIndent => sel!(increaseIndent:),
            EditorAction::DecreaseIndent => sel!(decreaseIndent:),
            EditorAction::Strikethrough => sel!(toggleStrikethrough:),
            EditorAction::Clear => sel!(clearFormatting:),
        })
    }

    fn install_toolbar_buttons(mtm: MainThreadMarker, target: &AppDelegate, content: &NSView) {
        for descriptor in editor_action_catalogue() {
            let action = descriptor.action;
            let Some(selector) = Self::toolbar_selector(action) else {
                continue;
            };
            let initial_title = NSString::from_str(if action == EditorAction::BlockStyle {
                compact_toolbar_label(action)
            } else {
                ""
            });
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &initial_title,
                    Some(target),
                    Some(selector),
                    mtm,
                )
            };
            button.setButtonType(match descriptor.kind() {
                EditorActionKind::Toggle => NSButtonType::PushOnPushOff,
                EditorActionKind::Momentary | EditorActionKind::Popup => {
                    NSButtonType::MomentaryPushIn
                }
            });
            button.setBezelStyle(NSBezelStyle::Toolbar);
            button.setBordered(false);
            if let Some(symbol_name) = toolbar_symbol_name(action) {
                let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(symbol_name),
                    Some(&NSString::from_str(descriptor.label)),
                );
                if let Some(image) = image {
                    image.setSize(NSSize::new(24.0, 24.0));
                    button.setImage(Some(&image));
                    button.setImagePosition(NSCellImagePosition::ImageOnly);
                }
            }
            button.setToolTip(Some(&NSString::from_str(descriptor.label)));
            content.addSubview(&button);
            target
                .ivars()
                .toolbar_buttons
                .borrow_mut()
                .push((action, button));
        }
    }

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
        let projection_prefixes = self.ivars().projection_prefixes.borrow().clone();
        let semantic_range = semantic_range_from_projection(range, &projection_prefixes);
        if semantic_range.is_none() {
            self.clear_pending_editor_intent();
            return false;
        }
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
                Ok(mut next) => {
                    if self.ivars().pending_editor_composition.borrow().is_none() {
                        next.baseline_semantic_range = semantic_range;
                    }
                    *self.ivars().pending_editor_composition.borrow_mut() = Some(next);
                    true
                }
                Err(_) => {
                    self.clear_pending_editor_intent();
                    false
                }
            }
        } else {
            let intent = PendingEditorIntent {
                range,
                semantic_range,
                replacement,
                old_view_text,
                old_semantic_text,
                covered_attachments,
            };
            if preflight_projected_editor_intent(&intent, &projection_prefixes).is_err() {
                self.clear_pending_editor_intent();
                return false;
            }
            append_pending_editor_intent(
                &mut self.ivars().pending_editor_intents.borrow_mut(),
                intent,
            )
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
        *self.ivars().projection_prefixes.borrow_mut() = rendered.projection_prefixes.clone();
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
        self.sync_typing_attributes_for_selection(body, NSRange::new(location, length));
        self.set_selected_range_programmatically(body, NSRange::new(location, length));
        self.refresh_search_highlights(body, false);
    }

    fn sync_typing_attributes_for_selection(&self, body: &NSTextView, selection: NSRange) {
        if selection.length != 0 {
            return;
        }
        let Some(selection) =
            semantic_range_from_projection(selection, &self.ivars().projection_prefixes.borrow())
        else {
            return;
        };
        let format = {
            let session_guard = self.ivars().editor_session.borrow();
            session_guard
                .as_ref()
                .and_then(|session| effective_typing_format_at(session, selection).ok())
        };
        if let Some(format) = format {
            sync_typing_attributes_with_format(body, &format);
        }
    }

    fn refresh_search_highlights(&self, body: &NSTextView, scroll_to_first: bool) {
        let Some(layout_manager) = (unsafe { body.layoutManager() }) else {
            return;
        };
        let query = if self.ivars().current_note_id.borrow().is_some() {
            self.ivars().note_filter_query.borrow().clone()
        } else {
            String::new()
        };
        let text = body.string().to_string();
        let ranges = apply_search_highlights(&layout_manager, &text, &query);
        if scroll_to_first && let Some(range) = ranges.first() {
            body.scrollRangeToVisible(*range);
        }
    }

    fn refresh_body_from_session(&self) {
        let Some(body) = self.ivars().body_view.get() else {
            return;
        };
        let selection = semantic_range_from_projection(
            body.selectedRange(),
            &self.ivars().projection_prefixes.borrow(),
        )
        .unwrap_or_else(|| body.selectedRange());
        self.refresh_body_from_session_at(body, selection);
    }

    fn refresh_body_from_session_at(&self, body: &NSTextView, selection: NSRange) {
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
        let projected_selection =
            projection_range_from_semantic(selection, &rendered.projection_prefixes)
                .unwrap_or(selection);
        self.install_rendered_document(body, &rendered, projected_selection);
        *self.ivars().loading_guard.borrow_mut() = previous_loading_guard;
        if failures == 0 {
            self.set_save_status("已保存", false);
        } else {
            self.set_save_status("部分图片未恢复，已保留引用", true);
        }
        self.update_editor_visibility();
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
        let projected_selection = body.selectedRange();
        if projected_selection.length != 0 {
            return;
        }
        let carrier = self
            .ivars()
            .projection_empty_carriers
            .borrow()
            .iter()
            .find(|carrier| carrier.addressable_offset == projected_selection.location)
            .cloned();
        if let Some(carrier) = carrier {
            body.setDefaultParagraphStyle(Some(&carrier.paragraph));
            let typing = body.typingAttributes();
            let mutable = typing.mutableCopy();
            unsafe {
                mutable.insert(NSParagraphStyleAttributeName, &carrier.paragraph);
                body.setTypingAttributes(&mutable);
            }
            self.sync_typing_attributes_for_selection(body, projected_selection);
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
        let probe = projected_selection.location.min(length - 1);
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
        self.sync_typing_attributes_for_selection(body, projected_selection);
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
        let (decision, projection_range, invalid) = if let Some(composition) = composition {
            self.clear_pending_editor_intent();
            (
                finish_marked_editor_intent(&composition, &new_text),
                composition.baseline_range,
                false,
            )
        } else {
            let (pending, invalid) = self.take_pending_editor_intent();
            let Some(intent) = pending else {
                // A live change is accepted only when AppKit gave us exactly one
                // pre-mutation intent. The old/new string diff remains a legacy
                // test and migration helper, never a live writeback path.
                eprintln!("native editor sync rejected: pending intent missing or duplicated");
                return EditorSessionSyncResult::Rejected;
            };
            if invalid {
                eprintln!("native editor sync rejected: pending intent marked invalid");
                return EditorSessionSyncResult::Rejected;
            }
            (
                decide_pending_editor_intent(&intent, &new_text),
                intent.range,
                false,
            )
        };
        if invalid {
            // A live change is accepted only when AppKit gave us exactly one
            // pre-mutation intent. The old/new string diff remains a legacy
            // test and migration helper, never a live writeback path.
            eprintln!("native editor sync rejected: pending intent invalid after dispatch");
            return EditorSessionSyncResult::Rejected;
        }
        let mut next_attachments = self.ivars().projection_attachments.borrow().clone();
        let mut next_prefixes = self.ivars().projection_prefixes.borrow().clone();
        let result = match decision {
            PendingIntentDecision::Noop => return EditorSessionSyncResult::Noop,
            PendingIntentDecision::Reject => {
                eprintln!("native editor sync rejected: projection did not match intent");
                return EditorSessionSyncResult::Rejected;
            }
            PendingIntentDecision::ApplyText { range, replacement } => {
                let replacement_length = NSString::from_str(&replacement).length();
                if !adjust_projection_prefixes_with_semantic(
                    &mut next_prefixes,
                    projection_range,
                    range,
                    replacement_length,
                ) {
                    eprintln!("native editor sync rejected: projection prefix range invalid");
                    return EditorSessionSyncResult::Rejected;
                }
                if !adjust_projection_attachments(
                    &mut next_attachments,
                    projection_range,
                    replacement_length,
                    None,
                ) {
                    eprintln!("native editor sync rejected: attachment range invalid");
                    return EditorSessionSyncResult::Rejected;
                }
                let mut session_guard = self.ivars().editor_session.borrow_mut();
                let Some(session) = session_guard.as_mut() else {
                    eprintln!("native editor sync rejected: semantic session unavailable");
                    return EditorSessionSyncResult::Rejected;
                };
                let applied = apply_committed_text_delta(session, range, &replacement).is_ok();
                if !applied {
                    eprintln!("native editor sync rejected: semantic delta failed");
                }
                applied
            }
            PendingIntentDecision::DeleteImage { range, resource_id } => {
                if !adjust_projection_prefixes_with_semantic(
                    &mut next_prefixes,
                    projection_range,
                    range,
                    0,
                ) {
                    eprintln!("native editor sync rejected: projection prefix delete invalid");
                    return EditorSessionSyncResult::Rejected;
                }
                if !adjust_projection_attachments(
                    &mut next_attachments,
                    projection_range,
                    0,
                    Some(&resource_id),
                ) {
                    eprintln!("native editor sync rejected: image attachment range invalid");
                    return EditorSessionSyncResult::Rejected;
                }
                let mut session_guard = self.ivars().editor_session.borrow_mut();
                let Some(session) = session_guard.as_mut() else {
                    eprintln!(
                        "native editor sync rejected: semantic session unavailable for image"
                    );
                    return EditorSessionSyncResult::Rejected;
                };
                let applied =
                    delete_image_anchor_if_identity(session, range, Some(&resource_id)).is_ok();
                if !applied {
                    eprintln!("native editor sync rejected: image delta failed");
                }
                applied
            }
        };
        if result {
            *self.ivars().projection_attachments.borrow_mut() = next_attachments;
            *self.ivars().projection_prefixes.borrow_mut() = next_prefixes;
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
        let search_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("搜索笔记"),
                Some(sel!(focusSearch:)),
                ns_string!("k"),
            )
        };
        unsafe {
            search_item.setTarget(Some(target));
        }
        search_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        file_menu.addItem(&search_item);
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
        let shell = shell_layout(width, height, *self.ivars().shell_visibility.borrow());
        let list = LayoutRect {
            x: shell.browser.x,
            y: 16.0,
            width: shell.browser.width,
            height: (shell.browser.height - 96.0).max(120.0),
        };
        if let Some(separator) = self.ivars().sidebar_separator.get() {
            separator.setFrame(NSRect::new(
                NSPoint::new(shell.navigation.width - 1.0, 0.0),
                NSSize::new(1.0, shell.navigation.height),
            ));
        }
        if let Some(background) = self.ivars().sidebar_background.get() {
            background.setFrame(shell.navigation.ns_rect());
            background.setHidden(shell.navigation.width == 0.0);
        }
        if let Some(background) = self.ivars().browser_background.get() {
            background.setFrame(shell.browser.ns_rect());
            background.setHidden(shell.browser.width == 0.0);
        }
        if let Some(background) = self.ivars().editor_background.get() {
            background.setFrame(shell.sheet.ns_rect());
            background.setHidden(shell.editor.width == 0.0);
            background.setCornerRadius(12.0);
            background.setFillColor(&NSColor::textBackgroundColor());
            background.setBorderColor(&NSColor::separatorColor());
            background.setBorderWidth(1.0);
        }
        if let Some(browser_scroll) = self.ivars().browser_scroll.get() {
            browser_scroll.setFrame(list.ns_rect());
            browser_scroll.setHidden(shell.browser.width == 0.0);
        }
        if let Some(title) = self.ivars().browser_title.get() {
            title.setFrame(NSRect::new(
                NSPoint::new(shell.browser.x + 16.0, height - 52.0),
                NSSize::new((shell.browser.width - 80.0).max(0.0), 28.0),
            ));
            title.setHidden(shell.browser.width == 0.0);
        }
        if let Some(count) = self.ivars().browser_count.get() {
            count.setFrame(NSRect::new(
                NSPoint::new(shell.browser.right() - 56.0, height - 48.0),
                NSSize::new(40.0, 20.0),
            ));
            count.setStringValue(&NSString::from_str(
                &self.ivars().note_previews.borrow().len().to_string(),
            ));
            count.setHidden(shell.browser.width == 0.0);
        }
        if let Some(collection) = self.ivars().note_collection.get() {
            let row_count = self.ivars().note_previews.borrow().len().div_ceil(2);
            let metrics = joplin_lite_native::native_note_browser::browser_metrics(list.width);
            if let Some(layout) = collection
                .collectionViewLayout()
                .and_then(|layout| layout.downcast::<NSCollectionViewFlowLayout>().ok())
            {
                layout.setItemSize(NSSize::new(metrics.card_width, metrics.card_height));
                layout.invalidateLayout();
            }
            let content_height =
                (row_count as f64 * (metrics.card_height + 8.0) + 16.0).max(list.height);
            collection.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(list.width, content_height),
            ));
            self.refresh_visible_note_cards(collection, metrics);
        }
        if let Some(label) = self.ivars().list_empty_label.get() {
            label.setFrame(NSRect::new(
                NSPoint::new(list.x + 12.0, list.y + (list.height - 32.0) * 0.5),
                NSSize::new((list.width - 24.0).max(200.0), 32.0),
            ));
            label.setHidden(
                shell.browser.width == 0.0 || !self.ivars().note_previews.borrow().is_empty(),
            );
        }
        if let Some(label) = self.ivars().library_label.get() {
            label.setFrame(NSRect::new(
                NSPoint::new(16.0, height - 48.0),
                NSSize::new((shell.navigation.width - 32.0).max(0.0), 28.0),
            ));
            label.setHidden(shell.navigation.width == 0.0);
        }
        if let Some(button) = self.ivars().new_button.get() {
            button.setFrame(NSRect::new(
                NSPoint::new(16.0, height - 96.0),
                NSSize::new((shell.navigation.width - 32.0).max(0.0), 30.0),
            ));
            button.setHidden(shell.navigation.width == 0.0);
        }
        if let Some(search) = self.ivars().search_field.get() {
            let x = 16.0;
            let search_width = (shell.navigation.width - 32.0).max(0.0);
            search.setFrame(NSRect::new(
                NSPoint::new(x, height - 136.0),
                NSSize::new(search_width, 30.0),
            ));
            search.setHidden(shell.navigation.width == 0.0);
        }
        if let Some(title) = self.ivars().title_field.get() {
            title.setFrame(shell.title.ns_rect());
        }
        if let Some(breadcrumb) = self.ivars().breadcrumb_label.get() {
            breadcrumb.setFrame(shell.breadcrumb.ns_rect());
            breadcrumb.setHidden(shell.editor.width == 0.0);
        }
        if let Some(updated) = self.ivars().updated_label.get() {
            updated.setFrame(shell.updated.ns_rect());
            updated.setHidden(shell.editor.width == 0.0);
        }
        if let Some(button) = self.ivars().focus_button.get() {
            button.setFrame(NSRect::new(
                NSPoint::new(shell.sheet.right() - 66.0, shell.sheet.top() - 42.0),
                NSSize::new(58.0, 24.0),
            ));
        }
        if let Some(button) = self.ivars().browser_toggle_button.get() {
            button.setFrame(NSRect::new(
                NSPoint::new(shell.sheet.right() - 132.0, shell.sheet.top() - 42.0),
                NSSize::new(58.0, 24.0),
            ));
        }
        let visible_actions = toolbar_actions_for_width(shell.toolbar.width);
        let narrow = shell.toolbar.width < 680.0;
        for (action, button) in self.ivars().toolbar_buttons.borrow().iter() {
            let Some(visible_index) = visible_actions.iter().position(|item| item == action) else {
                button.setHidden(true);
                continue;
            };
            let group_gaps = visible_actions
                .iter()
                .take(visible_index)
                .zip(visible_actions.iter().skip(1))
                .filter(|(previous, next)| action_group(**previous) != action_group(**next))
                .count() as f64
                * 4.0;
            let x = shell.toolbar.x + (visible_index as f64 * 32.0) + group_gaps;
            button.setFrame(NSRect::new(
                NSPoint::new(x, shell.toolbar.y),
                NSSize::new(28.0, shell.toolbar.height),
            ));
            if *action == EditorAction::BlockStyle {
                let title = self
                    .ivars()
                    .editor_session
                    .borrow()
                    .as_ref()
                    .map(|session| {
                        block_style_label(
                            session,
                            self.command_selection().unwrap_or(NSRange::new(0, 0)),
                        )
                    })
                    .unwrap_or(compact_toolbar_label(*action));
                button.setTitle(&NSString::from_str(title));
            } else {
                button.setTitle(ns_string!(""));
            }
            button.setToolTip(Some(&NSString::from_str(
                editor_action_catalogue()
                    .iter()
                    .find(|descriptor| descriptor.action == *action)
                    .map(|descriptor| descriptor.label)
                    .unwrap_or(compact_toolbar_label(*action)),
            )));
            button.setHidden(narrow && x + 28.0 > shell.toolbar.right());
        }
        if let Some(scroll) = self.ivars().body_scroll.get() {
            scroll.setFrame(shell.body.ns_rect());
        }
        if let Some(body) = self.ivars().body_view.get() {
            body.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(shell.body.width, shell.body.height),
            ));
            body.setMinSize(NSSize::new(
                shell.body.width,
                shell.body.height + shell.body_bottom_inset,
            ));
            body.setMaxSize(NSSize::new(shell.body.width, f64::MAX));
            let previous_loading_guard = *self.ivars().loading_guard.borrow();
            *self.ivars().loading_guard.borrow_mut() = true;
            resize_inline_attachments(body);
            *self.ivars().loading_guard.borrow_mut() = previous_loading_guard;
        }
        if let Some(button) = self.ivars().delete_button.get() {
            button.setHidden(true);
        }
        if let Some(status) = self.ivars().save_status.get() {
            status.setFrame(shell.status.ns_rect());
        }
        if let Some(label) = self.ivars().editor_empty_label.get() {
            label.setFrame(shell.empty_editor.ns_rect());
        }
        self.update_formatting_buttons();
    }

    fn refresh_visible_note_cards(
        &self,
        collection: &NSCollectionView,
        metrics: joplin_lite_native::native_note_browser::BrowserMetrics,
    ) {
        for item in collection.visibleItems().iter() {
            let Some(index_path) = collection.indexPathForItem(&item) else {
                continue;
            };
            let index = index_path.item() as usize;
            let Some(preview) = self.ivars().note_previews.borrow().get(index).cloned() else {
                continue;
            };
            let selected =
                self.ivars().current_note_id.borrow().as_deref() == Some(preview.note_id.as_str());
            let image = self.thumbnail_for_preview(&preview);
            let query = self.ivars().note_filter_query.borrow().clone();
            configure_note_card(
                &item,
                &preview,
                &query,
                image.as_deref(),
                selected,
                metrics,
                self.mtm(),
            );
        }
    }

    fn non_body_text_focus(&self) -> bool {
        if let Some(window) = self.ivars().window.get()
            && let Some(first_responder) = window.firstResponder()
        {
            let first_ptr = Retained::<NSResponder>::as_ptr(&first_responder);
            let title_focused = self.ivars().title_field.get().is_some_and(|field| {
                Retained::<NSTextField>::as_ptr(field) as *const NSResponder == first_ptr
                    || field.currentEditor().is_some_and(|editor| {
                        Retained::<NSText>::as_ptr(&editor) as *const NSResponder == first_ptr
                    })
            });
            let search_focused = self.ivars().search_field.get().is_some_and(|field| {
                Retained::<NSSearchField>::as_ptr(field) as *const NSResponder == first_ptr
                    || field.currentEditor().is_some_and(|editor| {
                        Retained::<NSText>::as_ptr(&editor) as *const NSResponder == first_ptr
                    })
            });
            return title_focused || search_focused;
        }
        false
    }

    fn command_selection(&self) -> Option<NSRange> {
        let body = self.ivars().body_view.get()?;
        let current_note_id = self.ivars().current_note_id.borrow().clone();
        let selection_note_id = self.ivars().last_body_selection_note_id.borrow().clone();
        if !command_selection_is_available(
            current_note_id.as_deref(),
            selection_note_id.as_deref(),
            self.non_body_text_focus(),
        ) {
            return None;
        }
        let selection = semantic_range_from_projection(
            body.selectedRange(),
            &self.ivars().projection_prefixes.borrow(),
        )?;
        if selection.length > 0 {
            Some(selection)
        } else {
            semantic_range_from_projection(
                *self.ivars().last_body_selection.borrow(),
                &self.ivars().projection_prefixes.borrow(),
            )
        }
    }

    fn restore_command_selection(&self, selection: NSRange) {
        let Some(body) = self.ivars().body_view.get() else {
            return;
        };
        let projected =
            projection_range_from_semantic(selection, &self.ivars().projection_prefixes.borrow())
                .unwrap_or(selection);
        self.set_selected_range_programmatically(body, projected);
        *self.ivars().last_body_selection.borrow_mut() = projected;
        if let Some(window) = self.ivars().window.get() {
            window.makeFirstResponder(Some(body));
        }
        self.update_formatting_buttons();
    }

    fn current_action_presentation(&self, action: EditorAction) -> EditorActionPresentation {
        let has_note = self.ivars().current_note_id.borrow().is_some();
        let command_selection = self.command_selection();
        let selection = command_selection.unwrap_or(NSRange::new(0, 0));
        let session_guard = self.ivars().editor_session.borrow();
        // A loaded session must not drive body formatting state while a title
        // or search field editor owns first responder.  command_selection is
        // the shared gate for all toolbar/menu semantic commands.
        let command_session = command_selection
            .is_some()
            .then_some(session_guard.as_ref())
            .flatten();
        let mut presentation =
            semantic_action_presentation(action, has_note, command_session, selection);
        if action == EditorAction::More {
            presentation.enabled = self
                .ivars()
                .window
                .get()
                .and_then(|window| window.contentView())
                .map(|content| {
                    if !toolbar_more_enabled_for_layout(
                        content.frame().size.width,
                        content.frame().size.height,
                        *self.ivars().shell_visibility.borrow(),
                        has_note,
                    ) {
                        return false;
                    }
                    let overflow = toolbar_overflow_actions_for_width(
                        shell_layout(
                            content.frame().size.width,
                            content.frame().size.height,
                            *self.ivars().shell_visibility.borrow(),
                        )
                        .toolbar
                        .width,
                    );
                    // Delete note is always an enabled child for a selected
                    // note, even if there are no formatting actions in the
                    // overflow list.
                    has_note
                        || more_has_enabled_child(&overflow, has_note, command_session, selection)
                })
                .unwrap_or(false);
        }
        presentation
    }

    fn inline_command_is_applicable(&self, selection: NSRange, command: InlineCommand) -> bool {
        self.ivars()
            .editor_session
            .borrow()
            .as_ref()
            .is_some_and(|session| match command {
                InlineCommand::Clear => query_clear_state(session, selection)
                    .map(|state| state != SelectionState::Inactive)
                    .unwrap_or(false),
                _ => query_inline_applicability(session, selection).unwrap_or(false),
            })
    }

    fn apply_inline_command(&self, command: InlineCommand) {
        if self
            .ivars()
            .body_view
            .get()
            .is_some_and(|body| body.hasMarkedText())
        {
            self.update_formatting_buttons();
            return;
        }
        let Some(selection) = self.command_selection() else {
            self.update_formatting_buttons();
            return;
        };
        if self.ivars().current_note_id.borrow().is_none() {
            return;
        }
        if !self.inline_command_is_applicable(selection, command) {
            self.restore_command_selection(selection);
            return;
        }
        if !self.save_current_note() {
            self.restore_command_selection(selection);
            return;
        }
        let before_revision = self
            .ivars()
            .editor_session
            .borrow()
            .as_ref()
            .map(NativeEditorSession::revision)
            .unwrap_or_default();
        let applied = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| apply_inline_command(session, selection, command).is_ok());
        let after_revision = self
            .ivars()
            .editor_session
            .borrow()
            .as_ref()
            .map(NativeEditorSession::revision)
            .unwrap_or(before_revision);
        let changed = applied && after_revision != before_revision;
        if changed {
            self.refresh_body_from_session();
            self.restore_command_selection(selection);
            self.save_current_note();
        } else {
            self.restore_command_selection(selection);
        }
    }

    fn apply_block_command(&self, command: BlockCommand) {
        if self
            .ivars()
            .body_view
            .get()
            .is_some_and(|body| body.hasMarkedText())
        {
            self.update_formatting_buttons();
            return;
        }
        let Some(selection) = self.command_selection() else {
            self.update_formatting_buttons();
            return;
        };
        if self.ivars().current_note_id.borrow().is_none() {
            return;
        }
        if !self.save_current_note() {
            self.restore_command_selection(selection);
            return;
        }
        let changed = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| apply_block_command(session, selection, command).is_ok());
        if changed {
            self.refresh_body_from_session();
            self.restore_command_selection(selection);
            self.save_current_note();
        } else {
            self.restore_command_selection(selection);
        }
    }

    fn apply_paragraph_command(&self, command: ParagraphCommand) {
        if self
            .ivars()
            .body_view
            .get()
            .is_some_and(|body| body.hasMarkedText())
        {
            self.update_formatting_buttons();
            return;
        }
        let Some(selection) = self.command_selection() else {
            self.update_formatting_buttons();
            return;
        };
        if self.ivars().current_note_id.borrow().is_none() {
            return;
        }
        if !self.save_current_note() {
            self.restore_command_selection(selection);
            return;
        }
        let changed = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| apply_paragraph_command(session, selection, command).is_ok());
        if changed {
            self.refresh_body_from_session();
            self.restore_command_selection(selection);
            self.save_current_note();
        } else {
            self.restore_command_selection(selection);
        }
    }

    fn show_link_editor(&self) {
        if self
            .ivars()
            .body_view
            .get()
            .is_some_and(|body| body.hasMarkedText())
        {
            self.update_formatting_buttons();
            return;
        }
        let Some(selection) = self.command_selection() else {
            self.update_formatting_buttons();
            return;
        };
        if self.ivars().current_note_id.borrow().is_none() {
            return;
        }
        if !self.save_current_note() {
            self.restore_command_selection(selection);
            return;
        }
        if selection.length == 0 {
            self.set_save_status("链接未应用：请先选择文字", true);
            self.restore_command_selection(selection);
            return;
        }
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(ns_string!("添加链接"));
        alert.setInformativeText(ns_string!("请输入 http、https 或 mailto 链接"));
        let field = NSTextField::initWithFrame(
            NSTextField::alloc(self.mtm()),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 24.0)),
        );
        field.setPlaceholderString(Some(ns_string!("https://example.com")));
        alert.setAccessoryView(Some(&field));
        alert.addButtonWithTitle(ns_string!("应用"));
        alert.addButtonWithTitle(ns_string!("取消"));
        if alert.runModal() != objc2_app_kit::NSAlertFirstButtonReturn {
            self.restore_command_selection(selection);
            return;
        }
        let url = field.stringValue().to_string();
        let changed = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| apply_link(session, selection, Some(&url)).is_ok());
        if !changed {
            self.set_save_status("链接未应用：地址无效或正文为空", true);
            self.restore_command_selection(selection);
            return;
        }
        self.refresh_body_from_session();
        self.restore_command_selection(selection);
        self.save_current_note();
    }

    #[allow(deprecated)]
    fn apply_format(&self, format: TextFormat) {
        let Some(body) = self.ivars().body_view.get() else {
            self.update_formatting_buttons();
            return;
        };
        if body.hasMarkedText() {
            self.update_formatting_buttons();
            return;
        }
        let Some(range) = self.command_selection() else {
            self.update_formatting_buttons();
            return;
        };
        if self.ivars().current_note_id.borrow().is_none() {
            return;
        }
        let command = match format {
            TextFormat::Bold => InlineCommand::Bold,
            TextFormat::Italic => InlineCommand::Italic,
            TextFormat::Underline => InlineCommand::Underline,
            TextFormat::Clear => InlineCommand::Clear,
        };
        if !self.inline_command_is_applicable(range, command) {
            self.restore_command_selection(range);
            return;
        }
        if !self.save_current_note() {
            self.restore_command_selection(range);
            return;
        }
        if matches!(format, TextFormat::Clear) {
            let before_revision = self
                .ivars()
                .editor_session
                .borrow()
                .as_ref()
                .map(NativeEditorSession::revision)
                .unwrap_or_default();
            let applied = self
                .ivars()
                .editor_session
                .borrow_mut()
                .as_mut()
                .is_some_and(|session| apply_clear_formatting(session, range).is_ok());
            let after_revision = self
                .ivars()
                .editor_session
                .borrow()
                .as_ref()
                .map(NativeEditorSession::revision)
                .unwrap_or(before_revision);
            if applied && after_revision != before_revision {
                self.refresh_body_from_session();
                self.restore_command_selection(range);
                self.save_current_note();
            } else {
                self.restore_command_selection(range);
            }
            return;
        }
        let before_revision = self
            .ivars()
            .editor_session
            .borrow()
            .as_ref()
            .map(NativeEditorSession::revision)
            .unwrap_or_default();
        let applied = self
            .ivars()
            .editor_session
            .borrow_mut()
            .as_mut()
            .is_some_and(|session| apply_inline_command(session, range, command).is_ok());
        let after_revision = self
            .ivars()
            .editor_session
            .borrow()
            .as_ref()
            .map(NativeEditorSession::revision)
            .unwrap_or(before_revision);
        if applied && after_revision != before_revision {
            self.refresh_body_from_session();
            self.restore_command_selection(range);
            self.update_formatting_buttons();
            self.save_current_note();
        } else {
            self.restore_command_selection(range);
        }
    }

    fn update_formatting_buttons(&self) {
        let has_note = self.ivars().current_note_id.borrow().is_some();
        for (action, button) in self.ivars().toolbar_buttons.borrow().iter() {
            let presentation = self.current_action_presentation(*action);
            button.setEnabled(presentation.enabled);
            if let Some(title) = toolbar_state_title(*action, presentation.label) {
                button.setTitle(&NSString::from_str(title));
            } else {
                button.setTitle(ns_string!(""));
            }
            if !has_note {
                button.setHidden(true);
            }
            button.setState(match presentation.state {
                SelectionState::Active => NSControlStateValueOn,
                SelectionState::Mixed => NSControlStateValueMixed,
                SelectionState::Inactive => NSControlStateValueOff,
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
                if let Some(body) = body
                    && body_paste_dispatch(body_is_first_responder)
                        == BodyPasteDispatch::DirectTextInsertion
                {
                    let _ = paste_plain_text_into_body(body);
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
            PasteboardImage::NotImage => {
                let _ = paste_plain_text_into_body(body);
            }
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
        let has_note = self.ivars().current_note_id.borrow().is_some();
        let has_marked_text = body.hasMarkedText();
        let flush_succeeded = self.save_current_note();
        if !image_insert_gate(has_note, has_marked_text, flush_succeeded) {
            return false;
        }
        let Ok(PasteboardImage::Data { bytes, title, mime }) =
            read_drag_pasteboard(&sender.draggingPasteboard())
        else {
            return false;
        };
        let point = body.convertPoint_fromView(sender.draggingLocation(), None);
        let character_index = body.characterIndexForInsertionAtPoint(point);
        let Some(character_index) = semantic_range_from_projection(
            NSRange::new(character_index, 0),
            &self.ivars().projection_prefixes.borrow(),
        ) else {
            return false;
        };
        self.insert_image_data_at_range(&bytes, &title, &mime, body, character_index)
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

#[allow(deprecated)]
fn paste_plain_text_into_body(body: &NSTextView) -> bool {
    let pasteboard = NSPasteboard::generalPasteboard();
    let Some(text) = (unsafe { pasteboard.stringForType(NSPasteboardTypeString) }) else {
        return false;
    };
    unsafe {
        body.insertText(&text as &AnyObject);
    }
    true
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
        let Some(body) = self.ivars().body_view.get() else {
            return false;
        };
        let has_note = self.ivars().current_note_id.borrow().is_some();
        let has_marked_text = body.hasMarkedText();
        let flush_succeeded = self.save_current_note();
        if !image_insert_gate(has_note, has_marked_text, flush_succeeded) {
            return false;
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            self.set_save_status("图片未插入：超过 10 MB", true);
            return false;
        }
        if !matches!(mime, "image/png" | "image/jpeg") || !valid_image_bytes_for_mime(bytes, mime) {
            self.set_save_status("图片未插入：格式不支持", true);
            return false;
        }
        let Some(insertion_range) = semantic_range_from_projection(
            body.selectedRange(),
            &self.ivars().projection_prefixes.borrow(),
        ) else {
            return false;
        };
        self.insert_image_data_at_range(bytes, title, mime, body, insertion_range)
    }

    fn insert_image_data_at_range(
        &self,
        bytes: &[u8],
        title: &str,
        mime: &str,
        body: &NSTextView,
        insertion_range: NSRange,
    ) -> bool {
        if body.hasMarkedText() || !self.save_current_note() {
            return false;
        }
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
        let title = self
            .ivars()
            .title_field
            .get()
            .map(|field| field.stringValue().to_string())
            .unwrap_or_default();
        let (_candidate, prepared, candidate_selection) = {
            let session_guard = self.ivars().editor_session.borrow();
            let Some(session) = session_guard.as_ref() else {
                self.rollback_imported_resource(&stored.id);
                self.set_save_status("编辑器状态不可用，未覆盖正文", true);
                return false;
            };
            match prepare_image_insert_candidate(
                session,
                insertion_range,
                &stored.id,
                &stored.title,
                title,
            ) {
                Ok(candidate) => candidate,
                Err(error) => {
                    self.rollback_imported_resource(&stored.id);
                    eprintln!("native image candidate failed: {error}");
                    self.set_save_status("图片未插入：编辑器状态不可用", true);
                    return false;
                }
            }
        };
        let previous_loading_guard = *self.ivars().loading_guard.borrow();
        let previous_selection_guard = *self.ivars().selection_sync_guard.borrow();
        *self.ivars().loading_guard.borrow_mut() = true;
        *self.ivars().selection_sync_guard.borrow_mut() = true;
        let candidate_body = prepared.update.body.clone();
        let persisted = self.persist_note_content(&note_id, prepared);
        let live_result = if persisted {
            let mut session_guard = self.ivars().editor_session.borrow_mut();
            if let Some(session) = session_guard.as_mut() {
                match insert_image_block_anchor(
                    session,
                    insertion_range,
                    &stored.id,
                    &stored.title,
                    1,
                    1,
                ) {
                    Ok(selection) => match document_from_session(session) {
                        Ok(document) => Some((selection, serialize_html(&document))),
                        Err(error) => {
                            eprintln!("native image live serialization failed: {error}");
                            None
                        }
                    },
                    Err(error) => {
                        eprintln!("native image live apply failed: {error}");
                        None
                    }
                }
            } else {
                eprintln!("native image live apply failed: editor state unavailable");
                None
            }
        } else {
            None
        };
        let applied = live_result.as_ref().is_some_and(|(selection, body)| {
            *selection == candidate_selection && *body == candidate_body
        });
        let inserted = if applied {
            self.refresh_body_from_session_at(body, candidate_selection);
            true
        } else {
            false
        };
        *self.ivars().selection_sync_guard.borrow_mut() = previous_selection_guard;
        *self.ivars().loading_guard.borrow_mut() = previous_loading_guard;
        if !persisted {
            self.rollback_imported_resource(&stored.id);
        }
        if persisted && !applied {
            if let Ok(Some(note)) = self.ivars().repository.get_note(&note_id) {
                self.load_note(&note);
            }
            return false;
        }
        if !inserted && let Ok(Some(note)) = self.ivars().repository.get_note(&note_id) {
            self.ivars()
                .autosave
                .borrow_mut()
                .reset(&note.id, &note.title, &note.body);
            self.set_save_status("保存失败，内容保留待重试", true);
        }
        inserted
    }

    fn rollback_imported_resource(&self, resource_id: &str) {
        if let Err(error) = self
            .ivars()
            .repository
            .rollback_unassociated_resource(resource_id)
        {
            eprintln!("image resource metadata rollback failed: {error}");
        }
    }

    fn load_note(&self, note: &Note) {
        self.clear_pending_editor_intent();
        *self.ivars().loading_guard.borrow_mut() = true;
        *self.ivars().current_note_id.borrow_mut() = Some(note.id.clone());
        *self.ivars().last_body_selection_note_id.borrow_mut() = Some(note.id.clone());
        self.ivars()
            .autosave
            .borrow_mut()
            .reset(&note.id, &note.title, &note.body);
        self.ivars()
            .title_field
            .get()
            .unwrap()
            .setStringValue(&NSString::from_str(&note.title));
        self.ivars()
            .updated_label
            .get()
            .unwrap()
            .setStringValue(&NSString::from_str(&format!(
                "更新 {}",
                format_updated_time(note.updated_time, current_time_millis())
            )));
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
                    // Install the new semantic session before rendering the
                    // collapsed caret attributes. Otherwise
                    // install_rendered_document can consult the previous
                    // note's typing context while replacing the view.
                    *self.ivars().editor_session.borrow_mut() = Some(session);
                    let initial_selection = projection_range_from_semantic(
                        NSRange::new(0, 0),
                        &rendered.projection_prefixes,
                    )
                    .unwrap_or(NSRange::new(0, 0));
                    self.install_rendered_document(body, &rendered, initial_selection);
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
                    self.ivars().projection_prefixes.borrow_mut().clear();
                    self.ivars().projection_empty_carriers.borrow_mut().clear();
                    body.setString(ns_string!("正文无法读取"));
                    ("正文无法读取", true)
                }
            },
            Err(_) => {
                *self.ivars().editor_session.borrow_mut() = None;
                self.ivars().projection_attachments.borrow_mut().clear();
                self.ivars().projection_prefixes.borrow_mut().clear();
                self.ivars().projection_empty_carriers.borrow_mut().clear();
                body.setString(ns_string!("正文无法读取"));
                ("正文无法读取", true)
            }
        };
        *self.ivars().loading_guard.borrow_mut() = false;
        self.refresh_search_highlights(body, true);
        self.set_save_status(status, is_error);
        self.update_editor_visibility();
        self.update_note_selection();
        self.update_formatting_buttons();
    }

    fn clear_current_note(&self) {
        self.clear_pending_editor_intent();
        *self.ivars().loading_guard.borrow_mut() = true;
        *self.ivars().current_note_id.borrow_mut() = None;
        *self.ivars().last_body_selection_note_id.borrow_mut() = None;
        self.ivars().autosave.borrow_mut().clear();
        *self.ivars().editor_session.borrow_mut() = None;
        self.ivars().projection_attachments.borrow_mut().clear();
        self.ivars().projection_prefixes.borrow_mut().clear();
        self.ivars().projection_empty_carriers.borrow_mut().clear();
        self.ivars()
            .title_field
            .get()
            .unwrap()
            .setStringValue(ns_string!(""));
        self.ivars()
            .updated_label
            .get()
            .unwrap()
            .setStringValue(ns_string!(""));
        self.ivars()
            .body_view
            .get()
            .unwrap()
            .setString(ns_string!(""));
        *self.ivars().loading_guard.borrow_mut() = false;
        self.refresh_search_highlights(self.ivars().body_view.get().unwrap(), false);
        self.update_editor_visibility();
        self.update_note_selection();
    }

    fn update_editor_visibility(&self) {
        let has_note = self.ivars().current_note_id.borrow().is_some();
        if let Some(view) = self.ivars().title_field.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().breadcrumb_label.get() {
            view.setHidden(!has_note);
        }
        if let Some(view) = self.ivars().updated_label.get() {
            view.setHidden(!has_note);
        }
        let visible_actions = self
            .ivars()
            .window
            .get()
            .and_then(|window| window.contentView())
            .map(|content| {
                shell_layout(
                    content.frame().size.width,
                    content.frame().size.height,
                    *self.ivars().shell_visibility.borrow(),
                )
                .toolbar
                .width
            })
            .map(toolbar_actions_for_width)
            .unwrap_or_else(|| toolbar_actions_for_width(0.0));
        for (action, button) in self.ivars().toolbar_buttons.borrow().iter() {
            button.setHidden(!has_note || !visible_actions.contains(action));
            button.setEnabled(has_note);
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
            let body_empty = self
                .ivars()
                .body_view
                .get()
                .is_none_or(|body| !body.string().is_empty());
            label.setHidden(!has_note || body_empty);
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
        let query = self.ivars().note_filter_query.borrow().clone();
        self.search_notes(&query);
    }

    fn note_list_items(
        &self,
        query: &str,
    ) -> Result<Vec<NoteListItem>, joplin_lite_native::core::CoreError> {
        if query.trim().is_empty() {
            self.ivars().repository.list_note_previews()
        } else {
            self.ivars().repository.search_note_previews(query)
        }
    }

    fn search_notes(&self, query: &str) {
        *self.ivars().note_filter_query.borrow_mut() = query.to_owned();
        if let Ok(items) = self.note_list_items(query) {
            self.replace_note_list(items);
        }
        if let Some(body) = self.ivars().body_view.get() {
            self.refresh_search_highlights(body, false);
        }
    }

    fn replace_note_list(&self, items: Vec<NoteListItem>) {
        self.replace_note_list_with_mode(items, false);
    }

    fn replace_note_list_preserving_editor(&self, items: Vec<NoteListItem>) {
        self.replace_note_list_with_mode(items, true);
    }

    fn replace_note_list_with_mode(
        &self,
        items: Vec<NoteListItem>,
        preserve_editor_geometry: bool,
    ) {
        let Some(collection) = self.ivars().note_collection.get() else {
            return;
        };
        let now = current_time_millis();
        let previews = items
            .iter()
            .map(|item| preview_from_list_item(item, now))
            .collect::<Vec<_>>();
        let update = preview_list_update(&self.ivars().note_previews.borrow(), &previews);
        *self.ivars().note_previews.borrow_mut() = previews;
        let should_relayout =
            should_relayout_editor_after_preview_update(&update, preserve_editor_geometry);
        match update {
            PreviewListUpdate::ReloadAll => collection.reloadData(),
            PreviewListUpdate::ReloadIndices(indices) => {
                let paths = indices
                    .into_iter()
                    .map(|index| NSIndexPath::indexPathForItem_inSection(index as isize, 0))
                    .collect::<Vec<_>>();
                if !paths.is_empty() {
                    let refs = paths.iter().map(|path| &**path).collect::<Vec<_>>();
                    collection.reloadItemsAtIndexPaths(&NSSet::from_slice(&refs));
                }
            }
        }
        let is_empty = self.ivars().note_previews.borrow().is_empty();
        if let Some(label) = self.ivars().list_empty_label.get() {
            label.setHidden(!is_empty);
        }
        if should_relayout {
            let (width, height) = self
                .ivars()
                .window
                .get()
                .and_then(|window| window.contentView())
                .map(|content| (content.frame().size.width, content.frame().size.height))
                .unwrap_or((1100.0, 720.0));
            self.layout_content(width, height);
        }
        self.update_note_selection();
    }

    fn update_note_selection(&self) {
        let Some(collection) = self.ivars().note_collection.get() else {
            return;
        };
        let was_guarded = self.ivars().collection_selection_guard.replace(true);
        unsafe {
            collection.deselectAll(None);
        }
        let ids = self
            .ivars()
            .note_previews
            .borrow()
            .iter()
            .map(|preview| preview.note_id.clone())
            .collect::<Vec<_>>();
        let selected_id = self.ivars().current_note_id.borrow().clone();
        if let Some(index) = selected_index_for_id(&ids, selected_id.as_deref()) {
            let path = NSIndexPath::indexPathForItem_inSection(index as isize, 0);
            let paths = NSSet::from_slice(&[&*path]);
            collection.selectItemsAtIndexPaths_scrollPosition(
                &paths,
                objc2_app_kit::NSCollectionViewScrollPosition::None,
            );
        }
        self.ivars().collection_selection_guard.set(was_guarded);
    }

    #[allow(deprecated)]
    fn save_current_note(&self) -> bool {
        if self.ivars().current_note_id.borrow().is_none() {
            return true;
        }
        let body_marked = self
            .ivars()
            .body_view
            .get()
            .is_some_and(|body| body.hasMarkedText());
        if should_defer_persistence(
            body_marked,
            self.title_field_has_marked_text(),
            *self.ivars().loading_guard.borrow(),
        ) {
            return false;
        }
        self.save_current_note_unchecked()
    }

    fn title_field_has_marked_text(&self) -> bool {
        self.ivars()
            .title_field
            .get()
            .and_then(|field| field.currentEditor())
            .and_then(|editor: Retained<NSText>| editor.downcast::<NSTextView>().ok())
            .is_some_and(|editor| editor.hasMarkedText())
    }

    fn prepared_current_note_content(&self) -> Option<PreparedNoteContent> {
        let title = self
            .ivars()
            .title_field
            .get()
            .map(|field| field.stringValue().to_string())
            .unwrap_or_default();
        let session_guard = self.ivars().editor_session.borrow();
        let session = session_guard.as_ref()?;
        let document = document_from_session(session).ok()?;
        Some(PreparedNoteContent {
            update: NoteContentUpdate {
                title,
                body: serialize_html(&document),
            },
        })
    }

    fn schedule_autosave_after(&self, note_id: &str, epoch: u64, generation: u64, delay: f64) {
        self.ivars()
            .autosave
            .borrow_mut()
            .mark_retry_scheduled_with_epoch(epoch, generation);
        let token = NSString::from_str(&format!("{note_id}\u{1f}{epoch}\u{1f}{generation}"));
        unsafe {
            let _: () = msg_send![
                self,
                performSelector: sel!(runAutosave:),
                withObject: &*token,
                afterDelay: delay
            ];
        }
    }

    fn schedule_autosave(&self, note_id: &str, epoch: u64, generation: u64) {
        self.schedule_autosave_after(note_id, epoch, generation, 0.3);
    }

    fn resume_deferred_autosave_if_ready(&self) {
        if should_defer_persistence(
            self.ivars()
                .body_view
                .get()
                .is_some_and(|body| body.hasMarkedText()),
            self.title_field_has_marked_text(),
            *self.ivars().loading_guard.borrow(),
        ) || !self.ivars().pending_editor_intents.borrow().is_empty()
            || self.ivars().pending_editor_composition.borrow().is_some()
        {
            return;
        }
        let Some(note_id) = self.ivars().current_note_id.borrow().clone() else {
            return;
        };
        let resumed = {
            let mut autosave = self.ivars().autosave.borrow_mut();
            let epoch = autosave.epoch;
            let generation = autosave.deferred_generation;
            generation
                .and_then(|generation| {
                    autosave.resume_deferred_timer_with_epoch(&note_id, epoch, generation)
                })
                .map(|generation| (epoch, generation))
        };
        if let Some((epoch, generation)) = resumed {
            self.schedule_autosave(&note_id, epoch, generation);
        }
    }

    fn mark_current_note_dirty(&self) {
        let body_marked = self
            .ivars()
            .body_view
            .get()
            .is_some_and(|body| body.hasMarkedText());
        if should_defer_persistence(
            body_marked,
            self.title_field_has_marked_text(),
            *self.ivars().loading_guard.borrow(),
        ) {
            return;
        }
        let Some(id) = self.ivars().current_note_id.borrow().clone() else {
            return;
        };
        let Some(prepared) = self.prepared_current_note_content() else {
            self.set_save_status("保存失败", true);
            return;
        };
        let generation = self.ivars().autosave.borrow_mut().mark_dirty(
            &id,
            &prepared.update.title,
            &prepared.update.body,
        );
        if let Some(generation) = generation {
            self.set_save_status("未保存", false);
            let epoch = self.ivars().autosave.borrow().epoch;
            self.schedule_autosave(&id, epoch, generation);
        }
    }

    fn save_current_note_unchecked(&self) -> bool {
        let Some(id) = self.ivars().current_note_id.borrow().clone() else {
            return true;
        };
        let Some(prepared) = self.prepared_current_note_content() else {
            self.set_save_status("编辑器状态不可用，未覆盖正文", true);
            return false;
        };
        let _generation = self.ivars().autosave.borrow_mut().mark_dirty(
            &id,
            &prepared.update.title,
            &prepared.update.body,
        );
        let decision = self.ivars().autosave.borrow().flush_decision(&id);
        match decision {
            AutosaveDecision::Noop => {
                self.set_save_status("已保存", false);
                true
            }
            AutosaveDecision::Stale => false,
            AutosaveDecision::Persist { generation, .. } => {
                let ok = self.persist_note_content(&id, prepared);
                if ok {
                    self.ivars().autosave.borrow_mut().mark_saved(generation);
                }
                ok
            }
        }
    }

    fn persist_note_content(&self, id: &str, prepared: PreparedNoteContent) -> bool {
        let PreparedNoteContent { update } = prepared;
        let generation =
            self.ivars()
                .autosave
                .borrow_mut()
                .mark_dirty(id, &update.title, &update.body);
        match self.ivars().repository.update_note_content(id, update) {
            Ok(updated) => {
                // Re-query the lightweight projection so title/body changes are
                // immediately re-filtered and updated_time ordering is restored.
                // The diff helper reloads only affected cards when membership and
                // order are unchanged; otherwise the collection performs one
                // structural reload.
                let query = self.ivars().note_filter_query.borrow().clone();
                if let Ok(items) = self.note_list_items(&query) {
                    self.replace_note_list_preserving_editor(items);
                }
                if self.ivars().current_note_id.borrow().as_deref() == Some(id)
                    && let Some(label) = self.ivars().updated_label.get()
                {
                    label.setStringValue(&NSString::from_str(&format!(
                        "更新 {}",
                        format_updated_time(updated.updated_time, current_time_millis())
                    )));
                }
                self.set_save_status("已保存", false);
                if let Some(generation) = generation {
                    self.ivars().autosave.borrow_mut().mark_saved(generation);
                }
                true
            }
            Err(error) => {
                eprintln!("autosave failed: {error}");
                if let Some(generation) = generation {
                    let epoch = self.ivars().autosave.borrow().epoch;
                    let delay = self
                        .ivars()
                        .autosave
                        .borrow_mut()
                        .mark_failed_with_epoch(epoch, generation);
                    if let Some(delay) = delay {
                        self.schedule_autosave_after(id, epoch, generation, delay);
                    }
                }
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

fn image_insert_gate(has_note: bool, has_marked_text: bool, flush_succeeded: bool) -> bool {
    has_note && !has_marked_text && flush_succeeded
}

#[allow(deprecated)]
fn insert_inline_attachment(body: &NSTextView, inline: &NSMutableAttributedString) {
    unsafe { body.insertText(inline as &AnyObject) };
    resize_inline_attachments(body);
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

fn search_match_ranges(text: &str, query: &str) -> Vec<NSRange> {
    let trimmed = query.trim();
    if trimmed.is_empty() || trimmed.contains('\u{fffc}') {
        return Vec::new();
    }

    let mut terms = Vec::<Vec<char>>::new();
    for candidate in std::iter::once(trimmed).chain(trimmed.split_whitespace()) {
        if candidate.is_empty() || candidate.contains('\u{fffc}') {
            continue;
        }
        let folded = candidate.chars().map(ascii_fold_char).collect::<Vec<_>>();
        if folded.is_empty() || terms.iter().any(|term| term == &folded) {
            continue;
        }
        terms.push(folded);
    }
    if terms.is_empty() {
        return Vec::new();
    }

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

    let mut ranges = Vec::new();
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
            let matches = chars[index..index + term.len()]
                .iter()
                .map(|character| ascii_fold_char(*character))
                .eq(term.iter().copied());
            matches.then_some(term.len())
        });
        if let Some(length) = matched {
            ranges.push(NSRange::new(
                utf16_offsets[index],
                utf16_offsets[index + length] - utf16_offsets[index],
            ));
            index += length;
        } else {
            index += 1;
        }
    }
    ranges
}

fn ascii_fold_char(character: char) -> char {
    character.to_ascii_lowercase()
}

fn clear_search_highlights(layout_manager: &NSLayoutManager, text_length: usize) {
    if text_length == 0 {
        return;
    }
    unsafe {
        layout_manager.removeTemporaryAttribute_forCharacterRange(
            NSBackgroundColorAttributeName,
            NSRange::new(0, text_length),
        );
    }
}

fn apply_search_highlights(
    layout_manager: &NSLayoutManager,
    text: &str,
    query: &str,
) -> Vec<NSRange> {
    let ranges = search_match_ranges(text, query);
    clear_search_highlights(layout_manager, text.encode_utf16().count());
    if ranges.is_empty() {
        return ranges;
    }
    let color = NSColor::systemYellowColor().colorWithAlphaComponent(0.32);
    for range in &ranges {
        unsafe {
            layout_manager.addTemporaryAttribute_value_forCharacterRange(
                NSBackgroundColorAttributeName,
                &color,
                *range,
            );
        }
    }
    ranges
}

fn inline_image_display_size(image_size: NSSize, available_width: f64) -> NSSize {
    let width = image_size.width.max(1.0);
    let height = image_size.height.max(1.0);
    let max_width = available_width.clamp(1.0, 640.0);
    let scale = (max_width / width).min(640.0 / height).min(1.0);
    NSSize::new(width * scale, height * scale)
}

fn apply_image_paragraph_style(
    storage: &NSTextStorage,
    attachment_location: usize,
    available_width: f64,
) {
    let length = storage.length();
    if attachment_location >= length {
        return;
    }
    let paragraph_range = storage
        .string()
        .paragraphRangeForRange(NSRange::new(attachment_location, 1));
    let paragraph_style = unsafe {
        storage
            .attribute_atIndex_effectiveRange(
                NSParagraphStyleAttributeName,
                paragraph_range.location,
                null_mut(),
            )
            .and_then(|value| value.downcast::<NSParagraphStyle>().ok())
    };
    let mutable = paragraph_style
        .map(|style| style.mutableCopy())
        .unwrap_or_else(NSMutableParagraphStyle::new);
    let desired_tail_indent = image_paragraph_tail_indent(available_width);
    if (mutable.tailIndent() - desired_tail_indent).abs() < f64::EPSILON {
        return;
    }
    mutable.setTailIndent(desired_tail_indent);
    unsafe {
        storage.addAttribute_value_range(NSParagraphStyleAttributeName, &mutable, paragraph_range);
    }
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
            apply_image_paragraph_style(storage.as_ref(), location, available_width);
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

fn selection_snapshot_for_reentrant_appkit(cell: &RefCell<NSRange>) -> NSRange {
    let selection = cell.borrow();
    *selection
}

fn inline_attachment_with_width(
    resource: &joplin_lite_native::core::StoredResource,
    alt: &str,
    available_width: f64,
) -> Option<Retained<NSMutableAttributedString>> {
    let image = editor_attachment_image(&resource.bytes)?;
    let attachment = NSTextAttachment::init(NSTextAttachment::alloc());
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
            note_previews: RefCell::new(Vec::new()),
            note_filter_query: RefCell::new(String::new()),
            thumbnail_cache: RefCell::new(ThumbnailCache::new(32)),
            thumbnail_decode_count: Cell::new(0),
            thumbnail_decode_attempts: Cell::new(0),
            thumbnail_negative_count: Cell::new(0),
            thumbnail_requests: RefCell::new(ThumbnailRequestLedger::new(32, 64)),
            thumbnail_drain_scheduled: Cell::new(false),
            loading_guard: RefCell::new(false),
            autosave: RefCell::new(AutosaveState::empty()),
            shell_visibility: RefCell::new(ShellVisibility::Default),
            focus_restore_visibility: RefCell::new(None),
            last_body_selection: RefCell::new(NSRange::new(0, 0)),
            last_body_selection_note_id: RefCell::new(None),
            selection_sync_guard: RefCell::new(false),
            collection_selection_guard: Cell::new(false),
            editor_session: RefCell::new(None),
            projection_attachments: RefCell::new(Vec::new()),
            projection_prefixes: RefCell::new(Vec::new()),
            projection_empty_carriers: RefCell::new(Vec::new()),
            pending_editor_intents: RefCell::new(Vec::new()),
            pending_editor_composition: RefCell::new(None),
            pending_editor_intent_invalid: RefCell::new(false),
            sidebar_background: OnceCell::new(),
            browser_background: OnceCell::new(),
            editor_background: OnceCell::new(),
            sidebar_separator: OnceCell::new(),
            browser_scroll: OnceCell::new(),
            note_collection: OnceCell::new(),
            browser_title: OnceCell::new(),
            browser_count: OnceCell::new(),
            breadcrumb_label: OnceCell::new(),
            updated_label: OnceCell::new(),
            list_empty_label: OnceCell::new(),
            library_label: OnceCell::new(),
            new_button: OnceCell::new(),
            search_field: OnceCell::new(),
            title_field: OnceCell::new(),
            focus_button: OnceCell::new(),
            browser_toggle_button: OnceCell::new(),
            body_scroll: OnceCell::new(),
            body_view: OnceCell::new(),
            delete_button: OnceCell::new(),
            save_status: OnceCell::new(),
            editor_empty_label: OnceCell::new(),
            toolbar_buttons: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DataDirError, DataFileError, FontTraitOperation, FormatDecision, FormatTarget,
        LegacyMigrationFailure, Note, PasteFileError, PasteRoute, PasteboardImage,
        PreviewListUpdate, TextFormat, apply_image_paragraph_style, candidate_with_attachment,
        choose_data_dir, display_note_title, document_from_attributed_string,
        ensure_notes_database_file, format_decision, format_target, image_signature_matches_mime,
        inline_attachment_with_width, inline_image_display_size, is_local_file_url_host,
        is_promised_pasteboard_type, legacy_migration_recovery_message, note_list_summary,
        note_list_title, paste_route, read_drag_image_file, read_pasteboard_image_from,
        read_regular_image_file, render_document_to_attributed_string, render_session,
        selection_snapshot_for_reentrant_appkit, typing_trait_operation,
        valid_image_bytes_for_mime, validate_canonical_data_dir,
    };
    use joplin_lite_native::core::{LegacyNoteForHtmlMigration, NoteRepository, StoredResource};
    use joplin_lite_native::html_body::{Block, Document, Inline, Marks, serialize_html};
    use objc2::{AnyThread, runtime::AnyObject};
    use objc2_app_kit::{
        NSAttachmentAttributeName, NSAttributedStringAttachmentConveniences,
        NSBackgroundColorAttributeName, NSBitmapImageFileType, NSBitmapImageRep,
        NSFontAttributeName, NSLayoutManager, NSMutableParagraphStyle, NSPasteboard,
        NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeTIFF, NSTextAttachment,
        NSTextContainer, NSTextStorage, NSUnderlineStyle, NSUnderlineStyleAttributeName,
    };
    use objc2_foundation::{
        NSAttributedString, NSData, NSDictionary, NSMutableAttributedString, NSRange, NSSize,
        NSString, NSURL,
    };
    use std::cell::RefCell;
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
    const LARGE_TEST_PNG: &[u8] = include_bytes!("../assets/AppIcon-source.png");

    fn paragraph(inlines: Vec<Inline>) -> Block {
        Block::Paragraph {
            style: Default::default(),
            inlines,
        }
    }

    #[test]
    fn collapsed_typing_projection_carries_all_inline_marks() {
        let format = text_document::TextFormat {
            font_bold: Some(true),
            font_italic: Some(true),
            font_underline: Some(true),
            font_strikeout: Some(true),
            background_color: Some(text_document::Color::rgb(255, 230, 120)),
            clear_link: true,
            ..Default::default()
        };
        assert_eq!(
            super::typing_format_projection(&format),
            super::TypingFormatProjection {
                font_bold: Some(true),
                font_italic: Some(true),
                font_underline: Some(true),
                font_strikeout: Some(true),
                has_background: true,
                clear_background: false,
                clear_link: true,
            }
        );
    }

    #[test]
    fn default_typing_projection_clears_old_background_and_link() {
        let projection = super::typing_format_projection(&text_document::TextFormat::default());
        assert!(projection.clear_background);
        assert!(projection.clear_link);
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
            semantic_range: None,
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
            semantic_range: None,
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
    fn duplicate_appkit_text_intent_is_idempotent_before_text_did_change() {
        let intent = super::PendingEditorIntent {
            range: NSRange::new(1, 0),
            semantic_range: None,
            replacement: "Q".into(),
            old_view_text: "前\u{fffc}后".into(),
            old_semantic_text: "前\u{fffc}后".into(),
            covered_attachments: Vec::new(),
        };
        let mut pending = vec![intent.clone()];
        assert!(super::append_pending_editor_intent(&mut pending, intent));
        assert_eq!(pending.len(), 1);
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
            semantic_range: None,
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
            semantic_range: None,
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
                semantic_range: None,
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
            projection_prefixes: Vec::new(),
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
    fn live_rendered_attachment_has_image_and_visible_geometry() {
        let resource_id = "0123456789abcdef0123456789abcdef";
        let document = Document::from_blocks(vec![paragraph(vec![
            Inline::Text {
                text: "前".into(),
                marks: Marks::default(),
            },
            Inline::Image {
                resource_id: resource_id.into(),
                alt: "截图".into(),
            },
            Inline::Text {
                text: "后".into(),
                marks: Marks::default(),
            },
        ])]);
        let session = super::session_from_document(&document).unwrap();
        let rendered = render_session(
            &session,
            |id| {
                Some(StoredResource {
                    id: id.into(),
                    sha256: "0".repeat(64),
                    size: LARGE_TEST_PNG.len(),
                    title: "截图.png".into(),
                    mime: "image/png".into(),
                    file_extension: "png".into(),
                    path: PathBuf::new(),
                    bytes: LARGE_TEST_PNG.to_vec(),
                })
            },
            640.0,
        );
        let source: &NSAttributedString = &rendered.attributed;
        let attachment_key = unsafe { NSAttachmentAttributeName };
        let mut effective_range = NSRange::new(0, 0);
        let attributes = unsafe {
            source.attributesAtIndex_longestEffectiveRange_inRange(
                2,
                &mut effective_range,
                NSRange::new(0, source.length()),
            )
        };
        let attachment = unsafe { attributes.objectForKey_unchecked(attachment_key) }
            .and_then(|value| value.downcast_ref::<NSTextAttachment>())
            .expect("rendered semantic image must carry NSTextAttachment");
        assert!(attachment.image().is_some());
        let bounds = attachment.bounds();
        assert!(bounds.size.width > 0.0 && bounds.size.height > 0.0);
        let image_size = attachment
            .image()
            .expect("display image must be attached")
            .size();
        assert!(image_size.width > 0.0 && image_size.height > 0.0);
        assert_eq!(bounds.size, inline_image_display_size(image_size, 640.0));
        assert_eq!(rendered.attributed.string().to_string(), "前\n\u{fffc}\n后");

        let resource = StoredResource {
            id: resource_id.into(),
            sha256: "0".repeat(64),
            size: TEST_PNG.len(),
            title: "截图.png".into(),
            mime: "image/png".into(),
            file_extension: "png".into(),
            path: PathBuf::new(),
            bytes: TEST_PNG.to_vec(),
        };
        let inserted = super::inline_attachment_with_width(&resource, "截图", 640.0)
            .expect("live insert must produce a display attachment");
        let inserted_source: &NSAttributedString = &inserted;
        let mut inserted_effective_range = NSRange::new(0, 0);
        let inserted_attributes = unsafe {
            inserted_source.attributesAtIndex_longestEffectiveRange_inRange(
                0,
                &mut inserted_effective_range,
                NSRange::new(0, inserted_source.length()),
            )
        };
        let inserted_attachment =
            unsafe { inserted_attributes.objectForKey_unchecked(attachment_key) }
                .and_then(|value| value.downcast_ref::<NSTextAttachment>())
                .expect("live insert must carry NSTextAttachment");
        assert!(inserted_attachment.bounds().size.width > 0.0);
        assert!(inserted_attachment.image().is_some());
    }

    #[test]
    fn relayout_selection_snapshot_releases_borrow_before_appkit_reentry() {
        let remembered = RefCell::new(NSRange::new(4, 2));
        let snapshot = selection_snapshot_for_reentrant_appkit(&remembered);
        *remembered.borrow_mut() = NSRange::new(0, 0);
        assert_eq!(snapshot, NSRange::new(4, 2));
    }

    #[test]
    fn textkit_places_text_after_inline_image_on_a_new_left_aligned_line() {
        let resource_id = "0123456789abcdef0123456789abcdef";
        let document = Document::from_blocks(vec![paragraph(vec![
            Inline::Text {
                text: "前".into(),
                marks: Marks::default(),
            },
            Inline::Image {
                resource_id: resource_id.into(),
                alt: "截图".into(),
            },
            Inline::Text {
                text: "需后".into(),
                marks: Marks::default(),
            },
        ])]);
        let session = super::session_from_document(&document).unwrap();
        let rendered = render_session(
            &session,
            |id| {
                Some(StoredResource {
                    id: id.into(),
                    sha256: "0".repeat(64),
                    size: LARGE_TEST_PNG.len(),
                    title: "截图.png".into(),
                    mime: "image/png".into(),
                    file_extension: "png".into(),
                    path: PathBuf::new(),
                    bytes: LARGE_TEST_PNG.to_vec(),
                })
            },
            680.0,
        );
        let storage = NSTextStorage::new();
        let layout = NSLayoutManager::new();
        let container = NSTextContainer::initWithContainerSize(
            NSTextContainer::alloc(),
            NSSize::new(680.0, 1200.0),
        );
        storage.addLayoutManager(&layout);
        layout.addTextContainer(&container);
        container.setLineFragmentPadding(0.0);
        storage.setAttributedString(&rendered.attributed);
        layout.ensureLayoutForTextContainer(&container);

        let mut attachment_effective_range = NSRange::new(0, 0);
        let attachment_line = unsafe {
            layout
                .lineFragmentRectForGlyphAtIndex_effectiveRange(2, &mut attachment_effective_range)
        };
        let mut following_effective_range = NSRange::new(0, 0);
        let following_line = unsafe {
            layout.lineFragmentRectForGlyphAtIndex_effectiveRange(4, &mut following_effective_range)
        };
        let following_location = layout.locationForGlyphAtIndex(4);
        assert!(
            following_line.origin.y > attachment_line.origin.y,
            "following text must be below image line: image={attachment_line:?}, text={following_line:?}"
        );
        assert!(
            following_location.x <= 1.0,
            "following text must restart at the document left edge: {following_location:?}"
        );
    }

    #[test]
    fn live_insert_projection_applies_image_block_style_without_changing_text() {
        let resource = StoredResource {
            id: "0123456789abcdef0123456789abcdef".into(),
            sha256: "0".repeat(64),
            size: LARGE_TEST_PNG.len(),
            title: "截图.png".into(),
            mime: "image/png".into(),
            file_extension: "png".into(),
            path: PathBuf::new(),
            bytes: LARGE_TEST_PNG.to_vec(),
        };
        let inline = inline_attachment_with_width(&resource, "截图", 680.0).unwrap();
        assert_eq!(inline.string().to_string(), "\u{fffc}");
        assert!(inline_image_display_size(NSSize::new(1280.0, 1280.0), 680.0).width <= 640.0);

        let source = NSMutableAttributedString::from_nsstring(&NSString::from_str("前需后"));
        let source_ref: &NSAttributedString = &source;
        let candidate = candidate_with_attachment(source_ref, NSRange::new(1, 0), &inline)
            .expect("image insertion candidate must preserve a valid range");
        assert_eq!(candidate.string().to_string(), "前\u{fffc}需后");

        let storage = NSTextStorage::new();
        let layout = NSLayoutManager::new();
        let container = NSTextContainer::initWithContainerSize(
            NSTextContainer::alloc(),
            NSSize::new(680.0, 1200.0),
        );
        storage.addLayoutManager(&layout);
        layout.addTextContainer(&container);
        container.setLineFragmentPadding(0.0);
        storage.setAttributedString(&candidate);
        apply_image_paragraph_style(&storage, 1, 680.0);
        layout.ensureLayoutForTextContainer(&container);

        let mut attachment_effective_range = NSRange::new(0, 0);
        let attachment_line = unsafe {
            layout
                .lineFragmentRectForGlyphAtIndex_effectiveRange(1, &mut attachment_effective_range)
        };
        let mut following_effective_range = NSRange::new(0, 0);
        let following_line = unsafe {
            layout.lineFragmentRectForGlyphAtIndex_effectiveRange(2, &mut following_effective_range)
        };
        let following_location = layout.locationForGlyphAtIndex(2);
        assert!(following_line.origin.y > attachment_line.origin.y);
        assert!(following_location.x <= 1.0);
        assert_eq!(storage.string().to_string(), "前\u{fffc}需后");
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
    fn shell_layout_keeps_editor_regions_disjoint_at_supported_sizes() {
        for (width, height) in [(1380.0, 820.0), (1100.0, 700.0)] {
            let layout = super::shell_layout(width, height, super::ShellVisibility::Default);
            assert!((176.0..=208.0).contains(&layout.navigation.width));
            assert!(layout.body.width <= 680.0);
            assert!(layout.body.y + layout.body.height <= layout.toolbar.y);
            assert!(layout.body.y >= 0.0);
            assert!(layout.body.y + layout.body.height <= height);
            assert!(layout.title.x >= layout.navigation.x + layout.navigation.width);
            assert_eq!(layout.empty_editor.height, 44.0);
            let expected_center = layout.body.y + layout.body.height * 0.45;
            let actual_center = layout.empty_editor.y + layout.empty_editor.height * 0.5;
            assert!((actual_center - expected_center).abs() < f64::EPSILON);
        }
        let wide = super::shell_layout(1800.0, 900.0, super::ShellVisibility::Default);
        assert_eq!(wide.body.width, 680.0);
        assert_eq!(
            wide.body.x + wide.body.width,
            wide.status.x + wide.status.width + 10.0
        );
    }

    #[test]
    fn red_shell_layout_has_three_regions_and_fixed_editor_measure() {
        let layout = super::shell_layout(1380.0, 820.0, super::ShellVisibility::Default);
        assert!((176.0..=208.0).contains(&layout.navigation.width));
        assert!((360.0..=400.0).contains(&layout.browser.width));
        assert_eq!(layout.document_measure, 680.0);
        assert!(layout.navigation.right() <= layout.browser.x);
        assert!(layout.browser.right() <= layout.editor.x);
        assert!(layout.editor.right() <= 1380.0);
        assert!(layout.sheet.right() <= layout.editor.right());
        assert!(layout.toolbar.y >= layout.body.y + layout.body.height);
        assert!(layout.status.y >= 0.0);
    }

    #[test]
    fn red_shell_layout_supports_collapsed_browser_and_focus_mode() {
        let collapsed =
            super::shell_layout(1100.0, 700.0, super::ShellVisibility::BrowserCollapsed);
        assert!(collapsed.navigation.width > 0.0);
        assert_eq!(collapsed.browser.width, 0.0);
        assert_eq!(collapsed.editor.x, collapsed.navigation.width);
        let focus = super::shell_layout(1100.0, 700.0, super::ShellVisibility::Focus);
        assert_eq!(focus.navigation.width, 0.0);
        assert_eq!(focus.browser.width, 0.0);
        assert_eq!(focus.editor.x, 0.0);
        assert_eq!(focus.editor.width, 1100.0);
        assert!(focus.document_measure <= 680.0);
    }

    #[test]
    fn red_toolbar_catalogue_has_one_real_action_source() {
        let catalogue = super::editor_action_catalogue();
        assert!(
            catalogue
                .iter()
                .any(|item| item.action == super::EditorAction::InsertImage)
        );
        assert!(
            catalogue
                .iter()
                .any(|item| item.action == super::EditorAction::More)
        );
        assert!(
            catalogue
                .iter()
                .any(|item| item.action == super::EditorAction::Link)
        );
        assert!(
            catalogue
                .iter()
                .any(|item| item.action == super::EditorAction::Checklist)
        );
        let ids = catalogue.iter().map(|item| item.action).collect::<Vec<_>>();
        let mut unique = ids.clone();
        unique.sort_by_key(|action| *action as u8);
        unique.dedup();
        assert_eq!(ids.len(), unique.len());
        assert!(!super::toolbar_actions_for_width(600.0).contains(&super::EditorAction::Link));
        assert!(super::toolbar_actions_for_width(900.0).contains(&super::EditorAction::Link));
        for descriptor in catalogue {
            if descriptor.action != super::EditorAction::BlockStyle {
                assert!(
                    super::toolbar_symbol_name(descriptor.action).is_some(),
                    "{} must use a native SF Symbol",
                    descriptor.label
                );
            }
        }
    }

    #[test]
    fn red_compact_toolbar_keeps_every_fixed_action_and_more_reachable() {
        let actions = super::toolbar_actions_for_width(412.0);
        assert_eq!(actions.len(), 12);
        assert_eq!(actions.last(), Some(&super::EditorAction::More));
        let group_gaps = 4.0 * 4.0;
        assert!((actions.len() as f64 * 32.0) + group_gaps <= 412.0);
    }

    #[test]
    fn red_navigation_controls_stay_inside_the_192_point_rail() {
        let layout = super::shell_layout(1100.0, 700.0, super::ShellVisibility::Default);
        let new_note = super::LayoutRect {
            x: 16.0,
            y: 0.0,
            width: 160.0,
            height: 30.0,
        };
        let search = super::LayoutRect {
            x: 16.0,
            y: 0.0,
            width: 160.0,
            height: 30.0,
        };
        assert!(new_note.right() <= layout.navigation.right());
        assert!(search.right() <= layout.navigation.right());
    }

    #[test]
    fn red_empty_autosave_state_represents_no_current_note() {
        assert!(super::AutosaveState::empty().note_id.is_none());
    }

    #[test]
    fn red_failed_autosave_waits_before_retrying_the_same_generation() {
        let mut state = super::AutosaveState::loaded("note-a", "标题", "<p>旧</p>");
        assert_eq!(state.mark_dirty("note-a", "标题", "<p>新</p>"), Some(1));
        state.mark_failed(1);
        assert_eq!(state.scheduled_generation, None);
        for _ in 1..super::AUTOSAVE_MAX_RETRIES {
            state.mark_retry_scheduled(1);
            state.mark_failed(1);
        }
        assert_eq!(state.mark_failed(1), None);
        assert_eq!(state.retry_generation, None);
    }

    #[test]
    fn red_autosave_rejects_an_old_same_note_token_after_an_aba_switch() {
        let mut state = super::AutosaveState::loaded("note-a", "标题", "<p>旧</p>");
        assert_eq!(state.mark_dirty("note-a", "标题", "<p>A1</p>"), Some(1));
        let old_epoch = state.epoch;
        state.reset("note-b", "标题", "<p>B</p>");
        state.reset("note-a", "标题", "<p>A1</p>");
        assert_eq!(state.mark_dirty("note-a", "标题", "<p>A2</p>"), Some(1));
        assert_eq!(
            state.timer_decision_with_epoch("note-a", old_epoch, 1),
            super::AutosaveDecision::Stale
        );
    }

    #[test]
    fn red_toolbar_descriptors_distinguish_toggle_popup_and_momentary_actions() {
        assert_eq!(
            super::editor_action_kind(super::EditorAction::Bold),
            super::EditorActionKind::Toggle
        );
        assert_eq!(
            super::editor_action_kind(super::EditorAction::BlockStyle),
            super::EditorActionKind::Popup
        );
        assert_eq!(
            super::editor_action_kind(super::EditorAction::Undo),
            super::EditorActionKind::Momentary
        );
    }

    #[test]
    fn red_title_field_marked_text_defers_persistence() {
        assert!(super::should_defer_persistence(false, true, false));
        assert!(super::should_defer_persistence(true, false, false));
        assert!(!super::should_defer_persistence(false, false, false));
    }

    #[test]
    fn red_updated_time_is_short_and_regions_do_not_overlap() {
        let now = 1_700_000_000_000_i64;
        assert_eq!(super::format_updated_time(now, now), "刚刚");
        assert_eq!(
            super::format_updated_time(now - 3 * 60 * 1000, now),
            "3分钟前"
        );
        let layout = super::shell_layout(1380.0, 820.0, super::ShellVisibility::Default);
        assert!(layout.updated.y >= layout.toolbar.top());
        let focus_button = super::LayoutRect {
            x: layout.sheet.right() - 66.0,
            y: layout.sheet.top() - 42.0,
            width: 58.0,
            height: 24.0,
        };
        assert!(layout.breadcrumb.right() <= focus_button.x);
    }

    #[test]
    fn red_focus_visibility_restores_prior_browser_state_without_opening_both() {
        let (focus, restore) =
            super::toggle_focus_visibility(super::ShellVisibility::BrowserCollapsed, None);
        assert_eq!(focus, super::ShellVisibility::Focus);
        assert_eq!(restore, Some(super::ShellVisibility::BrowserCollapsed));
        assert_eq!(
            super::toggle_focus_visibility(focus, restore).0,
            super::ShellVisibility::BrowserCollapsed
        );
        assert_eq!(
            super::toggle_browser_visibility(super::ShellVisibility::Focus),
            super::ShellVisibility::BrowserOnly
        );
    }

    #[test]
    fn red_search_focus_visibility_restores_a_navigation_shell() {
        let cases = [
            (
                super::ShellVisibility::Default,
                None,
                super::ShellVisibility::Default,
                None,
            ),
            (
                super::ShellVisibility::BrowserCollapsed,
                None,
                super::ShellVisibility::BrowserCollapsed,
                None,
            ),
            (
                super::ShellVisibility::Focus,
                Some(super::ShellVisibility::Default),
                super::ShellVisibility::Default,
                None,
            ),
            (
                super::ShellVisibility::Focus,
                Some(super::ShellVisibility::BrowserCollapsed),
                super::ShellVisibility::BrowserCollapsed,
                None,
            ),
            (
                super::ShellVisibility::Focus,
                None,
                super::ShellVisibility::Default,
                None,
            ),
            (
                super::ShellVisibility::BrowserOnly,
                Some(super::ShellVisibility::BrowserCollapsed),
                super::ShellVisibility::Default,
                None,
            ),
        ];
        for (current, restore, expected_visibility, expected_restore) in cases {
            assert_eq!(
                super::search_focus_visibility(current, restore),
                (expected_visibility, expected_restore),
                "current={current:?} restore={restore:?}"
            );
        }
    }

    #[test]
    fn search_match_ranges_cover_utf16_casefold_terms_and_attachments() {
        let ranges = super::search_match_ranges("😀复盘 \u{fffc}同步 复盘", "复盘 同步");
        assert_eq!(
            ranges,
            vec![NSRange::new(2, 2), NSRange::new(6, 2), NSRange::new(9, 2)]
        );

        let ascii = super::search_match_ranges("Alpha ALPHA alpha", "alpha");
        assert_eq!(
            ascii,
            vec![NSRange::new(0, 5), NSRange::new(6, 5), NSRange::new(12, 5)]
        );

        assert!(super::search_match_ranges("😀\u{fffc}图片", "\u{fffc}").is_empty());
        assert!(super::search_match_ranges("正文", "   ").is_empty());
        assert_eq!(
            super::search_match_ranges("aaaa", "aa"),
            vec![NSRange::new(0, 2), NSRange::new(2, 2)]
        );
    }

    #[test]
    fn search_highlight_uses_temporary_layout_attributes_only() {
        let storage = NSTextStorage::new();
        let layout = NSLayoutManager::new();
        let container = NSTextContainer::initWithContainerSize(
            NSTextContainer::alloc(),
            NSSize::new(640.0, 200.0),
        );
        storage.addLayoutManager(&layout);
        layout.addTextContainer(&container);
        let source = NSMutableAttributedString::from_nsstring(&NSString::from_str("Alpha"));
        storage.setAttributedString(&source);
        layout.ensureLayoutForTextContainer(&container);

        let ranges = super::apply_search_highlights(&layout, "Alpha", "alpha");
        assert_eq!(ranges, vec![NSRange::new(0, 5)]);
        let mut effective_range = NSRange::new(0, 0);
        assert!(
            unsafe {
                storage.attribute_atIndex_effectiveRange(
                    NSBackgroundColorAttributeName,
                    0,
                    &mut effective_range,
                )
            }
            .is_none()
        );
        assert!(
            unsafe {
                layout.temporaryAttribute_atCharacterIndex_effectiveRange(
                    NSBackgroundColorAttributeName,
                    0,
                    &mut effective_range,
                )
            }
            .is_some()
        );

        super::clear_search_highlights(&layout, storage.length());
        assert!(
            unsafe {
                layout.temporaryAttribute_atCharacterIndex_effectiveRange(
                    NSBackgroundColorAttributeName,
                    0,
                    &mut effective_range,
                )
            }
            .is_none()
        );
        let source: &NSAttributedString = &storage;
        let document = super::document_from_attributed_string(source).unwrap();
        assert_eq!(super::serialize_html(&document), "<p>Alpha</p>");
    }

    #[test]
    fn red_breadcrumb_and_title_share_the_writing_measure() {
        let layout = super::shell_layout(1380.0, 820.0, super::ShellVisibility::Default);
        assert!(layout.breadcrumb.width > 0.0);
        assert_eq!(layout.breadcrumb.x, layout.body.x);
        assert_eq!(layout.title.x, layout.body.x);
        assert!(layout.title.y < layout.breadcrumb.y);
    }

    #[test]
    fn red_autosave_coalesces_bursts_and_rejects_stale_tokens() {
        let mut state = super::AutosaveState::loaded("note-a", "标题", "<p>旧</p>");
        assert_eq!(state.mark_dirty("note-a", "标题", "<p>一</p>"), Some(1));
        assert_eq!(state.mark_dirty("note-a", "标题", "<p>二</p>"), Some(2));
        assert_eq!(
            state.timer_decision("note-a", 1),
            super::AutosaveDecision::Stale
        );
        assert_eq!(
            state.timer_decision("note-a", 2),
            super::AutosaveDecision::Persist {
                generation: 2,
                title: "标题".into(),
                html: "<p>二</p>".into()
            }
        );
        state.mark_saved(2);
        assert!(!state.is_dirty());
        assert_eq!(state.mark_dirty("note-b", "标题", "<p>二</p>"), None);
        assert_eq!(
            state.timer_decision("note-a", 2),
            super::AutosaveDecision::Stale
        );
    }

    #[test]
    fn autosave_state_handles_identical_html_title_only_failure_retry_and_flush() {
        let mut state = super::AutosaveState::loaded("note-a", "旧", "<p>正文</p>");
        assert_eq!(state.mark_dirty("note-a", "旧", "<p>正文</p>"), None);
        assert_eq!(state.mark_dirty("note-a", "新", "<p>正文</p>"), Some(1));
        assert_eq!(
            state.flush_decision("note-a"),
            super::AutosaveDecision::Persist {
                generation: 1,
                title: "新".into(),
                html: "<p>正文</p>".into()
            }
        );
        assert_eq!(state.mark_failed(1), Some(0.3));
        assert!(state.is_dirty());
        assert_eq!(state.retry_delay(1), Some(0.3));
        assert_eq!(
            state.timer_decision("note-a", 1),
            super::AutosaveDecision::Stale
        );
        state.mark_retry_scheduled(1);
        assert_eq!(
            state.timer_decision("note-a", 1),
            super::AutosaveDecision::Persist {
                generation: 1,
                title: "新".into(),
                html: "<p>正文</p>".into()
            }
        );
        state.mark_saved(1);
        assert!(!state.is_dirty());
        state.reset("note-b", "", "<p>B</p>");
        assert_eq!(
            state.timer_decision("note-a", 1),
            super::AutosaveDecision::Stale
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
    fn blank_title_uses_placeholder_and_keeps_body_in_summary() {
        assert_eq!(display_note_title("", "  正文首行\n第二行"), "无标题笔记");
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
        assert_eq!(note_list_title(&untitled), "无标题笔记");
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
    fn body_paste_never_reenters_text_view_paste_delegate() {
        assert_eq!(
            super::body_paste_dispatch(true),
            super::BodyPasteDispatch::DirectTextInsertion
        );
        assert_eq!(
            super::body_paste_dispatch(false),
            super::BodyPasteDispatch::ResponderPaste
        );
    }

    #[test]
    fn autosave_preview_refresh_preserves_editor_geometry() {
        assert!(!super::should_relayout_editor_after_preview_update(
            &PreviewListUpdate::ReloadIndices(vec![0]),
            false,
        ));
        assert!(super::should_relayout_editor_after_preview_update(
            &PreviewListUpdate::ReloadAll,
            false,
        ));
        assert!(!super::should_relayout_editor_after_preview_update(
            &PreviewListUpdate::ReloadAll,
            true,
        ));
    }

    #[test]
    fn projection_prefix_mapping_survives_sequential_text_edits() {
        let mut prefixes = vec![
            super::RenderedProjectionPrefix {
                projected_range: NSRange::new(0, 2),
                semantic_offset: 0,
                text: "• ".into(),
            },
            super::RenderedProjectionPrefix {
                projected_range: NSRange::new(6, 2),
                semantic_offset: 4,
                text: "• ".into(),
            },
        ];
        assert_eq!(
            super::semantic_text_from_projection("• one\n• two", &prefixes).as_deref(),
            Some("one\ntwo")
        );
        assert_eq!(
            super::semantic_range_from_projection(NSRange::new(5, 0), &prefixes),
            Some(NSRange::new(3, 0))
        );
        assert!(super::adjust_projection_prefixes_with_semantic(
            &mut prefixes,
            NSRange::new(5, 0),
            NSRange::new(3, 0),
            1,
        ));
        assert_eq!(prefixes[1].projected_range, NSRange::new(7, 2));
        assert_eq!(prefixes[1].semantic_offset, 5);
        assert_eq!(
            super::semantic_range_from_projection(NSRange::new(9, 0), &prefixes),
            Some(NSRange::new(5, 0))
        );
        assert_eq!(
            super::semantic_range_from_projection(NSRange::new(0, 12), &prefixes),
            Some(NSRange::new(0, 8))
        );
        assert_eq!(
            super::projection_range_from_semantic(NSRange::new(0, 8), &prefixes),
            Some(NSRange::new(0, 12))
        );
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
    fn red_failed_image_candidate_preserves_live_undo_and_redo_history() {
        let document = Document::from_blocks(vec![paragraph(vec![Inline::Text {
            text: "ab".into(),
            marks: Marks::default(),
        }])]);
        let mut live = super::session_from_document(&document).unwrap();
        super::apply_committed_text_delta(&mut live, NSRange::new(2, 0), "!").unwrap();
        live.undo().unwrap();
        assert!(live.can_redo());

        let before_document = super::document_from_session(&live).unwrap();
        let before_revision = live.revision();
        let before_can_undo = live.can_undo();
        let before_can_redo = live.can_redo();
        let before_redo_document = Document::from_blocks(vec![paragraph(vec![Inline::Text {
            text: "ab!".into(),
            marks: Marks::default(),
        }])]);

        let (_candidate, prepared, _candidate_selection) = super::prepare_image_insert_candidate(
            &live,
            NSRange::new(2, 0),
            "0123456789abcdef0123456789abcdef",
            "失败图片",
            "标题".into(),
        )
        .unwrap();
        assert!(
            prepared
                .update
                .body
                .contains("0123456789abcdef0123456789abcdef")
        );
        let persistence_succeeded = false;
        assert!(!persistence_succeeded);

        assert_eq!(
            super::document_from_session(&live).unwrap(),
            before_document
        );
        assert_eq!(live.revision(), before_revision);
        assert_eq!(live.can_undo(), before_can_undo);
        assert_eq!(live.can_redo(), before_can_redo);
        let mut redo = live;
        redo.redo().unwrap();
        assert_eq!(
            super::document_from_session(&redo).unwrap(),
            before_redo_document
        );
    }

    #[test]
    fn image_insert_success_adds_one_undoable_live_command() {
        let document = Document::from_blocks(vec![paragraph(vec![Inline::Text {
            text: "ab".into(),
            marks: Marks::default(),
        }])]);
        let mut live = super::session_from_document(&document).unwrap();
        let before_revision = live.revision();
        let (_candidate, prepared, candidate_selection) = super::prepare_image_insert_candidate(
            &live,
            NSRange::new(2, 0),
            "0123456789abcdef0123456789abcdef",
            "图片",
            "标题".into(),
        )
        .unwrap();
        let candidate_body = prepared.update.body.clone();
        let live_selection = super::insert_image_block_anchor(
            &mut live,
            NSRange::new(2, 0),
            "0123456789abcdef0123456789abcdef",
            "图片",
            1,
            1,
        )
        .unwrap();
        assert_eq!(live_selection, candidate_selection);
        assert_eq!(
            super::serialize_html(&super::document_from_session(&live).unwrap()),
            candidate_body
        );
        assert_eq!(live.revision(), before_revision + 1);
        let expected = Document::from_blocks(vec![
            paragraph(vec![Inline::Text {
                text: "ab".into(),
                marks: Marks::default(),
            }]),
            paragraph(vec![Inline::Image {
                resource_id: "0123456789abcdef0123456789abcdef".into(),
                alt: "图片".into(),
            }]),
            paragraph(vec![]),
        ]);
        assert_eq!(super::document_from_session(&live).unwrap(), expected);
        assert!(live.can_undo());
        live.undo().unwrap();
        assert_eq!(super::document_from_session(&live).unwrap(), document);
        assert!(live.can_redo());
        live.redo().unwrap();
        assert_eq!(super::document_from_session(&live).unwrap(), expected);
    }

    #[test]
    fn red_format_refresh_keeps_image_only_actions_titleless() {
        assert_eq!(
            super::toolbar_state_title(super::EditorAction::BlockStyle, "标题 1"),
            Some("标题 1")
        );
        assert_eq!(
            super::toolbar_state_title(super::EditorAction::Highlight, "高亮"),
            None
        );
        assert_eq!(
            super::toolbar_state_title(super::EditorAction::Strikethrough, "删除线"),
            None
        );
    }

    #[test]
    fn red_drag_image_uses_the_same_flush_and_ime_gate_as_open_image() {
        assert!(super::image_insert_gate(true, false, true));
        assert!(!super::image_insert_gate(true, true, true));
        assert!(!super::image_insert_gate(true, false, false));
        assert!(!super::image_insert_gate(false, false, true));
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

    #[test]
    fn red_resize_recomputes_more_enabled_for_the_production_layout() {
        let wide = super::toolbar_more_enabled_for_layout(
            1380.0,
            820.0,
            super::ShellVisibility::Default,
            true,
        );
        let narrow = super::toolbar_more_enabled_for_layout(
            1100.0,
            700.0,
            super::ShellVisibility::Default,
            true,
        );
        let focus = super::toolbar_more_enabled_for_layout(
            1380.0,
            820.0,
            super::ShellVisibility::Focus,
            true,
        );
        let no_note_wide = super::toolbar_more_enabled_for_layout(
            1380.0,
            820.0,
            super::ShellVisibility::Default,
            false,
        );
        let no_note_focus = super::toolbar_more_enabled_for_layout(
            1380.0,
            820.0,
            super::ShellVisibility::Focus,
            false,
        );
        assert!(
            super::toolbar_overflow_actions_for_width(
                super::shell_layout(1380.0, 820.0, super::ShellVisibility::Default)
                    .toolbar
                    .width,
            )
            .is_empty()
        );
        assert!(
            super::toolbar_overflow_actions_for_width(
                super::shell_layout(1380.0, 820.0, super::ShellVisibility::Focus)
                    .toolbar
                    .width,
            )
            .is_empty()
        );
        assert!(wide);
        assert!(narrow);
        assert!(focus);
        assert!(!no_note_wide);
        assert!(!no_note_focus);
        assert!(
            super::toolbar_actions_for_width(
                super::shell_layout(1100.0, 700.0, super::ShellVisibility::Default)
                    .toolbar
                    .width
            )
            .contains(&super::EditorAction::More)
        );
    }

    #[test]
    fn red_marked_selection_event_preserves_explicit_caret_override() {
        assert!(!super::should_sync_caret_context(false, false, true, false));
        assert!(!super::should_sync_caret_context(false, false, false, true));
        assert!(super::should_sync_caret_context(false, false, false, false));
    }

    #[test]
    fn red_action_presentation_is_shared_and_disables_semantic_noops() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "plain".into(),
                marks: Marks::default(),
            }],
        }]);
        let session = super::session_from_document(&document).unwrap();
        let selection = NSRange::new(0, 0);
        let more_strike = super::semantic_action_presentation(
            super::EditorAction::Strikethrough,
            true,
            Some(&session),
            selection,
        );
        let fixed_strike = super::semantic_action_presentation(
            super::EditorAction::Strikethrough,
            true,
            Some(&session),
            selection,
        );
        assert_eq!(more_strike, fixed_strike);
        assert!(
            !super::semantic_action_presentation(
                super::EditorAction::Link,
                true,
                Some(&session),
                selection,
            )
            .enabled
        );
        assert!(
            !super::semantic_action_presentation(
                super::EditorAction::DecreaseIndent,
                true,
                Some(&session),
                selection,
            )
            .enabled
        );
        assert!(
            !super::semantic_action_presentation(
                super::EditorAction::Clear,
                true,
                Some(&session),
                selection,
            )
            .enabled
        );
    }

    #[test]
    fn inline_format_presentation_requires_a_real_text_or_caret_target() {
        let image_session =
            super::session_from_document(&Document::from_blocks(vec![Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Image {
                    resource_id: "image-only".into(),
                    alt: "图".into(),
                }],
            }]))
            .unwrap();
        let soft_break_session =
            super::session_from_document(&Document::from_blocks(vec![Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Text {
                    text: "\u{2028}".into(),
                    marks: Marks::default(),
                }],
            }]))
            .unwrap();
        let empty_session =
            super::session_from_document(&Document::from_blocks(vec![Block::Heading {
                level: joplin_lite_native::html_body::HeadingLevel::Two,
                style: Default::default(),
                inlines: Vec::new(),
            }]))
            .unwrap();
        for action in [
            super::EditorAction::Bold,
            super::EditorAction::Italic,
            super::EditorAction::Underline,
            super::EditorAction::Highlight,
            super::EditorAction::Strikethrough,
        ] {
            assert!(
                !super::semantic_action_presentation(
                    action,
                    true,
                    Some(&image_session),
                    NSRange::new(0, 1),
                )
                .enabled
            );
            assert!(
                !super::semantic_action_presentation(
                    action,
                    true,
                    Some(&soft_break_session),
                    NSRange::new(0, 1),
                )
                .enabled
            );
            assert!(
                !super::semantic_action_presentation(
                    action,
                    true,
                    Some(&empty_session),
                    NSRange::new(0, 0),
                )
                .enabled
            );
        }
    }

    #[test]
    fn active_alignment_remains_enabled_and_is_marked_active() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "正文".into(),
                marks: Default::default(),
            }],
        }]);
        let session = super::session_from_document(&document).unwrap();
        let presentation = super::semantic_action_presentation(
            super::EditorAction::AlignLeft,
            true,
            Some(&session),
            NSRange::new(0, 0),
        );
        assert_eq!(presentation.state, super::SelectionState::Active);
        assert!(presentation.enabled);
    }

    #[test]
    fn command_selection_is_rejected_for_non_body_focus_or_stale_note() {
        assert!(!super::command_selection_is_available(None, None, false));
        assert!(!super::command_selection_is_available(
            Some("current"),
            Some("previous"),
            false
        ));
        assert!(!super::command_selection_is_available(
            Some("current"),
            Some("current"),
            true
        ));
        assert!(super::command_selection_is_available(
            Some("current"),
            Some("current"),
            false
        ));
    }

    #[test]
    fn red_pending_edit_selection_event_commits_then_syncs_the_real_caret() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "x".into(),
                marks: Marks::default(),
            }],
        }]);
        let mut session = super::session_from_document(&document).unwrap();
        super::apply_inline_command(&mut session, NSRange::new(0, 0), super::InlineCommand::Bold)
            .unwrap();
        assert!(!super::should_sync_caret_context_after_delegate(
            false, false, false, true, false
        ));
        super::apply_committed_text_delta(&mut session, NSRange::new(0, 0), "A").unwrap();
        super::sync_caret_after_editor_event(
            &mut session,
            super::EditorSessionSyncResult::Applied,
            NSRange::new(1, 0),
            false,
            false,
        );
        assert_eq!(
            super::query_inline_state(&session, NSRange::new(1, 0), super::InlineCommand::Bold,)
                .unwrap(),
            super::SelectionState::Active
        );
    }

    #[test]
    fn red_action_presentation_requires_a_live_session_and_enabled_more_child() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "plain".into(),
                marks: Marks::default(),
            }],
        }]);
        let session = super::session_from_document(&document).unwrap();
        let selection = NSRange::new(0, 0);
        assert!(
            !super::semantic_action_presentation(
                super::EditorAction::InsertImage,
                true,
                None,
                selection,
            )
            .enabled
        );
        assert!(!super::more_has_enabled_child(
            &[super::EditorAction::Link],
            true,
            Some(&session),
            selection,
        ));
        assert!(super::more_has_enabled_child(
            &[super::EditorAction::Link],
            true,
            Some(&session),
            NSRange::new(0, 5),
        ));
        assert!(
            !super::semantic_action_presentation(super::EditorAction::More, true, None, selection,)
                .enabled
        );
    }

    #[test]
    fn red_every_catalogue_action_requires_a_live_session() {
        let selection = NSRange::new(0, 0);
        for descriptor in super::editor_action_catalogue() {
            let presentation =
                super::semantic_action_presentation(descriptor.action, true, None, selection);
            assert!(
                !presentation.enabled,
                "{} must be disabled without a live semantic session",
                descriptor.label
            );
        }
    }

    #[test]
    fn red_editor_commit_refreshes_post_commit_semantic_presentation() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "x".into(),
                marks: Default::default(),
            }],
        }]);
        let mut session = super::session_from_document(&document).unwrap();
        super::apply_inline_command(&mut session, NSRange::new(0, 0), super::InlineCommand::Bold)
            .unwrap();
        super::apply_committed_text_delta(&mut session, NSRange::new(0, 0), "A").unwrap();
        super::sync_caret_after_editor_event(
            &mut session,
            super::EditorSessionSyncResult::Applied,
            NSRange::new(1, 0),
            false,
            false,
        );
        let presentation = super::semantic_action_presentation(
            super::EditorAction::Bold,
            true,
            Some(&session),
            NSRange::new(1, 0),
        );
        assert_eq!(presentation.state, super::SelectionState::Active);
        assert!(presentation.enabled);
    }

    #[test]
    fn red_link_presentation_uses_real_text_applicability_and_identity() {
        let linked = |url: Option<&str>| Marks {
            link: url.map(str::to_owned),
            ..Default::default()
        };
        let plain = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "plain".into(),
                marks: linked(None),
            }],
        }]);
        let same = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "plain".into(),
                marks: linked(Some("https://example.com")),
            }],
        }]);
        let mixed = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![
                Inline::Text {
                    text: "plain".into(),
                    marks: linked(Some("https://example.com")),
                },
                Inline::Text {
                    text: "other".into(),
                    marks: linked(None),
                },
            ],
        }]);
        let different = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![
                Inline::Text {
                    text: "one".into(),
                    marks: linked(Some("https://example.com/a")),
                },
                Inline::Text {
                    text: "two".into(),
                    marks: linked(Some("https://example.com/b")),
                },
            ],
        }]);
        let separated = Document::from_blocks(vec![
            Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Text {
                    text: "first".into(),
                    marks: linked(Some("https://example.com")),
                }],
            },
            Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Text {
                    text: "second".into(),
                    marks: linked(None),
                }],
            },
        ]);
        let image = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Image {
                resource_id: "resource".into(),
                alt: "image".into(),
            }],
        }]);
        let plain = super::session_from_document(&plain).unwrap();
        let same = super::session_from_document(&same).unwrap();
        let mixed = super::session_from_document(&mixed).unwrap();
        let different = super::session_from_document(&different).unwrap();
        let separated = super::session_from_document(&separated).unwrap();
        let image = super::session_from_document(&image).unwrap();
        let range = NSRange::new(0, 5);
        assert_eq!(
            super::query_link_selection(&plain, range).unwrap(),
            super::LinkSelectionState {
                state: super::SelectionState::Inactive,
                has_linkable_text: true,
            }
        );
        assert_eq!(
            super::query_link_selection(&same, range).unwrap().state,
            super::SelectionState::Active
        );
        assert_eq!(
            super::query_link_selection(&mixed, NSRange::new(0, 10))
                .unwrap()
                .state,
            super::SelectionState::Mixed
        );
        assert_eq!(
            super::query_link_selection(&different, NSRange::new(0, 6))
                .unwrap()
                .state,
            super::SelectionState::Mixed
        );
        assert_eq!(
            super::query_link_selection(&separated, NSRange::new(0, 12))
                .unwrap()
                .state,
            super::SelectionState::Mixed
        );
        assert!(
            !super::query_link_selection(&image, NSRange::new(0, 1))
                .unwrap()
                .has_linkable_text
        );
        assert!(super::query_link_selection(&plain, NSRange::new(99, 1)).is_err());
    }

    #[test]
    fn red_marked_timer_defers_once_and_resumes_after_noop_or_new_generation() {
        let mut state = super::AutosaveState::loaded("note-a", "标题", "<p>旧</p>");
        assert_eq!(state.mark_dirty("note-a", "标题", "<p>A</p>"), Some(1));
        let epoch = state.epoch;
        assert!(state.defer_timer_with_epoch("note-a", epoch, 1));
        assert!(!state.defer_timer_with_epoch("note-a", epoch, 1));
        assert_eq!(state.scheduled_generation, None);
        assert_eq!(state.deferred_generation, Some(1));
        assert_eq!(
            state.resume_deferred_timer_with_epoch("note-a", epoch, 1),
            Some(1)
        );
        assert_eq!(
            state.resume_deferred_timer_with_epoch("note-a", epoch, 1),
            None
        );
        assert_eq!(state.scheduled_generation, Some(1));
        state.defer_timer_with_epoch("note-a", epoch, 1);
        assert_eq!(state.mark_dirty("note-a", "标题", "<p>B</p>"), Some(2));
        assert_eq!(state.deferred_generation, None);
        assert_eq!(state.scheduled_generation, Some(2));
        state.reset("note-b", "标题", "<p>B</p>");
        assert!(!state.defer_timer_with_epoch("note-a", epoch, 1));
        assert_eq!(
            state.resume_deferred_timer_with_epoch("note-a", epoch, 1),
            None
        );
        assert!(super::autosave_timer_defer_allowed(true, false));
        assert!(!super::autosave_timer_defer_allowed(true, true));
    }
}
