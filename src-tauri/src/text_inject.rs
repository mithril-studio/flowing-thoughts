use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

fn save_clipboard() -> Option<String> {
    let output = Command::new("pbpaste").output().ok()?;
    if output.status.success() && !output.stdout.is_empty() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

fn restore_clipboard(text: &str) {
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
        }
        let _ = child.wait();
    }
}

pub fn write_clipboard(text: &str) -> Result<(), String> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start pbcopy: {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Failed to get pbcopy stdin".to_string())?;
    stdin
        .write_all(text.as_bytes())
        .map_err(|e| format!("Failed to write text to pbcopy: {e}"))?;
    drop(stdin);
    let status = child
        .wait()
        .map_err(|e| format!("Failed waiting for pbcopy: {e}"))?;
    if !status.success() {
        return Err(format!("pbcopy exited unsuccessfully: {status}"));
    }
    Ok(())
}

/// Ask the system (via AppleScript / System Events) to perform Cmd+V in the
/// currently focused app. This sets modifier flags atomically, so it is much
/// more reliable than simulating raw key events.
fn applescript_paste() -> Result<(), String> {
    let script = r#"tell application "System Events" to keystroke "v" using command down"#;
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("Failed to run paste AppleScript: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "Paste failed. Make sure FlowingThoughts has Accessibility permission. Details: {stderr}"
        ));
    }
    Ok(())
}

/// Inject `text` into the currently focused app.
///
/// Flow:
/// 1. Remember the existing clipboard so we can restore it afterwards.
/// 2. Place `text` on the clipboard.
/// 3. Wait briefly so any modifier keys the user was still holding
///    (e.g. Fn / Shift / Cmd from the recording hotkey) have time to clear.
/// 4. Fire Cmd+V via AppleScript — this sets modifier flags atomically,
///    so it ignores whatever keys are physically held.
/// 5. Restore the original clipboard.
pub fn inject_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Skipping empty text injection".to_string());
    }

    let original_clipboard = save_clipboard();

    if let Err(e) = write_clipboard(text) {
        if let Some(ref original) = original_clipboard {
            restore_clipboard(original);
        }
        return Err(e);
    }

    // Give the OS a beat to register that the hotkey has been released
    // and let the clipboard settle before the paste fires.
    thread::sleep(Duration::from_millis(80));

    let paste_result = applescript_paste();

    // Always try to restore the clipboard, even on failure.
    thread::sleep(Duration::from_millis(150));
    if let Some(ref original) = original_clipboard {
        restore_clipboard(original);
    }

    paste_result
}
