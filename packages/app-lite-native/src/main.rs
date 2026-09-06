#[cfg(target_os = "macos")]
mod app;

fn main() {
    #[cfg(target_os = "macos")]
    app::run();

    #[cfg(not(target_os = "macos"))]
    eprintln!("joplin-lite-native is currently macOS-only");
}
