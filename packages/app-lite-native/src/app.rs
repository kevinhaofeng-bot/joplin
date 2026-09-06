use joplin_lite_native::body::{
    extract_resource_ids, markdown_marker, marker_spans, project_search_text,
};
use joplin_lite_native::core::{
    CreateNote, Note, NoteContentUpdate, NoteRepository, ResourceImport,
};
use joplin_lite_native::resource_store::MAX_IMAGE_BYTES;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
#[allow(deprecated)]
use objc2_app_kit::NSObliquenessAttributeName;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAttachmentAttributeName,
    NSAttributedStringAppKitDocumentFormats, NSAttributedStringAttachmentConveniences,
    NSBackgroundColorAttributeName, NSBackingStoreType, NSBaselineOffsetAttributeName,
    NSBezelStyle, NSBitmapImageFileType, NSBitmapImageRep, NSBorderType, NSBox, NSBoxType,
    NSButton, NSButtonType, NSColor, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSEventModifierFlags, NSFont, NSFontAttributeName,
    NSFontTraitMask, NSForegroundColorAttributeName, NSImage, NSKernAttributeName,
    NSLayoutAttribute, NSLineBreakMode, NSMenu, NSMenuItem,
    NSMutableAttributedStringAppKitAdditions, NSMutableParagraphStyle, NSPasteboard,
    NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeTIFF, NSResponder, NSScrollView,
    NSSearchField, NSShadowAttributeName, NSStackView, NSStackViewDistribution,
    NSStrikethroughStyleAttributeName, NSStrokeColorAttributeName, NSStrokeWidthAttributeName,
    NSTextAlignment, NSTextAttachment, NSTextDelegate, NSTextField, NSTextFieldDelegate,
    NSTextView, NSTextViewDelegate, NSUnderlineStyle, NSUnderlineStyleAttributeName,
    NSUserInterfaceLayoutOrientation, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSData, NSDictionary,
    NSMutableAttributedString, NSMutableCopying, NSNotification, NSNumber, NSObject,
    NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSURL, ns_string,
};
use std::cell::{OnceCell, RefCell};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Arc;

const RESOURCE_ID_ATTRIBUTE: &str = "com.kevinhao.joplin-lite.resource-id";
const RESOURCE_ALT_ATTRIBUTE: &str = "com.kevinhao.joplin-lite.resource-alt";

#[derive(Debug, Clone, PartialEq, Eq)]
struct AttachmentDescriptor {
    resource_id: String,
    alt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditorSegment {
    Text(String),
    Attachment(AttachmentDescriptor),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorProjection {
    body: String,
    body_text: String,
    resource_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
enum EditorCodecError {
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

fn editor_save_projection(
    segments: &[EditorSegment],
) -> Result<EditorProjection, EditorCodecError> {
    let mut body = String::new();
    for segment in segments {
        match segment {
            EditorSegment::Text(text) => body.push_str(&text.replace('\u{fffc}', "[图片]")),
            EditorSegment::Attachment(attachment) => {
                body.push_str(&markdown_marker(&attachment.resource_id, &attachment.alt)?);
            }
        }
    }
    let resource_ids = extract_resource_ids(&body);
    Ok(EditorProjection {
        body_text: project_search_text(&body),
        body,
        resource_ids,
    })
}

fn resource_id_attribute_key() -> Retained<NSAttributedStringKey> {
    NSString::from_str(RESOURCE_ID_ATTRIBUTE)
}

fn resource_alt_attribute_key() -> Retained<NSAttributedStringKey> {
    NSString::from_str(RESOURCE_ALT_ATTRIBUTE)
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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum RtfLoadDecision {
    ParsedRtf,
    PlainBodyFallback,
}

fn rtf_load_decision(parse_succeeded: bool) -> RtfLoadDecision {
    if parse_succeeded {
        RtfLoadDecision::ParsedRtf
    } else {
        RtfLoadDecision::PlainBodyFallback
    }
}

fn rtf_text_matches_body(expanded_text: Option<&str>, canonical_body: &str) -> bool {
    expanded_text == Some(canonical_body)
}

#[derive(Debug, PartialEq, Eq)]
enum RtfSavePlan {
    Rich(Vec<u8>),
    PlainTextFallback,
}

fn rtf_save_plan(payload: Option<Vec<u8>>) -> RtfSavePlan {
    match payload {
        Some(payload) => RtfSavePlan::Rich(payload),
        None => RtfSavePlan::PlainTextFallback,
    }
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

fn editor_segments_with_ranges(
    source: &NSAttributedString,
) -> (Vec<EditorSegment>, Vec<(NSRange, String)>) {
    let length = source.string().length();
    let mut location = 0;
    let mut segments = Vec::new();
    let mut replacement_ranges = Vec::new();
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
        let text = string_for_range(source, effective_range);
        let resource_id = attribute_string(&attributes, &id_key);
        let alt = attribute_string(&attributes, &alt_key).unwrap_or_else(|| "图片".into());
        let has_attachment = unsafe { attributes.objectForKey_unchecked(attachment_key) }.is_some();
        if let Some(resource_id) = resource_id.filter(|_| text == "\u{fffc}") {
            if let Ok(marker) = markdown_marker(&resource_id, &alt) {
                segments.push(EditorSegment::Attachment(AttachmentDescriptor {
                    resource_id,
                    alt,
                }));
                replacement_ranges.push((effective_range, marker));
            } else {
                segments.push(EditorSegment::Text("[图片]".into()));
                replacement_ranges.push((effective_range, "[图片]".into()));
            }
        } else if has_attachment || text.contains('\u{fffc}') {
            let text = text.replace('\u{fffc}', "[图片]");
            segments.push(EditorSegment::Text(text.clone()));
            replacement_ranges.push((effective_range, text));
        } else {
            segments.push(EditorSegment::Text(text));
        }
        let next = effective_range
            .location
            .saturating_add(effective_range.length);
        if next <= location {
            break;
        }
        location = next;
    }
    (segments, replacement_ranges)
}

fn sanitized_rtf_from_editor(
    source: &NSAttributedString,
    replacement_ranges: &[(NSRange, String)],
    canonical_body: &str,
) -> Option<Vec<u8>> {
    let mutable = source.mutableCopy();
    for (range, replacement) in replacement_ranges.iter().rev() {
        mutable.replaceCharactersInRange_withString(*range, &NSString::from_str(replacement));
    }
    let full_range = NSRange::new(0, mutable.string().length());
    let attachment_key = unsafe { NSAttachmentAttributeName };
    mutable.removeAttribute_range(attachment_key, full_range);
    mutable.removeAttribute_range(&resource_id_attribute_key(), full_range);
    mutable.removeAttribute_range(&resource_alt_attribute_key(), full_range);
    let empty_keys: [&NSString; 0] = [];
    let empty_values: [&AnyObject; 0] = [];
    let document_attributes =
        NSDictionary::<NSString, AnyObject>::from_slices(&empty_keys, &empty_values);
    let immutable: &NSAttributedString = &mutable;
    let data = unsafe {
        immutable.RTFFromRange_documentAttributes(
            NSRange::new(0, immutable.string().length()),
            &document_attributes,
        )?
    };
    let parsed = unsafe {
        NSAttributedString::initWithRTF_documentAttributes(
            NSAttributedString::alloc(),
            &data,
            None,
        )?
    };
    if parsed.string().to_string() != canonical_body || attributed_string_has_attachments(&parsed) {
        return None;
    }
    Some(data.to_vec())
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

fn attributed_string_has_attachments(source: &NSAttributedString) -> bool {
    let length = source.string().length();
    let attachment_key = unsafe { NSAttachmentAttributeName };
    let mut location = 0;
    while location < length {
        let mut effective_range = NSRange::new(location, 0);
        let attributes = unsafe {
            source.attributesAtIndex_longestEffectiveRange_inRange(
                location,
                &mut effective_range,
                NSRange::new(0, length),
            )
        };
        if unsafe { attributes.objectForKey_unchecked(attachment_key) }.is_some() {
            return true;
        }
        let next = effective_range
            .location
            .saturating_add(effective_range.length);
        if next <= location {
            break;
        }
        location = next;
    }
    false
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

struct AppDelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    repository: Arc<NoteRepository>,
    current_note_id: RefCell<Option<String>>,
    notes: RefCell<Vec<Note>>,
    loading_guard: RefCell<bool>,
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
        fn application_should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool { true }

        #[unsafe(method(applicationSupportsSecureRestorableState:))]
        fn application_supports_secure_restorable_state(&self, _app: &NSApplication) -> bool { true }

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
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
            );
            sidebar_background.setBoxType(NSBoxType::Custom);
            sidebar_background.setTransparent(false);
            sidebar_background.setFillColor(&NSColor::underPageBackgroundColor());
            sidebar_background.setBorderWidth(0.0);
            content.addSubview(&sidebar_background);

            let separator = NSBox::initWithFrame(
                NSBox::alloc(mtm),
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
            );
            separator.setBoxType(NSBoxType::Separator);
            content.addSubview(&separator);

            let list_scroll = NSScrollView::initWithFrame(
                NSScrollView::alloc(mtm),
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
            );
            list_scroll.setHasVerticalScroller(true);
            list_scroll.setAutohidesScrollers(true);
            list_scroll.setBorderType(NSBorderType::NoBorder);
            list_scroll.setDrawsBackground(false);
            let list_stack = NSStackView::initWithFrame(
                NSStackView::alloc(mtm),
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
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
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
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
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
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
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
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

            let body = NSTextView::initWithFrame(
                NSTextView::alloc(mtm),
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
            );
            body.setEditable(true);
            body.setRichText(true);
            body.setAllowsUndo(true);
            body.setImportsGraphics(true);
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
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
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
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
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
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
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
            self.ivars().sidebar_background.set(sidebar_background).unwrap();
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
            self.ivars().body_view.set(body.clone()).unwrap();
            self.ivars().delete_button.set(delete_button).unwrap();
            self.ivars().save_status.set(save_status).unwrap();
            self.ivars().editor_empty_label.set(editor_empty_label).unwrap();

            unsafe { title_field.setDelegate(Some(ProtocolObject::from_ref(self))); }
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
            if let Some(query) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_SEARCH") {
                self.search_notes(&query.to_string_lossy());
                println!("searchNotes: result count={}", self.ivars().notes.borrow().len());
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
        fn text_did_change(&self, _notification: &NSNotification) {
            self.save_current_note();
        }
    }
    unsafe impl NSTextFieldDelegate for AppDelegate {}
    unsafe impl NSTextViewDelegate for AppDelegate {
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
            if let Some(manager) = self.ivars().body_view.get().and_then(|body| body.undoManager())
                && manager.canUndo()
            {
                manager.undo();
            }
        }

        #[unsafe(method(redoText:))]
        fn redo_text(&self, _sender: &NSObject) {
            if let Some(manager) = self.ivars().body_view.get().and_then(|body| body.undoManager())
                && manager.canRedo()
            {
                manager.redo();
            }
        }

        #[unsafe(method(newNote:))]
        fn new_note(&self, _sender: &NSObject) {
            let note = self.ivars().repository.create_note(CreateNote {
                title: String::new(),
                body: String::new(),
                body_rtf: Vec::new(),
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
        let range = body.selectedRange();
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

    fn read_pasteboard_image(&self) -> PasteboardImage {
        let pasteboard = NSPasteboard::generalPasteboard();
        let png_type = unsafe { NSPasteboardTypePNG };
        let tiff_type = unsafe { NSPasteboardTypeTIFF };
        let file_url_type = unsafe { NSPasteboardTypeFileURL };
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
        if pasteboard.types().as_ref().is_some_and(|types| {
            types.iter().any(|item| {
                let item: &NSString = item.as_ref();
                item == file_url_type
            })
        }) {
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
            if !url.isFileURL() {
                return PasteboardImage::Rejected("图片未插入：格式不支持");
            }
            let Some(path) = url.path() else {
                return PasteboardImage::Rejected("图片未插入：格式不支持");
            };
            let path = std::path::PathBuf::from(path.to_string());
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase);
            let mime = match extension.as_deref() {
                Some("png") => ("image/png", "png"),
                Some("jpg") | Some("jpeg") => ("image/jpeg", "jpg"),
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
                Err(PasteFileError::Invalid) => {
                    return PasteboardImage::Rejected("图片未插入：格式不支持");
                }
            };
            if !valid_image_bytes(&bytes) {
                return PasteboardImage::Rejected("图片未插入：格式不支持");
            }
            return PasteboardImage::Data {
                bytes,
                title,
                mime: mime.0.to_owned(),
            };
        }
        PasteboardImage::NotImage
    }

    fn insert_image_data(&self, bytes: &[u8], title: &str, mime: &str) -> bool {
        if bytes.len() > MAX_IMAGE_BYTES {
            self.set_save_status("图片未插入：超过 10 MB", true);
            return false;
        }
        if !matches!(mime, "image/png" | "image/jpeg") || !valid_image_bytes(bytes) {
            self.set_save_status("图片未插入：格式不支持", true);
            return false;
        }
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
        let Some(body) = self.ivars().body_view.get() else {
            return false;
        };
        let Some(inline) = inline_attachment(&stored) else {
            self.set_save_status("图片未插入：格式不支持", true);
            return false;
        };
        insert_inline_attachment(body, &inline);
        self.save_current_note();
        true
    }

    fn load_note(&self, note: &Note) {
        *self.ivars().loading_guard.borrow_mut() = true;
        *self.ivars().current_note_id.borrow_mut() = Some(note.id.clone());
        self.ivars()
            .title_field
            .get()
            .unwrap()
            .setStringValue(&NSString::from_str(&note.title));
        let body = self.ivars().body_view.get().unwrap();
        let loaded_rtf = if note.body_rtf.is_empty() {
            body.setString(&NSString::from_str(&note.body));
            None
        } else {
            let rtf = NSData::with_bytes(&note.body_rtf);
            let parsed = unsafe {
                NSAttributedString::initWithRTF_documentAttributes(
                    NSAttributedString::alloc(),
                    &rtf,
                    None,
                )
            };
            if let Some(parsed) = parsed.filter(|parsed| {
                let expanded = parsed.string().to_string();
                rtf_text_matches_body(Some(&expanded), &note.body)
                    && !attributed_string_has_attachments(parsed)
            }) {
                if let Some(storage) = unsafe { body.textStorage() } {
                    storage.setAttributedString(&parsed);
                    Some(true)
                } else {
                    body.setString(&NSString::from_str(&note.body));
                    Some(false)
                }
            } else {
                body.setString(&NSString::from_str(&note.body));
                Some(false)
            }
        };
        let rtf_failed = loaded_rtf
            .map(rtf_load_decision)
            .map(|decision| decision == RtfLoadDecision::PlainBodyFallback)
            .unwrap_or(false);
        self.render_body_attachments(note);
        body.setSelectedRange(NSRange::new(0, 0));
        *self.ivars().loading_guard.borrow_mut() = false;
        if rtf_failed {
            self.set_save_status("格式恢复失败，已回退正文", true);
        } else {
            self.set_save_status("已保存", false);
        }
        self.update_editor_visibility();
        self.update_note_selection();
        self.update_formatting_buttons();
    }

    fn render_body_attachments(&self, note: &Note) {
        let Some(body) = self.ivars().body_view.get() else {
            return;
        };
        let Some(storage) = (unsafe { body.textStorage() }) else {
            return;
        };
        let mut replacements = Vec::new();
        for (range, resource_id, alt) in canonical_marker_ranges(&note.body) {
            let Ok(resource) = self.ivars().repository.get_resource(&resource_id) else {
                continue;
            };
            let Some(resource) = resource else {
                continue;
            };
            let Some(inline) = inline_attachment_with_alt(&resource, &alt) else {
                continue;
            };
            replacements.push((range, inline));
        }
        for (range, inline) in replacements.into_iter().rev() {
            storage.replaceCharactersInRange_withAttributedString(range, inline.as_ref());
        }
    }

    fn clear_current_note(&self) {
        *self.ivars().loading_guard.borrow_mut() = true;
        *self.ivars().current_note_id.borrow_mut() = None;
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

    fn save_current_note(&self) {
        if *self.ivars().loading_guard.borrow() {
            return;
        }
        let Some(id) = self.ivars().current_note_id.borrow().clone() else {
            return;
        };
        let title = self
            .ivars()
            .title_field
            .get()
            .map(|field| field.stringValue().to_string())
            .unwrap_or_default();
        let body_view = self.ivars().body_view.get().unwrap();
        let (segments, attachment_ranges) = (unsafe { body_view.textStorage() })
            .map(|storage| {
                let source: &NSAttributedString = &storage;
                editor_segments_with_ranges(source)
            })
            .unwrap_or_else(|| {
                (
                    vec![EditorSegment::Text(body_view.string().to_string())],
                    Vec::new(),
                )
            });
        let projection = match editor_save_projection(&segments) {
            Ok(projection) => projection,
            Err(error) => {
                eprintln!("editor projection failed: {error}");
                self.set_save_status("保存失败", true);
                return;
            }
        };
        let body = projection.body.clone();
        let save_plan = rtf_save_plan((unsafe { body_view.textStorage() }).and_then(|storage| {
            let source: &NSAttributedString = &storage;
            sanitized_rtf_from_editor(source, &attachment_ranges, &projection.body)
        }));
        let formatting_fallback = matches!(save_plan, RtfSavePlan::PlainTextFallback);
        let rtf = match save_plan {
            RtfSavePlan::Rich(rtf) => rtf,
            // Clearing the stored RTF is intentional: keeping stale rich text
            // would make a restart restore an older body than the plain text.
            RtfSavePlan::PlainTextFallback => Vec::new(),
        };
        match self.ivars().repository.update_note_content(
            &id,
            NoteContentUpdate {
                title,
                body,
                body_text: projection.body_text,
                body_rtf: rtf,
                resource_ids: projection.resource_ids,
            },
        ) {
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
                if formatting_fallback {
                    self.set_save_status("正文已保存，格式未保存", true);
                } else {
                    self.set_save_status("已保存", false);
                }
            }
            Err(error) => {
                eprintln!("autosave failed: {error}");
                self.set_save_status("保存失败", true);
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
    } else if valid_image_bytes(&bytes) {
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
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return false;
    }
    let data = NSData::with_bytes(bytes);
    NSImage::initWithData(NSImage::alloc(), &data).is_some()
}

fn inline_attachment(
    resource: &joplin_lite_native::core::StoredResource,
) -> Option<Retained<NSMutableAttributedString>> {
    inline_attachment_with_alt(resource, &resource.title)
}

#[allow(deprecated)]
fn insert_inline_attachment(body: &NSTextView, inline: &NSMutableAttributedString) {
    unsafe { body.insertText(inline as &AnyObject) };
}

fn inline_attachment_with_alt(
    resource: &joplin_lite_native::core::StoredResource,
    alt: &str,
) -> Option<Retained<NSMutableAttributedString>> {
    let data = NSData::with_bytes(&resource.bytes);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    let uti = NSString::from_str(if resource.mime == "image/jpeg" {
        "public.jpeg"
    } else {
        "public.png"
    });
    let attachment =
        NSTextAttachment::initWithData_ofType(NSTextAttachment::alloc(), Some(&data), Some(&uti));
    attachment.setImage(Some(&image));
    let size = image.size();
    let scale = if size.width > 640.0 || size.height > 640.0 {
        (640.0 / size.width.max(size.height)).min(1.0)
    } else {
        1.0
    };
    attachment.setBounds(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(size.width * scale, size.height * scale),
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
    let title = display_note_title(&note.title, &note.body);
    let summary = note
        .body
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(52).collect::<String>())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "暂无正文".to_string());
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
        AttachmentDescriptor, ContentLayout, DataDirError, DataFileError, EditorSegment,
        FontTraitOperation, FormatDecision, FormatTarget, PasteFileError, PasteRoute,
        RtfLoadDecision, RtfSavePlan, TextFormat, attributed_string_has_attachments,
        choose_data_dir, content_layout, display_note_title, editor_save_projection,
        editor_segments_with_ranges, ensure_notes_database_file, format_decision, format_target,
        paste_route, read_regular_image_file, rtf_load_decision, rtf_save_plan,
        rtf_text_matches_body, sanitized_rtf_from_editor, typing_trait_operation,
        validate_canonical_data_dir,
    };
    use objc2::AnyThread;
    use objc2_app_kit::{
        NSAttributedStringAppKitDocumentFormats, NSAttributedStringAttachmentConveniences,
        NSTextAttachment,
    };
    use objc2_foundation::{
        NSAttributedString, NSData, NSMutableAttributedString, NSRange, NSString,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    #[test]
    fn editor_projection_preserves_attachment_order_without_binary_rtf() {
        let projection = editor_save_projection(&[
            EditorSegment::Text("前文\n".into()),
            EditorSegment::Attachment(AttachmentDescriptor {
                resource_id: "0123456789abcdef0123456789abcdef".into(),
                alt: "截图.png".into(),
            }),
            EditorSegment::Text("\n后文".into()),
        ])
        .unwrap();
        assert_eq!(
            projection.body,
            "前文\n![截图.png](:/0123456789abcdef0123456789abcdef)\n后文"
        );
        assert_eq!(
            projection.resource_ids,
            vec!["0123456789abcdef0123456789abcdef"]
        );
        assert!(projection.body_text.contains("截图.png"));
        assert!(!projection.body.contains('\u{fffc}'));
    }

    #[test]
    fn editor_projection_rejects_invalid_resource_ids() {
        let error = editor_save_projection(&[EditorSegment::Attachment(AttachmentDescriptor {
            resource_id: "not-a-resource".into(),
            alt: "图片".into(),
        })])
        .unwrap_err();
        assert!(matches!(error, super::EditorCodecError::Body(_)));
    }

    #[test]
    fn editor_projection_keeps_missing_marker_text_and_association() {
        let body = "前文\n![损坏图](:/0123456789abcdef0123456789abcdef)\n后文";
        let projection = editor_save_projection(&[EditorSegment::Text(body.into())]).unwrap();
        assert_eq!(projection.body, body);
        assert_eq!(
            projection.resource_ids,
            vec!["0123456789abcdef0123456789abcdef"]
        );
        assert!(projection.body_text.contains("损坏图"));
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
    fn blank_title_uses_body_first_line_only_for_list_display() {
        assert_eq!(display_note_title("", "  正文首行\n第二行"), "正文首行");
        assert_eq!(
            display_note_title("我的真实标题", "正文首行"),
            "我的真实标题"
        );
        assert_eq!(display_note_title("", ""), "无标题笔记");
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
    fn rtf_cache_requires_exact_expanded_body_text() {
        assert!(rtf_text_matches_body(Some("前文😀"), "前文😀"));
        assert!(!rtf_text_matches_body(Some("前文"), "前文😀"));
        assert!(!rtf_text_matches_body(None, "前文"));
    }

    #[test]
    fn sanitizer_downgrades_known_and_unknown_attachments_without_rtf_payload() {
        let id = "0123456789abcdef0123456789abcdef";
        let bytes = NSData::with_bytes(b"not-a-real-image");
        let known_attachment = NSTextAttachment::initWithData_ofType(
            NSTextAttachment::alloc(),
            Some(&bytes),
            Some(&NSString::from_str("public.png")),
        );
        let unknown_attachment = NSTextAttachment::initWithData_ofType(
            NSTextAttachment::alloc(),
            Some(&bytes),
            Some(&NSString::from_str("public.png")),
        );
        let known = NSAttributedString::attributedStringWithAttachment(&known_attachment);
        let unknown = NSAttributedString::attributedStringWithAttachment(&unknown_attachment);
        let source = NSMutableAttributedString::from_nsstring(&NSString::from_str("前"));
        source.appendAttributedString(&known);
        let id_key = super::resource_id_attribute_key();
        let id_value = NSString::from_str(id);
        unsafe {
            source.addAttribute_value_range(&id_key, &id_value, NSRange::new(1, 1));
        }
        source.appendAttributedString(&unknown);
        source.appendAttributedString(&NSAttributedString::initWithString(
            NSAttributedString::alloc(),
            &NSString::from_str("后"),
        ));
        let source_ref: &NSAttributedString = &source;
        let (segments, replacements) = editor_segments_with_ranges(source_ref);
        let projection = editor_save_projection(&segments).unwrap();
        assert_eq!(projection.body, format!("前![图片](:/{id})[图片]后"));
        let rtf = sanitized_rtf_from_editor(source_ref, &replacements, &projection.body).unwrap();
        let parsed = unsafe {
            NSAttributedString::initWithRTF_documentAttributes(
                NSAttributedString::alloc(),
                &NSData::with_bytes(&rtf),
                None,
            )
        }
        .unwrap();
        assert_eq!(parsed.string().to_string(), projection.body);
        assert!(!attributed_string_has_attachments(&parsed));
        let rtf_text = String::from_utf8_lossy(&rtf);
        assert!(!rtf_text.contains("\\pict"));
        assert!(!rtf_text.contains("pngblip"));
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
    fn bad_rtf_chooses_plain_body_without_replacing_saved_rtf() {
        assert_eq!(rtf_load_decision(false), RtfLoadDecision::PlainBodyFallback);
        assert_eq!(rtf_load_decision(true), RtfLoadDecision::ParsedRtf);
    }

    #[test]
    fn missing_rtf_export_chooses_plain_text_without_stale_rich_text() {
        assert_eq!(rtf_save_plan(None), RtfSavePlan::PlainTextFallback);
        assert_eq!(
            rtf_save_plan(Some(vec![1, 2])),
            RtfSavePlan::Rich(vec![1, 2])
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
