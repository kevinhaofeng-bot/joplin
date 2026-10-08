//! macOS launch arguments for an already verified private attachment copy.
//! File verification/materialization/lease ownership stays in NoteSession.
use std::path::Path;
use std::process::Command;

pub(crate) fn system_open_command(path: &Path) -> Command {
    let mut command = Command::new("/usr/bin/open");
    // A WAV default association can resolve to Music without opening a
    // playable document (actual302). Use the bundled viewer verified there;
    // this does not change LaunchServices associations or create a player.
    if path.extension().and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("wav"))
    {
        command.args(["-b", "com.apple.QuickTimePlayerX"]);
    }
    command.arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn args(command: &Command) -> Vec<OsString> {
        command.get_args().map(OsString::from).collect()
    }

    #[test]
    fn wav_307_uses_known_system_preview_not_music_association() {
        let path = Path::new("/var/tmp/owned-audio-307.wav");
        assert_eq!(args(&system_open_command(path)), vec![
            OsString::from("-b"), OsString::from("com.apple.QuickTimePlayerX"), path.as_os_str().to_owned(),
        ]);
    }

    #[test]
    fn uppercase_wav_307_has_the_same_preview_route() {
        let path = Path::new("/var/tmp/owned-audio-307.WAV");
        assert_eq!(args(&system_open_command(path)), vec![
            OsString::from("-b"), OsString::from("com.apple.QuickTimePlayerX"), path.as_os_str().to_owned(),
        ]);
    }

    #[test]
    fn non_wav_307_keeps_the_existing_default_open_chain() {
        for filename in ["owned.pdf", "owned.txt", "owned.mp4", "owned.png", "owned.bin", "owned"] {
            let path = Path::new("/var/tmp").join(filename);
            let command = system_open_command(&path);
            assert_eq!(command.get_program(), "/usr/bin/open");
            assert_eq!(args(&command), vec![path.as_os_str().to_owned()]);
        }
    }

    #[test]
    fn managed_path_307_is_one_argument_not_shell_text() {
        let path = Path::new("/var/tmp/owned name with $ and ;/file.txt");
        assert_eq!(args(&system_open_command(path)), vec![path.as_os_str().to_owned()]);
    }
}
