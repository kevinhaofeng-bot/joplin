//! Velotype - a block-based Markdown editor built with GPUI.
//!
//! Reads file paths from command-line arguments and opens one GPUI window per
//! file. With no arguments, a single empty window is created.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::borrow::Cow;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[cfg(target_os = "macos")]
use futures::{StreamExt, channel::mpsc};
use gpui::*;

mod app_identity;
mod app_menu;
mod components;
mod config;
mod editor;
mod export;
#[cfg(any(target_os = "macos", test))]
mod file_url;
mod i18n;
mod native_editor;
mod net;
mod spike_app;
mod theme;
mod window_chrome;

use app_menu::{init as init_app_menu, open_editor_window};
use components::init_with_keybindings as init_editor;
#[cfg(target_os = "macos")]
use file_url::parse_file_url;
use i18n::I18nManager;
use theme::ThemeManager;

struct VelotypeAssets;

fn open_startup_window(cx: &mut App, startup_open: config::StartupOpenPreference) {
    if startup_open == config::StartupOpenPreference::LastOpenedFile
        && let Some(path) = config::first_existing_recent_markdown_file()
    {
        match std::fs::read_to_string(&path) {
            Ok(markdown) => {
                open_editor_window(cx, markdown, Some(path));
                return;
            }
            Err(err) => {
                eprintln!(
                    "failed to read last opened file '{}': {err}",
                    path.display()
                );
            }
        }
    }

    open_editor_window(cx, String::new(), None);
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
    if let Some(request) = spike_app::global_help_or_version(&args[1..]) {
        match request {
            "version" => {
                println!("velotype {}", env!("CARGO_PKG_VERSION"));
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
        let exe_path = std::env::current_exe().expect("Failed to get executable path");
        let non_detach_args: Vec<String> = args
            .iter()
            .filter(|arg| *arg != "--detach" && *arg != "-d")
            .cloned()
            .collect();

        Command::new(exe_path)
            .args(&non_detach_args[1..])
            .spawn()
            .expect("Failed to detach process");

        return;
    }

    #[cfg(target_os = "macos")]
    let (open_file_tx, mut open_file_rx) = mpsc::unbounded::<PathBuf>();
    #[cfg(target_os = "macos")]
    let open_file_requested = Arc::new(AtomicBool::new(false));

    let app = Application::new().with_assets(VelotypeAssets);

    #[cfg(target_os = "macos")]
    {
        let open_file_requested_for_callback = open_file_requested.clone();
        app.on_open_urls(move |urls| {
            for url in urls {
                let Some(path) = parse_file_url(&url) else {
                    continue;
                };
                open_file_requested_for_callback.store(true, Ordering::SeqCst);
                let _ = open_file_tx.unbounded_send(path);
            }
        });
    }

    app.run(move |cx: &mut App| {
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

        let preferences = config::load_or_create_app_preferences().unwrap_or_else(|err| {
            eprintln!("failed to initialize app preferences: {err}");
            Default::default()
        });
        I18nManager::init_with_language_id(cx, &preferences.default_language_id);
        ThemeManager::init_with_theme_id(cx, &preferences.default_theme_id);
        config::EditorSettings::init(cx, preferences.show_table_headers);
        net::install_http_client(cx);
        init_editor(cx, &preferences.keybindings);
        init_app_menu(cx);

        #[cfg(target_os = "macos")]
        cx.spawn(async move |cx| {
            while let Some(path) = open_file_rx.next().await {
                let _ = cx.update(move |cx| {
                    if let Err(err) = app_menu::open_file_in_new_window(cx, &path) {
                        eprintln!("failed to open '{}': {err}", path.display());
                    }
                });
            }
        })
        .detach();

        if input_paths.is_empty() {
            #[cfg(target_os = "macos")]
            {
                let startup_open = preferences.startup_open;
                let open_file_requested = open_file_requested.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(150))
                        .await;
                    if !open_file_requested.load(Ordering::SeqCst) {
                        let _ = cx.update(move |cx| open_startup_window(cx, startup_open));
                    }
                })
                .detach();
            }

            #[cfg(not(target_os = "macos"))]
            open_startup_window(cx, preferences.startup_open);

            return;
        }

        for path in &input_paths {
            let absolute_path = if path.is_absolute() {
                path.clone()
            } else {
                match std::env::current_dir() {
                    Ok(cwd) => cwd.join(path),
                    Err(_) => path.clone(),
                }
            };

            let markdown = match std::fs::read_to_string(&absolute_path) {
                Ok(content) => {
                    if let Err(err) = config::record_recent_file(&absolute_path) {
                        eprintln!("failed to update recent file history: {err}");
                    }
                    content
                }
                Err(err) => {
                    eprintln!(
                        "failed to read '{}': {err}. opened as empty document.",
                        absolute_path.display()
                    );
                    String::new()
                }
            };
            open_editor_window(cx, markdown, Some(absolute_path));
        }
        app_menu::install_menus(cx);
        cx.refresh_windows();
    });
}

#[cfg(test)]
mod tests {
    use super::{AssetSource, VelotypeAssets};
    use crate::native_editor::commands::CommandCatalogue;

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
}

fn print_help() {
    println!(
        "velotype {} - A block-based Markdown editor",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("USAGE:");
    println!("    velotype [OPTIONS] [FILES...]");
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
    println!("    One or more markdown files to open. If no files are specified,");
    println!("    opens an empty document.");
}
