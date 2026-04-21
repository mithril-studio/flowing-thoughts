use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use rdev::{simulate, EventType, Key};

/// Save the current system clipboard contents (plain text).
/// Returns `None` if the clipboard is empty or pbpaste fails.
fn save_clipboard() -> Option<String> {
    let output = Command::new("pbpaste").output().ok()?;
    if output.status.success() && !output.stdout.is_empty() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

/// Restore the system clipboard to the given text via pbcopy.
fn restore_clipboard(text: &str) {
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
        }
        let _ = child.wait();
    }
}

/// Inject text into the focused app.
///
/// Strategy:
/// 1) save current clipboard
/// 2) copy text to system clipboard
/// 3) simulate Cmd+V paste
/// 4) restore original clipboard
///
/// This requires Accessibility permission for key simulation.
pub fn inject_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Skipping empty text injection".to_string());
    }

    // Save current clipboard before overwriting
    let original_clipboard = save_clipboard();

    let mut pbcopy = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start pbcopy: {e}"))?;

    let mut stdin = pbcopy
        .stdin
        .take()
        .ok_or_else(|| "Failed to get pbcopy stdin".to_string())?;
    stdin
        .write_all(text.as_bytes())
        .map_err(|e| format!("Failed to write text to pbcopy: {e}"))?;
    drop(stdin);

    let status = pbcopy
        .wait()
        .map_err(|e| format!("Failed waiting for pbcopy: {e}"))?;
    if !status.success() {
        // Restore clipboard even on failure
        if let Some(ref original) = original_clipboard {
            restore_clipboard(original);
        }
        return Err(format!("pbcopy exited unsuccessfully: {status}"));
    }

    simulate(&EventType::KeyPress(Key::MetaLeft))
        .map_err(|e| format!("Failed to press Meta key: {e:?}"))?;
    thread::sleep(Duration::from_millis(8));
    simulate(&EventType::KeyPress(Key::KeyV))
        .map_err(|e| format!("Failed to press V key: {e:?}"))?;
    thread::sleep(Duration::from_millis(8));
    simulate(&EventType::KeyRelease(Key::KeyV))
        .map_err(|e| format!("Failed to release V key: {e:?}"))?;
    thread::sleep(Duration::from_millis(8));
    simulate(&EventType::KeyRelease(Key::MetaLeft))
        .map_err(|e| format!("Failed to release Meta key: {e:?}"))?;

    // Wait for paste to complete, then restore original clipboard
    if let Some(ref original) = original_clipboard {
        thread::sleep(Duration::from_millis(150));
        restore_clipboard(original);
    }

    Ok(())
}
