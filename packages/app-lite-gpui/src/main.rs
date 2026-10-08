//! Joplin Lite - a local-first native notes library built with GPUI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::borrow::Cow;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc;

use gpui::*;

mod app;
mod app_identity;
mod app_menu;
mod components;
mod config;
mod editor;
mod export;
mod extractor;
mod file_picker;
mod file_url;
mod i18n;
mod library_menu;
mod library_profile;
mod native_editor;
mod net;
mod spike_app;
mod theme;
mod ui;
mod window_chrome;

struct VelotypeAssets;

fn library_version_label() -> String {
    format!("Joplin Lite {}", env!("CARGO_PKG_VERSION"))
}

fn resolve_library_profile(profile_override: Option<OsString>) -> Result<PathBuf, String> {
    if let Some(profile_override) = profile_override {
        let profile = PathBuf::from(profile_override);
        if profile.as_os_str().is_empty() || !profile.is_absolute() {
            return Err("JOPLIN_LITE_PROFILE 必须是非空绝对路径。".into());
        }
        return Ok(profile);
    }
    directories::ProjectDirs::from("com", "ArielKevin", "Joplin Lite")
        .map(|dirs| dirs.data_local_dir().join("library"))
        .ok_or_else(|| "无法解析系统资料库目录；未使用工作目录作为回退。".into())
}

/// The formal product's bundle identifier. Any other
/// `com.arielkevin.joplinlite.*` identifier is an acceptance bundle.
const FORMAL_BUNDLE_IDENTIFIER: &str = "com.arielkevin.joplinlite";

/// Info.plist key with which an acceptance bundle pins its library.
const ACCEPTANCE_PROFILE_KEY: &str = "JoplinLiteAcceptanceProfile";

/// The Info.plist pin as read: no key, a string, or a key whose value is
/// not a string. Missing and malformed stay distinct from each other.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PinnedProfile {
    Absent,
    Value(OsString),
    Malformed,
}

/// What startup knows about where the library must be: the one-shot
/// environment override, the bundle identifier and the profile an
/// acceptance bundle pins in its Info.plist.
struct StartupProfileInput {
    env_profile: Option<OsString>,
    bundle_identifier: Option<String>,
    pinned_profile: PinnedProfile,
}

#[derive(Debug, PartialEq, Eq)]
struct StartupProfile {
    base: PathBuf,
    /// Explicit profiles are used as given, never redirected to the
    /// library the person last switched to.
    explicit: bool,
}

fn is_acceptance_bundle(identifier: Option<&str>) -> bool {
    identifier.is_some_and(|identifier| {
        identifier
            .strip_prefix(FORMAL_BUNDLE_IDENTIFIER)
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
    })
}

/// Evidence 86: an acceptance bundle relaunched without its one-shot
/// JOPLIN_LITE_PROFILE opened the person's default library. A pinned
/// profile is used on every launch; an acceptance identity without a valid
/// pin, or with an environment override naming another library, refuses to
/// start instead of falling back. Without a pin, the formal bundle and an
/// unbundled binary resolve as before.
fn resolve_startup_profile(input: StartupProfileInput) -> Result<StartupProfile, String> {
    match input.pinned_profile {
        PinnedProfile::Malformed => Err(format!(
            "验收包的 {ACCEPTANCE_PROFILE_KEY} 不是文本，已拒绝启动以免打开其他资料库。"
        )),
        PinnedProfile::Value(pinned) => {
            let base = resolve_library_profile(Some(pinned)).map_err(|_| {
                format!("验收包的 {ACCEPTANCE_PROFILE_KEY} 必须是非空绝对路径，已拒绝启动。")
            })?;
            if let Some(env) = input.env_profile
                && PathBuf::from(&env) != base
            {
                return Err(format!(
                    "JOPLIN_LITE_PROFILE 与验收包固定的资料库不同（{}），已拒绝启动。",
                    base.display()
                ));
            }
            Ok(StartupProfile {
                base,
                explicit: true,
            })
        }
        PinnedProfile::Absent if is_acceptance_bundle(input.bundle_identifier.as_deref()) => Err(
            format!("验收包缺少 {ACCEPTANCE_PROFILE_KEY}，已拒绝启动以免打开默认资料库。"),
        ),
        PinnedProfile::Absent => {
            let explicit = input.env_profile.is_some();
            resolve_library_profile(input.env_profile).map(|base| StartupProfile { base, explicit })
        }
    }
}

/// The running bundle's identifier and acceptance pin, read from its
/// Info.plist. An unbundled binary has neither.
#[cfg(target_os = "macos")]
fn bundle_profile_metadata() -> (Option<String>, PinnedProfile) {
    use cocoa::base::id;
    use objc::{class, msg_send, sel, sel_impl};
    // Drained when the closure returns or unwinds; only owned values leave.
    objc::rc::autoreleasepool(|| unsafe {
        let bundle: id = msg_send![class!(NSBundle), mainBundle];
        profile_metadata_from_bundle(bundle)
    })
}

/// A bundle's identifier and acceptance pin from its Info.plist; nil is no
/// bundle. Startup passes the main bundle, tests a bundle read from disk.
#[cfg(target_os = "macos")]
unsafe fn profile_metadata_from_bundle(bundle: cocoa::base::id) -> (Option<String>, PinnedProfile) {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{msg_send, sel, sel_impl};
    if bundle == nil {
        return (None, PinnedProfile::Absent);
    }
    // Drained when the closure returns or unwinds; only owned values leave.
    objc::rc::autoreleasepool(|| unsafe {
        let identifier: id = msg_send![bundle, bundleIdentifier];
        let key = NSString::alloc(nil)
            .init_str(ACCEPTANCE_PROFILE_KEY)
            .autorelease();
        let value: id = msg_send![bundle, objectForInfoDictionaryKey: key];
        (
            ns_string_value(identifier),
            pinned_profile_from_info_value(value),
        )
    })
}

#[cfg(not(target_os = "macos"))]
fn bundle_profile_metadata() -> (Option<String>, PinnedProfile) {
    (None, PinnedProfile::Absent)
}

/// Classifies an Info.plist value: nil is no key, an NSString is the pin,
/// anything else (a number, an array…) is malformed.
#[cfg(target_os = "macos")]
unsafe fn pinned_profile_from_info_value(value: cocoa::base::id) -> PinnedProfile {
    use cocoa::base::nil;
    use objc::{class, msg_send, sel, sel_impl};
    if value == nil {
        return PinnedProfile::Absent;
    }
    let is_string: bool = unsafe { msg_send![value, isKindOfClass: class!(NSString)] };
    if !is_string {
        return PinnedProfile::Malformed;
    }
    match unsafe { ns_string_value(value) } {
        Some(text) => PinnedProfile::Value(OsString::from(text)),
        None => PinnedProfile::Malformed,
    }
}

#[cfg(target_os = "macos")]
unsafe fn ns_string_value(string: cocoa::base::id) -> Option<String> {
    use cocoa::base::nil;
    use objc::{msg_send, sel, sel_impl};
    if string == nil {
        return None;
    }
    let utf8: *const std::os::raw::c_char = unsafe { msg_send![string, UTF8String] };
    if utf8.is_null() {
        return None;
    }
    unsafe { std::ffi::CStr::from_ptr(utf8) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

fn initialize_library_runtime(
    cx: &mut App,
    profile: PathBuf,
    open_url_receiver: mpsc::Receiver<Vec<String>>,
) -> Result<(), String> {
    let preferences = config::load_or_create_app_preferences()
        .map_err(|error| format!("无法加载应用偏好设置: {error}"))?;
    i18n::I18nManager::init_with_language_id(cx, &preferences.default_language_id);
    theme::ThemeManager::init_with_theme_id(cx, &preferences.default_theme_id);
    config::EditorSettings::init(cx, preferences.show_table_headers);
    components::init_with_keybindings(cx, &preferences.keybindings);
    library_menu::init(cx, profile, open_url_receiver);
    Ok(())
}

impl AssetSource for VelotypeAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        match path {
            "icon/workspace/folder.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/workspace/folder.svg"
            )))),
            "icon/workspace/markdown.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/workspace/markdown.svg"
            )))),
            "icon/titlebar/chrome-close.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/titlebar/chrome-close.svg"
            )))),
            "icon/titlebar/chrome-minimize.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/titlebar/chrome-minimize.svg"
            )))),
            "icon/titlebar/chrome-maximize.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/titlebar/chrome-maximize.svg"
            )))),
            "icon/titlebar/chrome-restore.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/titlebar/chrome-restore.svg"
            )))),
            "icon/editor/image.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/image.svg"
            )))),
            "icon/editor/undo.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/undo.svg"
            )))),
            "icon/editor/redo.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/redo.svg"
            )))),
            "icon/editor/bold.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/bold.svg"
            )))),
            "icon/editor/italic.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/italic.svg"
            )))),
            "icon/editor/underline.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/underline.svg"
            )))),
            "icon/editor/highlight.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/highlight.svg"
            )))),
            "icon/editor/bulleted-list.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/bulleted-list.svg"
            )))),
            "icon/editor/ordered-list.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/ordered-list.svg"
            )))),
            "icon/editor/checklist.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/checklist.svg"
            )))),
            "icon/editor/link.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/link.svg"
            )))),
            "icon/editor/align-left.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/align-left.svg"
            )))),
            "icon/editor/align-center.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/align-center.svg"
            )))),
            "icon/editor/align-right.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/align-right.svg"
            )))),
            "icon/editor/indent.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/indent.svg"
            )))),
            "icon/editor/outdent.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/outdent.svg"
            )))),
            "icon/editor/strike.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/editor/strike.svg"
            )))),
            _ => Ok(None),
        }
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // This worker must never resolve a profile or initialize the GPUI library.
    if args.get(1).is_some_and(|arg| arg == "--native-file-picker") {
        std::process::exit(file_picker::run_child());
    }
    // A LaunchServices/control-tool relaunch of the helper must never start a
    // second notes writer. Only its explicit picker mode above is permitted.
    if std::env::current_exe().is_ok_and(|path| file_picker::is_helper_executable(&path)) {
        eprintln!("picker helper requires --native-file-picker");
        std::process::exit(2);
    }
    // This must remain before all GPUI/profile initialization: the extractor
    // child receives only a bounded stdin stream and exits after one job.
    if args
        .get(1)
        .is_some_and(|arg| arg == "--extract-resource-text")
    {
        std::process::exit(extractor::run_child(&args[2..]));
    }
    if let Some(request) = spike_app::global_help_or_version(&args[1..]) {
        match request {
            "version" => {
                println!("{}", library_version_label());
            }
            "help" => print_help(),
            _ => unreachable!("global help/version parser returned unknown request"),
        }
        return;
    }
    let measurement_requested = spike_app::measurement_options_requested(&args[1..]);
    let spike_options = if measurement_requested {
        match spike_app::parse_spike_options(&args[1..]) {
            Ok(options) => Some(options),
            Err(error) => {
                eprintln!("invalid Task 7 spike options: {error}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    // Parse command-line arguments
    let mut detach = false;
    let mut evernote_spike = false;
    let mut input_paths = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-v" | "-V" | "--help" | "-h" => {
                unreachable!("global help/version options are handled before value parsing")
            }
            "--detach" | "-d" => {
                detach = true;
            }
            "--evernote-spike" => {
                evernote_spike = true;
            }
            "--fixture" | "--ready-file" | "--diagnostics-file" => {
                i += 1;
            }
            option if option.starts_with('-') => {
                eprintln!("Unknown option: {}", option);
                std::process::exit(1);
            }
            path => {
                input_paths.push(PathBuf::from(path));
            }
        }
        i += 1;
    }

    #[cfg(not(target_os = "macos"))]
    let _ = detach;

    // On macOS, detach from terminal if requested
    // TODO: Other platforms may also need to be adapted
    #[cfg(target_os = "macos")]
    if detach {
        use std::process::Command;

        // Re-launch the application in the background without the --detach flag
        let exe_path = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("无法确定 Joplin Lite 可执行文件以后台启动: {error}");
                std::process::exit(1);
            }
        };
        let non_detach_args: Vec<String> = args
            .iter()
            .filter(|arg| *arg != "--detach" && *arg != "-d")
            .cloned()
            .collect();

        if let Err(error) = Command::new(exe_path).args(&non_detach_args[1..]).spawn() {
            eprintln!("无法在后台启动 Joplin Lite: {error}");
            std::process::exit(1);
        }

        return;
    }

    let (open_url_sender, open_url_receiver) = mpsc::channel::<Vec<String>>();
    let app = Application::new().with_assets(VelotypeAssets);
    app.on_open_urls(move |urls| {
        let _ = open_url_sender.send(urls);
    });

    app.run(move |cx: &mut App| {
        // This must precede profile resolution and runtime initialization so
        // even an early visible StartupErrorView has ordinary last-window
        // quit semantics.
        library_menu::install_last_window_quit(cx);
        if evernote_spike {
            // Keep this route intentionally below the ordinary app bootstrap:
            // it needs the donor key/action table, but not donor workspace,
            // menu, network, updater, exporter, sync, or web runtimes.
            components::init(cx);
            // This route intentionally skips `init_app_menu`, which is where
            // the ordinary application activates itself. The native GPUI
            // window must still be active so macOS keeps its display link
            // driving the measurement frames after the asynchronous workload.
            cx.activate(true);
            spike_app::open_with_options(cx, spike_options.clone());
            cx.refresh_windows();
            return;
        }

        // The ordinary route is a local library, never the editor spike or a
        // sample document. A temporary profile can be supplied for smoke tests,
        // but it must be an explicit absolute path. An acceptance bundle's
        // pinned profile is resolved here, before preferences or any library
        // file is opened.
        let (bundle_identifier, pinned_profile) = bundle_profile_metadata();
        let startup = resolve_startup_profile(StartupProfileInput {
            env_profile: std::env::var_os("JOPLIN_LITE_PROFILE"),
            bundle_identifier,
            pinned_profile,
        });
        let (base, explicit) = match startup {
            Ok(StartupProfile { base, explicit }) => (base, explicit),
            Err(error) => {
                let _ = ui::open_startup_error_window(cx, error);
                cx.activate(true);
                return;
            }
        };
        // An explicit profile is always honoured; otherwise reopen the
        // imported library the person last switched to, if still valid.
        let profile = if explicit {
            base.clone()
        } else {
            library_profile::resolve_active(&base)
        };
        let profiles = library_profile::LibraryProfiles {
            base,
            active: profile.clone(),
        };
        let restore_parent = profiles.imports_dir();
        // A crashed restore cannot drop its TempDir. Reclaim only marked,
        // unlocked scratch directories, off the UI thread; never fail opening
        // the current library because optional scratch cleanup failed.
        cx.background_executor().spawn(async move {
            if let Err(error) = app_lite_core::cleanup_abandoned_restore_staging(&restore_parent) {
                eprintln!("无法清理中断恢复暂存（当前资料库未更改）: {error}");
            }
            if let Err(error) = app_lite_core::cleanup_abandoned_import_staging(&restore_parent.join(".staging")) {
                eprintln!("无法清理中断导入暂存（当前资料库未更改）: {error}");
            }
        }).detach();
        cx.set_global(profiles);
        if let Err(error) = initialize_library_runtime(cx, profile.clone(), open_url_receiver) {
            let _ = ui::open_startup_error_window(cx, error);
            cx.activate(true);
            return;
        }
        let import_notice =
            (!input_paths.is_empty()).then(|| library_menu::import_notice(&input_paths));
        match ui::open_library_window_with_notice(cx, profile, import_notice) {
            Ok(_) => {
                cx.activate(true);
                cx.refresh_windows();
            }
            Err(error) => {
                let _ = ui::open_startup_error_window(
                    cx,
                    format!("Joplin Lite 无法打开本地资料库: {error}"),
                );
                cx.activate(true);
            }
        }
        return;
    });
}

#[cfg(test)]
mod tests {
    use super::{AssetSource, VelotypeAssets, library_version_label, resolve_library_profile};
    use crate::native_editor::commands::CommandCatalogue;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn editor_command_icons_load_from_the_real_application_asset_source() {
        let assets = VelotypeAssets;
        for descriptor in CommandCatalogue::new().descriptors() {
            let Some(path) = descriptor.icon_path else {
                continue;
            };
            let bytes = assets
                .load(path)
                .expect("asset source should not fail")
                .unwrap_or_else(|| panic!("missing descriptor asset: {path}"));
            assert!(!bytes.is_empty(), "empty descriptor asset: {path}");
        }
    }

    #[test]
    fn library_profile_override_rejects_empty_and_relative_paths() {
        assert!(resolve_library_profile(Some(OsString::new())).is_err());
        assert!(resolve_library_profile(Some(OsString::from("relative-profile"))).is_err());
        assert_eq!(
            resolve_library_profile(Some(OsString::from("/tmp/joplin-lite-profile"))).unwrap(),
            PathBuf::from("/tmp/joplin-lite-profile")
        );
    }

    // Evidence 86: an acceptance bundle relaunched without its one-shot
    // JOPLIN_LITE_PROFILE opened the person's default library. A bundle
    // that pins its profile keeps it on every launch; an acceptance
    // identity without a usable pin refuses to start rather than fall back.
    #[test]
    fn startup_profile_pins_acceptance_bundles_and_fails_closed() {
        use super::{PinnedProfile, StartupProfile, StartupProfileInput, resolve_startup_profile};
        const FORMAL: &str = "com.arielkevin.joplinlite";
        const ACCEPTANCE: &str = "com.arielkevin.joplinlite.acceptance.caretoct2";
        let pin = "/tmp/joplin-codex-caret-profile-oct2";
        let typed =
            |env: Option<&str>, id: Option<&str>, pinned: PinnedProfile| StartupProfileInput {
                env_profile: env.map(OsString::from),
                bundle_identifier: id.map(str::to_owned),
                pinned_profile: pinned,
            };
        let input = |env: Option<&str>, id: Option<&str>, pinned: Option<&str>| {
            typed(
                env,
                id,
                pinned.map_or(PinnedProfile::Absent, |pinned| {
                    PinnedProfile::Value(OsString::from(pinned))
                }),
            )
        };
        let pinned = || {
            Some(StartupProfile {
                base: PathBuf::from(pin),
                explicit: true,
            })
        };
        let default = resolve_library_profile(None).unwrap();
        let mut failures = Vec::new();
        let mut check =
            |name: &str, got: Result<StartupProfile, String>, want_ok: Option<StartupProfile>| {
                let pass = match (&got, &want_ok) {
                    (Ok(got), Some(want)) => got == want,
                    (Err(_), None) => true,
                    _ => false,
                };
                if !pass {
                    failures.push(format!(
                        "{name}: got {got:?}, want {}",
                        match &want_ok {
                            Some(want) => format!("Ok({want:?})"),
                            None => "Err(refused)".to_owned(),
                        }
                    ));
                }
            };

        // Acceptance identity, relaunched without the environment variable.
        check(
            "acceptance + pin, no env",
            resolve_startup_profile(input(None, Some(ACCEPTANCE), Some(pin))),
            pinned(),
        );
        check(
            "acceptance + same env",
            resolve_startup_profile(input(Some(pin), Some(ACCEPTANCE), Some(pin))),
            pinned(),
        );
        check(
            "acceptance + other env",
            resolve_startup_profile(input(Some("/tmp/elsewhere"), Some(ACCEPTANCE), Some(pin))),
            None,
        );
        check(
            "acceptance, no pin, no env",
            resolve_startup_profile(input(None, Some(ACCEPTANCE), None)),
            None,
        );
        check(
            "acceptance, no pin, env only",
            resolve_startup_profile(input(Some(pin), Some(ACCEPTANCE), None)),
            None,
        );
        check(
            "acceptance + empty pin",
            resolve_startup_profile(input(None, Some(ACCEPTANCE), Some(""))),
            None,
        );
        check(
            "acceptance + relative pin",
            resolve_startup_profile(input(None, Some(ACCEPTANCE), Some("relative"))),
            None,
        );
        // A pin is an explicit opt-in on any bundle.
        check(
            "formal + pin, no env",
            resolve_startup_profile(input(None, Some(FORMAL), Some(pin))),
            pinned(),
        );
        // Unchanged: formal identity or no bundle, without a pin.
        check(
            "formal, no pin, no env",
            resolve_startup_profile(input(None, Some(FORMAL), None)),
            Some(StartupProfile {
                base: default.clone(),
                explicit: false,
            }),
        );
        check(
            "formal, no pin, env",
            resolve_startup_profile(input(Some(pin), Some(FORMAL), None)),
            pinned(),
        );
        check(
            "no bundle, no pin, no env",
            resolve_startup_profile(input(None, None, None)),
            Some(StartupProfile {
                base: default.clone(),
                explicit: false,
            }),
        );
        check(
            "no bundle, no pin, relative env",
            resolve_startup_profile(input(Some("relative"), None, None)),
            None,
        );
        // A key whose value is not a string is refused on any bundle, with
        // or without an environment override; it is not read as absent.
        check(
            "acceptance + malformed pin",
            resolve_startup_profile(typed(None, Some(ACCEPTANCE), PinnedProfile::Malformed)),
            None,
        );
        check(
            "formal + malformed pin",
            resolve_startup_profile(typed(None, Some(FORMAL), PinnedProfile::Malformed)),
            None,
        );
        check(
            "no bundle + malformed pin + env",
            resolve_startup_profile(typed(Some(pin), None, PinnedProfile::Malformed)),
            None,
        );
        // Only the formal identifier and its own sub-identifiers count; a
        // look-alike prefix is an ordinary bundle.
        check(
            "look-alike id, no pin, no env",
            resolve_startup_profile(input(None, Some("com.arielkevin.joplinlitex"), None)),
            Some(StartupProfile {
                base: default.clone(),
                explicit: false,
            }),
        );

        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    // The Info.plist value as Foundation hands it over: no key, a string,
    // or another property-list type.
    #[cfg(target_os = "macos")]
    #[test]
    fn info_plist_pin_values_are_typed_absent_value_or_malformed() {
        use super::{PinnedProfile, pinned_profile_from_info_value};
        use cocoa::base::{id, nil};
        use cocoa::foundation::{NSAutoreleasePool, NSString};
        use objc::{class, msg_send, sel, sel_impl};
        objc::rc::autoreleasepool(|| unsafe {
            let path = NSString::alloc(nil)
                .init_str("/tmp/joplin-codex-caret-profile-oct2")
                .autorelease();
            let empty = NSString::alloc(nil).init_str("").autorelease();
            let number: id = msg_send![class!(NSNumber), numberWithInt: 1i32];
            let array: id = msg_send![class!(NSArray), array];
            assert_eq!(pinned_profile_from_info_value(nil), PinnedProfile::Absent);
            assert_eq!(
                pinned_profile_from_info_value(path),
                PinnedProfile::Value(OsString::from("/tmp/joplin-codex-caret-profile-oct2"))
            );
            // An empty string is a value; startup then refuses it as a path.
            assert_eq!(
                pinned_profile_from_info_value(empty),
                PinnedProfile::Value(OsString::new())
            );
            assert_eq!(
                pinned_profile_from_info_value(number),
                PinnedProfile::Malformed
            );
            assert_eq!(
                pinned_profile_from_info_value(array),
                PinnedProfile::Malformed
            );
        });
    }

    // A test binary is not an app bundle: no identifier, no pin, so startup
    // resolves exactly as before.
    #[test]
    fn unbundled_binary_has_no_acceptance_metadata() {
        use super::{PinnedProfile, bundle_profile_metadata};
        let (identifier, pinned) = bundle_profile_metadata();
        assert!(
            identifier
                .as_deref()
                .is_none_or(|id| !id.starts_with("com.arielkevin.joplinlite")),
            "{identifier:?}"
        );
        assert_eq!(pinned, PinnedProfile::Absent);
    }

    // The whole chain on real bundles: an Info.plist on disk, read through
    // NSBundle by the same adapter startup uses, then resolved. No app,
    // preferences or library is opened.
    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_info_plist_on_disk_resolves_through_the_startup_guard() {
        use super::{
            PinnedProfile, StartupProfile, StartupProfileInput, profile_metadata_from_bundle,
            resolve_startup_profile,
        };
        use cocoa::base::{id, nil};
        use cocoa::foundation::{NSAutoreleasePool, NSString};
        use objc::{class, msg_send, sel, sel_impl};
        const FORMAL: &str = "com.arielkevin.joplinlite";
        const ACCEPTANCE: &str = "com.arielkevin.joplinlite.acceptance.caretoct2";
        let pin = "/tmp/joplin-codex-caret-profile-oct2";
        let default = resolve_library_profile(None).unwrap();
        let pinned = |path: &str| PinnedProfile::Value(OsString::from(path));
        // (case, identifier, pin key's plist value, metadata pin, startup)
        let cases = [
            (
                "acceptance + valid pin",
                ACCEPTANCE,
                Some(format!("<string>{pin}</string>")),
                pinned(pin),
                Some(StartupProfile {
                    base: PathBuf::from(pin),
                    explicit: true,
                }),
            ),
            (
                "acceptance, no pin",
                ACCEPTANCE,
                None,
                PinnedProfile::Absent,
                None,
            ),
            (
                "acceptance + number pin",
                ACCEPTANCE,
                Some("<integer>1</integer>".to_owned()),
                PinnedProfile::Malformed,
                None,
            ),
            (
                "acceptance + array pin",
                ACCEPTANCE,
                Some(format!("<array><string>{pin}</string></array>")),
                PinnedProfile::Malformed,
                None,
            ),
            (
                "acceptance + empty pin",
                ACCEPTANCE,
                Some("<string></string>".to_owned()),
                pinned(""),
                None,
            ),
            (
                "acceptance + relative pin",
                ACCEPTANCE,
                Some("<string>relative/library</string>".to_owned()),
                pinned("relative/library"),
                None,
            ),
            (
                "formal, no pin",
                FORMAL,
                None,
                PinnedProfile::Absent,
                Some(StartupProfile {
                    base: default.clone(),
                    explicit: false,
                }),
            ),
        ];
        let root = tempfile::tempdir().unwrap();
        for (index, (case, identifier, pin_value, want_pin, want_startup)) in
            cases.into_iter().enumerate()
        {
            // One bundle path per case: NSBundle caches bundles by path.
            let app = root.path().join(format!("case-{index}.app"));
            std::fs::create_dir_all(app.join("Contents")).unwrap();
            let pin_entry = pin_value
                .map(|value| format!("<key>JoplinLiteAcceptanceProfile</key>{value}"))
                .unwrap_or_default();
            std::fs::write(
                app.join("Contents/Info.plist"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                     <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
                     \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                     <plist version=\"1.0\"><dict>\
                     <key>CFBundleIdentifier</key><string>{identifier}</string>\
                     <key>CFBundlePackageType</key><string>APPL</string>\
                     {pin_entry}</dict></plist>\n"
                ),
            )
            .unwrap();
            let (read_identifier, read_pin) = objc::rc::autoreleasepool(|| unsafe {
                let path = NSString::alloc(nil)
                    .init_str(app.to_str().unwrap())
                    .autorelease();
                let bundle: id = msg_send![class!(NSBundle), bundleWithPath: path];
                assert!(bundle != nil, "{case}: bundle not readable");
                profile_metadata_from_bundle(bundle)
            });
            assert_eq!(read_identifier.as_deref(), Some(identifier), "{case}");
            assert_eq!(read_pin, want_pin, "{case}");
            let startup = resolve_startup_profile(StartupProfileInput {
                env_profile: None,
                bundle_identifier: read_identifier,
                pinned_profile: read_pin,
            });
            match want_startup {
                Some(want) => assert_eq!(startup, Ok(want), "{case}"),
                None => assert!(startup.is_err(), "{case}: {startup:?}"),
            }
        }
    }

    #[test]
    fn ordinary_product_version_uses_the_library_identity() {
        assert!(library_version_label().starts_with("Joplin Lite "));
        assert!(!library_version_label().contains("velotype"));
    }
}

fn print_help() {
    println!(
        "Joplin Lite {} - 本地优先的原生笔记资料库",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("USAGE:");
    println!("    joplin-lite [OPTIONS] [FILES...]");
    println!();
    println!("OPTIONS:");
    println!("    -v, --version    Print version information");
    println!("    -V               Print version information");
    println!("    -h, --help       Print this help message");
    println!("    -d, --detach     Launch in background (non-blocking)");
    println!("        --evernote-spike  Launch the native editor spike");
    println!("        --fixture empty|typical|long  Task 7 deterministic fixture");
    println!("        --ready-file PATH  Task 7 readiness marker (absolute)");
    println!("        --diagnostics-file PATH  Task 7 JSON diagnostics (absolute)");
    println!();
    println!("FILES:");
    println!("    传入的文件会显示“暂不支持导入”提示；不会读取或修改源文件。");
    println!("    未传入文件时打开本地笔记资料库。");
}
