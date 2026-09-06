use joplin_lite_native::core::{CreateNote, Note, NoteRepository};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSButton, NSControlTextEditingDelegate, NSEventModifierFlags, NSMenu, NSMenuItem, NSScrollView,
    NSSearchField, NSStackView, NSTextDelegate, NSTextField, NSTextFieldDelegate, NSTextView,
    NSTextViewDelegate, NSUserInterfaceLayoutOrientation, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSData, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect,
    NSSize, ns_string,
};
use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

struct AppDelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    repository: Arc<NoteRepository>,
    current_note_id: RefCell<Option<String>>,
    notes: RefCell<Vec<Note>>,
    title_field: OnceCell<Retained<NSTextField>>,
    body_view: OnceCell<Retained<NSTextView>>,
    search_field: OnceCell<Retained<NSSearchField>>,
    list_stack: OnceCell<Retained<NSStackView>>,
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
            let application = notification.object().unwrap().downcast::<NSApplication>().unwrap();
            let window = unsafe { NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(980.0, 680.0)), NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable, NSBackingStoreType::Buffered, false) };
            unsafe { window.setReleasedWhenClosed(false) };
            window.setTitle(ns_string!("Joplin Lite Native"));
            let list_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::new(16.0, 32.0), NSSize::new(280.0, 550.0)));
            list_scroll.setHasVerticalScroller(true);
            let list_stack = NSStackView::initWithFrame(NSStackView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 540.0)));
            list_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            list_stack.setSpacing(4.0);
            list_stack.setEdgeInsets(objc2_foundation::NSEdgeInsets { top: 8.0, left: 8.0, bottom: 8.0, right: 8.0 });
            list_scroll.setDocumentView(Some(&list_stack));
            window.contentView().unwrap().addSubview(&list_scroll);
            let search_field = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), NSRect::new(NSPoint::new(16.0, 600.0), NSSize::new(280.0, 32.0)));
            search_field.setPlaceholderString(Some(ns_string!("搜索笔记")));
            search_field.setContinuous(true);
            unsafe {
                search_field.setTarget(Some(self));
                search_field.setAction(Some(sel!(searchNotes:)));
            }
            window.contentView().unwrap().addSubview(&search_field);
            let title_field = NSTextField::initWithFrame(NSTextField::alloc(mtm), NSRect::new(NSPoint::new(312.0, 550.0), NSSize::new(636.0, 32.0)));
            title_field.setStringValue(ns_string!(""));
            title_field.setEditable(true);
            window.contentView().unwrap().addSubview(&title_field);
            let body = NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(620.0, 480.0)));
            body.setEditable(true); body.setRichText(true); body.setAllowsUndo(true);
            let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::new(312.0, 32.0), NSSize::new(636.0, 500.0)));
            scroll.setHasVerticalScroller(true); scroll.setDocumentView(Some(&body));
            window.contentView().unwrap().addSubview(&scroll);
            let button = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("新建笔记"), Some(self), Some(sel!(newNote:)), mtm) };
            button.setFrame(NSRect::new(NSPoint::new(312.0, 600.0), NSSize::new(140.0, 32.0)));
            window.contentView().unwrap().addSubview(&button);
            let delete_button = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("删除"), Some(self), Some(sel!(deleteNote:)), mtm) };
            delete_button.setFrame(NSRect::new(NSPoint::new(460.0, 600.0), NSSize::new(100.0, 32.0)));
            window.contentView().unwrap().addSubview(&delete_button);
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("主菜单"));
            let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("应用"));
            let quit_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("退出"), Some(sel!(terminate:)), ns_string!("q")) };
            unsafe { quit_item.setTarget(None); }
            quit_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
            app_menu.addItem(&quit_item);
            let app_menu_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("应用"), None, ns_string!("")) };
            app_menu_item.setSubmenu(Some(&app_menu));
            menu.addItem(&app_menu_item);
            let file_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("文件"));
            let new_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("新建笔记"), Some(sel!(newNote:)), ns_string!("n")) };
            unsafe { new_item.setTarget(Some(self)); file_menu.addItem(&new_item); }
            let file_menu_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("文件"), None, ns_string!("")) };
            file_menu_item.setSubmenu(Some(&file_menu)); menu.addItem(&file_menu_item);
            let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("编辑"));
            let undo_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("撤销"), Some(sel!(undo:)), ns_string!("z")) };
            let redo_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("重做"), Some(sel!(redo:)), ns_string!("z")) };
            let cut_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("剪切"), Some(sel!(cut:)), ns_string!("x")) };
            let copy_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("拷贝"), Some(sel!(copy:)), ns_string!("c")) };
            let paste_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("粘贴"), Some(sel!(paste:)), ns_string!("v")) };
            let select_all_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("全选"), Some(sel!(selectAll:)), ns_string!("a")) };
            for item in [&undo_item, &redo_item, &cut_item, &copy_item, &paste_item, &select_all_item] {
                unsafe { item.setTarget(None); }
                item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
            }
            redo_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
            edit_menu.addItem(&undo_item);
            edit_menu.addItem(&redo_item);
            edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
            edit_menu.addItem(&cut_item);
            edit_menu.addItem(&copy_item);
            edit_menu.addItem(&paste_item);
            edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
            edit_menu.addItem(&select_all_item);
            let edit_menu_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("编辑"), None, ns_string!("")) };
            edit_menu_item.setSubmenu(Some(&edit_menu));
            menu.addItem(&edit_menu_item);
            let format_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("格式"));
            let bold_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("粗体"), Some(sel!(toggleBoldface:)), ns_string!("b")) };
            let italic_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("斜体"), Some(sel!(toggleItalics:)), ns_string!("i")) };
            let underline_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("下划线"), Some(sel!(underline:)), ns_string!("u")) };
            for item in [&bold_item, &italic_item, &underline_item] {
                unsafe { item.setTarget(None); }
                item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
                format_menu.addItem(item);
            }
            let format_menu_item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("格式"), None, ns_string!("")) };
            format_menu_item.setSubmenu(Some(&format_menu));
            menu.addItem(&format_menu_item);
            application.setMainMenu(Some(&menu));
            window.center(); window.makeKeyAndOrderFront(None);
            window.setDelegate(Some(ProtocolObject::from_ref(self)));
            self.ivars().window.set(window).unwrap();
            self.ivars().title_field.set(title_field.clone()).unwrap();
            self.ivars().body_view.set(body.clone()).unwrap();
            self.ivars().search_field.set(search_field).unwrap();
            self.ivars().list_stack.set(list_stack).unwrap();
            unsafe { title_field.setDelegate(Some(ProtocolObject::from_ref(self))); }
            body.setDelegate(Some(ProtocolObject::from_ref(self)));
            #[allow(deprecated)] application.activateIgnoringOtherApps(true);
            self.refresh_notes();
            if let Some(note) = self.ivars().notes.borrow().first().cloned() { self.load_note(&note); }
            if let Some(count) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_NEW_COUNT") {
                let count = count.to_string_lossy().parse::<usize>().unwrap_or(0);
                let sender = NSObject::new();
                for _ in 0..count { self.new_note(sel!(newNote:), &sender); }
            } else if std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_NEW_NOTE").is_some() { let sender = NSObject::new(); self.new_note(sel!(newNote:), &sender); }
            if let Some(body) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_BODY") { self.ivars().body_view.get().unwrap().setString(&objc2_foundation::NSString::from_str(&body.to_string_lossy())); self.save_current_note(); }
            if let Some(query) = std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_SEARCH") { self.search_notes(&query.to_string_lossy()); println!("searchNotes: result count={}", self.ivars().notes.borrow().len()); }
            if std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_DELETE_CURRENT").is_some() { let sender = NSObject::new(); self.delete_note(sel!(deleteNote:), &sender); }
            if std::env::var_os("JOPLIN_LITE_NATIVE_SMOKE_EXIT").is_some() { application.terminate(None); }
        }
    }
    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) { NSApplication::sharedApplication(self.mtm()).terminate(None); }
    }
    unsafe impl NSControlTextEditingDelegate for AppDelegate {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _obj: &NSNotification) { self.save_current_note(); }
    }
    unsafe impl NSTextDelegate for AppDelegate {
        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) { self.save_current_note(); }
    }
    unsafe impl NSTextFieldDelegate for AppDelegate {}
    unsafe impl NSTextViewDelegate for AppDelegate {}
    impl AppDelegate {
        #[unsafe(method(newNote:))]
        fn new_note(&self, _sender: &NSObject) {
            let note = self.ivars().repository.create_note(CreateNote { title: String::new(), body: String::new(), body_rtf: Vec::new(), is_draft: true });
            match note {
                Ok(note) => { self.ivars().search_field.get().unwrap().setStringValue(ns_string!("")); self.load_note(&note); self.refresh_notes(); if let Some(window) = self.ivars().window.get() { window.setTitle(ns_string!("Joplin Lite Native — 新建笔记")); window.makeFirstResponder(Some(self.ivars().body_view.get().unwrap())); } println!("newNote: action triggered"); }
                Err(error) => eprintln!("newNote: could not create note: {error}"),
            }
        }
        #[unsafe(method(selectNote:))]
        fn select_note(&self, sender: &NSButton) {
            let index = sender.tag();
            if index < 0 { return; }
            if let Some(note) = self.ivars().notes.borrow().get(index as usize).cloned() { self.load_note(&note); }
        }
        #[unsafe(method(searchNotes:))]
        fn search_notes_action(&self, sender: &NSSearchField) { self.search_notes(&sender.stringValue().to_string()); }
        #[unsafe(method(deleteNote:))]
        fn delete_note(&self, _sender: &NSObject) {
            let Some(id) = self.ivars().current_note_id.borrow_mut().take() else { return; };
            if let Err(error) = self.ivars().repository.soft_delete(&id) { eprintln!("deleteNote: could not delete note: {error}"); *self.ivars().current_note_id.borrow_mut() = Some(id); return; }
            let query = self.ivars().search_field.get().map(|field| field.stringValue().to_string()).unwrap_or_default();
            self.search_notes(&query);
            if let Some(note) = self.ivars().notes.borrow().first().cloned() { self.load_note(&note); } else {
                self.ivars().title_field.get().unwrap().setStringValue(ns_string!(""));
                self.ivars().body_view.get().unwrap().setString(ns_string!(""));
            }
        }
    }
);

impl AppDelegate {
    fn load_note(&self, note: &Note) {
        *self.ivars().current_note_id.borrow_mut() = Some(note.id.clone());
        self.ivars()
            .title_field
            .get()
            .unwrap()
            .setStringValue(&objc2_foundation::NSString::from_str(&note.title));
        let body_view = self.ivars().body_view.get().unwrap();
        body_view.setString(ns_string!(""));
        if !note.body_rtf.is_empty() {
            let rtf = NSData::with_bytes(&note.body_rtf);
            body_view.replaceCharactersInRange_withRTF(NSRange::new(0, 0), &rtf);
        } else {
            body_view.setString(&objc2_foundation::NSString::from_str(&note.body));
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
        for (index, note) in self.ivars().notes.borrow().iter().enumerate() {
            let title = if note.title.trim().is_empty() {
                "无标题笔记"
            } else {
                &note.title
            };
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &objc2_foundation::NSString::from_str(title),
                    Some(self),
                    Some(sel!(selectNote:)),
                    self.mtm(),
                )
            };
            button.setTag(index as isize);
            button.setAlignment(objc2_app_kit::NSTextAlignment::Left);
            stack.addArrangedSubview(&button);
            self.ivars().note_buttons.borrow_mut().push(button);
        }
        let height = (self.ivars().notes.borrow().len() as f64 * 28.0 + 16.0).max(540.0);
        let mut frame = stack.frame();
        frame.size.height = height;
        stack.setFrame(frame);
    }

    fn save_current_note(&self) {
        let Some(id) = self.ivars().current_note_id.borrow().clone() else {
            return;
        };
        let title = self
            .ivars()
            .title_field
            .get()
            .map(|f| f.stringValue().to_string())
            .unwrap_or_default();
        let body_view = self.ivars().body_view.get().unwrap();
        let body = body_view.string().to_string();
        let display_title = if title.trim().is_empty() {
            body.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(120)
                .collect()
        } else {
            title
        };
        if let Some(field) = self.ivars().title_field.get() {
            field.setStringValue(&objc2_foundation::NSString::from_str(&display_title));
        }
        let rtf = body_view
            .RTFFromRange(NSRange::new(0, body_view.string().length()))
            .map(|data| data.to_vec())
            .unwrap_or_default();
        match self.ivars().repository.update_note(
            &id,
            joplin_lite_native::core::UpdateNote {
                title: Some(display_title),
                body: Some(body),
                body_rtf: Some(rtf),
            },
        ) {
            Ok(updated) => {
                let index = {
                    let notes = self.ivars().notes.borrow();
                    notes.iter().position(|note| note.id == updated.id)
                };
                if let Some(index) = index {
                    self.ivars().notes.borrow_mut()[index] = updated.clone();
                    if let Some(button) = self.ivars().note_buttons.borrow().get(index) {
                        let title = if updated.title.trim().is_empty() {
                            "无标题笔记"
                        } else {
                            &updated.title
                        };
                        button.setTitle(&objc2_foundation::NSString::from_str(title));
                    }
                }
            }
            Err(error) => eprintln!("autosave failed: {error}"),
        }
    }
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("AppKit must run on the main thread");
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let mut data_dir = std::env::var_os("JOPLIN_LITE_NATIVE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("com.kevinhao.joplin-lite-native")
        });
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("could not create data directory: {error}");
    }
    data_dir.push("notes.sqlite");
    let repository =
        Arc::new(NoteRepository::open(&data_dir).expect("could not open notes database"));
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
            title_field: OnceCell::new(),
            body_view: OnceCell::new(),
            search_field: OnceCell::new(),
            list_stack: OnceCell::new(),
            note_buttons: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(this), init] }
    }
}
