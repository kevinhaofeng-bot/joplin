use joplin_lite_native::core::{CreateNote, Note, NoteRepository};
use objc2::rc::Retained;
use objc2::runtime::{ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
#[allow(deprecated)]
use objc2_app_kit::NSObliquenessAttributeName;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSAttributedStringAppKitDocumentFormats, NSBackgroundColorAttributeName, NSBackingStoreType,
    NSBaselineOffsetAttributeName, NSBezelStyle, NSBorderType, NSBox, NSBoxType, NSButton,
    NSButtonType, NSColor, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSEventModifierFlags, NSFont, NSFontAttributeName,
    NSFontTraitMask, NSForegroundColorAttributeName, NSKernAttributeName, NSLayoutAttribute,
    NSLineBreakMode, NSMenu, NSMenuItem, NSMutableAttributedStringAppKitAdditions,
    NSMutableParagraphStyle, NSScrollView, NSSearchField, NSShadowAttributeName, NSStackView,
    NSStrikethroughStyleAttributeName, NSStrokeColorAttributeName, NSStrokeWidthAttributeName,
    NSTextAlignment, NSTextDelegate, NSTextField, NSTextFieldDelegate, NSTextView,
    NSTextViewDelegate, NSUnderlineStyle, NSUnderlineStyleAttributeName,
    NSUserInterfaceLayoutOrientation, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSData, NSMutableAttributedString, NSMutableCopying,
    NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize,
    NSString, ns_string,
};
use std::cell::{OnceCell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Arc;

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
    let editor_width = (right_width - margin * 2.0).max(400.0);
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
        x: sidebar_width + margin,
        y: height - 84.0,
        width: (editor_width - 84.0).max(260.0),
        height: 42.0,
    };
    let toolbar = LayoutRect {
        x: sidebar_width + margin,
        y: toolbar_y,
        width: editor_width,
        height: 30.0,
    };
    let body = LayoutRect {
        x: sidebar_width + margin,
        y: body_y,
        width: editor_width,
        height: body_height,
    };
    let delete = LayoutRect {
        x: width - margin - 62.0,
        y: height - 60.0,
        width: 62.0,
        height: 26.0,
    };
    let status = LayoutRect {
        x: width - margin - 164.0,
        y: toolbar_y + 5.0,
        width: 154.0,
        height: 20.0,
    };
    let empty_editor = LayoutRect {
        x: body.x,
        y: body.y + body.height * 0.45,
        width: body.width,
        height: 32.0,
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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum RtfSaveError {
    ExportUnavailable,
}

fn rtf_save_payload(payload: Option<Vec<u8>>) -> Result<Vec<u8>, RtfSaveError> {
    payload.ok_or(RtfSaveError::ExportUnavailable)
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
            list_stack.setSpacing(0.0);
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
            content.addSubview(&new_button);

            let search_field = NSSearchField::initWithFrame(
                NSSearchField::alloc(mtm),
                LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }.ns_rect(),
            );
            search_field.setPlaceholderString(Some(ns_string!("搜索笔记")));
            search_field.setContinuous(true);
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
            editor_empty_label.setStringValue(ns_string!("新建一条笔记开始记录"));
            editor_empty_label.setBezeled(false);
            editor_empty_label.setDrawsBackground(false);
            editor_empty_label.setEditable(false);
            editor_empty_label.setAlignment(NSTextAlignment::Center);
            editor_empty_label.setFont(Some(&NSFont::systemFontOfSize(17.0)));
            editor_empty_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            content.addSubview(&editor_empty_label);

            self.ivars().window.set(window.clone()).unwrap();
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
                item.setTarget(None);
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
        if let Some(list_scroll) = self.ivars().list_scroll.get() {
            list_scroll.setFrame(layout.list.ns_rect());
        }
        if let Some(list_stack) = self.ivars().list_stack.get() {
            let stack_height =
                (self.ivars().notes.borrow().len() as f64 * 58.0 + 24.0).max(layout.list.height);
            list_stack.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(layout.list.width, stack_height),
            ));
            for button in self.ivars().note_buttons.borrow().iter() {
                button.setFrame(NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new((layout.list.width - 24.0).max(180.0), 56.0),
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
                NSPoint::new(16.0, height - 58.0),
                NSSize::new(112.0, 32.0),
            ));
        }
        if let Some(search) = self.ivars().search_field.get() {
            let x = 136.0;
            let search_width = (layout.sidebar.width - x - 14.0).max(108.0);
            search.setFrame(NSRect::new(
                NSPoint::new(x, height - 58.0),
                NSSize::new(search_width, 32.0),
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
            if let Some(parsed) = parsed {
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
            stack.removeArrangedSubview(&button);
            button.removeFromSuperview();
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
                NSPoint::new(0.0, 0.0),
                NSSize::new(220.0, 56.0),
            ));
            stack.addArrangedSubview(&button);
            self.ivars().note_buttons.borrow_mut().push(button);
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
        let body = body_view.string().to_string();
        let rtf = match rtf_save_payload(
            body_view
                .RTFFromRange(NSRange::new(0, body_view.string().length()))
                .map(|data| data.to_vec()),
        ) {
            Ok(rtf) => rtf,
            Err(RtfSaveError::ExportUnavailable) => {
                self.set_save_status("保存失败", true);
                return;
            }
        };
        match self.ivars().repository.update_note(
            &id,
            joplin_lite_native::core::UpdateNote {
                title: Some(title),
                body: Some(body),
                body_rtf: Some(rtf),
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
                self.set_save_status("已保存", false);
            }
            Err(error) => {
                eprintln!("autosave failed: {error}");
                self.set_save_status("保存失败", true);
            }
        }
    }
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
        NSColor::controlAccentColor()
    } else {
        NSColor::labelColor()
    };
    let summary_color = NSColor::secondaryLabelColor();
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
        });
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContentLayout, DataDirError, FontTraitOperation, FormatDecision, FormatTarget,
        RtfLoadDecision, RtfSaveError, TextFormat, choose_data_dir, content_layout,
        display_note_title, format_decision, format_target, rtf_load_decision, rtf_save_payload,
        typing_trait_operation, validate_canonical_data_dir,
    };
    use objc2_foundation::NSRange;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

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
        }
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
    }

    #[test]
    fn bad_rtf_chooses_plain_body_without_replacing_saved_rtf() {
        assert_eq!(rtf_load_decision(false), RtfLoadDecision::PlainBodyFallback);
        assert_eq!(rtf_load_decision(true), RtfLoadDecision::ParsedRtf);
    }

    #[test]
    fn missing_rtf_export_fails_without_an_empty_replacement() {
        assert_eq!(rtf_save_payload(None), Err(RtfSaveError::ExportUnavailable),);
        assert_eq!(rtf_save_payload(Some(vec![])), Ok(Vec::new()));
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
