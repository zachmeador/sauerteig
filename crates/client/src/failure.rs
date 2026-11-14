//! Startup failures. A player who opened the game from a menu or a shortcut has
//! no terminal, so a failure is also shown in a native dialog and written to a
//! log file in the data folder, as well as printed to stderr.
use std::path::{Path, PathBuf};

const TITLE: &str = "Sauerteig could not start";
const LOG_NAME: &str = "sauerteig.log";
/// The log restarts from empty once it reaches this size.
const LOG_LIMIT: u64 = 256 * 1024;

/// Prints and logs the failure, then shows it to the player. Call it before the
/// event loop starts or from inside one of its callbacks: on macOS, winit's run
/// loop observer panics on a dialog shown after the loop has ended.
pub fn report(error: &str) {
    let log = record(error);
    let mut message = error.to_string();
    if let Some(path) = &log {
        message.push_str(&format!("\n\nThe details are also in {}", path.display()));
    }
    show_dialog(TITLE, &message);
}

/// Prints and logs the failure without a dialog. Returns the log file's path
/// when one could be written.
pub fn record(error: &str) -> Option<PathBuf> {
    log::error!("{error}");
    eprintln!("Sauerteig could not start: {error}");
    let directory = match crate::user_data::data_directory() {
        Ok(directory) => directory,
        Err(problem) => {
            eprintln!("The failure could not be written to a log file: {problem}");
            return None;
        }
    };
    match write_log(&directory, error) {
        Ok(path) => Some(path),
        Err(problem) => {
            eprintln!("The failure could not be written to a log file: {problem}");
            None
        }
    }
}

/// Shows a problem that has no other way to reach the player, such as a final
/// save that failed while the application was already quitting.
pub fn show_problem(title: &str, message: &str) {
    show_dialog(title, message);
}

fn write_log(directory: &Path, error: &str) -> Result<PathBuf, String> {
    use std::{fs, io::Write, time::SystemTime};
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let path = directory.join(LOG_NAME);
    let oversized = fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= LOG_LIMIT);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(!oversized)
        .write(true)
        .truncate(oversized)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    writeln!(
        file,
        "[unix time {seconds}] Sauerteig {} could not start: {error}",
        env!("CARGO_PKG_VERSION")
    )
    .and_then(|()| file.sync_all())
    .map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(windows)]
fn show_dialog(title: &str, message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
    let (title, message) = (wide(title), wide(message));
    // SAFETY: both buffers are NUL-terminated UTF-16 that outlive the call,
    // and `MessageBoxW` keeps neither pointer. A null (zero) owner window is allowed
    // and gives a dialog without a parent.
    unsafe {
        MessageBoxW(0, message.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(target_os = "macos")]
fn show_dialog(title: &str, message: &str) {
    use objc2_app_kit::{NSAlert, NSApplication};
    use objc2_foundation::{MainThreadMarker, NSString};
    let Some(main_thread) = MainThreadMarker::new() else {
        eprintln!("No dialog was shown: the failure was not reported from the main thread.");
        return;
    };
    // Needed so an alert shown before the event loop runs gets a connection to
    // the window server.
    let _application = NSApplication::sharedApplication(main_thread);
    // SAFETY: These are plain AppKit calls on the main thread, which
    // `main_thread` proves. The strings are retained objects that stay valid
    // for each call, and `NSAlert` copies them.
    unsafe {
        let alert = NSAlert::new(main_thread);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn show_dialog(title: &str, message: &str) {
    use std::process::{Command, Stdio};
    // The first helper that is installed and succeeds wins. Neither installed
    // is normal, and stderr and the log file still hold the failure.
    // zenity reads its text as markup.
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let attempts: [(&str, Vec<&str>); 2] = [
        (
            "zenity",
            vec!["--error", "--title", title, "--text", &escaped],
        ),
        ("kdialog", vec!["--title", title, "--error", message]),
    ];
    for (program, arguments) in attempts {
        let shown = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if shown.is_ok_and(|status| status.success()) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_names_the_error_and_keeps_earlier_failures() {
        let directory =
            std::env::temp_dir().join(format!("sauerteig-failure-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let path = write_log(&directory, "No graphics adapter").unwrap();
        write_log(&directory, "Second failure").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("No graphics adapter") && text.contains("Second failure"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_oversized_log_starts_again() {
        let directory =
            std::env::temp_dir().join(format!("sauerteig-failure-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(LOG_NAME), vec![b'x'; LOG_LIMIT as usize]).unwrap();
        let path = write_log(&directory, "Fresh failure").unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("Fresh failure"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
