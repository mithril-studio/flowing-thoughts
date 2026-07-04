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

/// Post a synthetic Cmd+V through CoreGraphics. Unlike the AppleScript route
/// this only needs the Accessibility permission we already hold for the
/// hotkey tap — no separate Automation ("control System Events") grant, which
/// is what silently broke injection in bundled builds. Setting the flags on
/// the synthetic events also overrides whatever keys (Fn, Shift) the user is
/// still physically holding from the recording hotkey.
#[cfg(target_os = "macos")]
fn cgevent_paste() -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    const KC_V: u16 = 9; // kVK_ANSI_V

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "Failed to create CGEventSource".to_string())?;

    let key_down = CGEvent::new_keyboard_event(source.clone(), KC_V, true)
        .map_err(|_| "Failed to create paste key-down event".to_string())?;
    key_down.set_flags(CGEventFlags::CGEventFlagCommand);
    key_down.post(CGEventTapLocation::HID);

    thread::sleep(Duration::from_millis(15));

    let key_up = CGEvent::new_keyboard_event(source, KC_V, false)
        .map_err(|_| "Failed to create paste key-up event".to_string())?;
    key_up.set_flags(CGEventFlags::CGEventFlagCommand);
    key_up.post(CGEventTapLocation::HID);

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn cgevent_paste() -> Result<(), String> {
    Err("Synthetic paste is only supported on macOS".to_string())
}

/// AppleScript fallback. Requires both Accessibility and the Automation
/// permission for System Events, so it is only used when the CGEvent path
/// fails.
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
/// 1. Fail fast with an actionable message if Accessibility is missing —
///    the text is left on the clipboard so the user can paste manually.
/// 2. Remember the existing clipboard so we can restore it afterwards.
/// 3. Place `text` on the clipboard, give the paste target a beat.
/// 4. Fire Cmd+V via CGEvent (AppleScript as fallback).
/// 5. Restore the original clipboard after the target has read it — but only
///    when the paste succeeded, so a failed injection leaves the dictation
///    on the clipboard instead of throwing it away.
pub fn inject_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Skipping empty text injection".to_string());
    }

    #[cfg(target_os = "macos")]
    if !crate::macos_ax::is_process_trusted(false) {
        let _ = write_clipboard(text);
        return Err(
            "FlowingThoughts can't type into other apps without Accessibility permission. \
             Your dictation is on the clipboard — press ⌘V to paste it. \
             Enable FlowingThoughts in System Settings → Privacy & Security → Accessibility."
                .to_string(),
        );
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
    thread::sleep(Duration::from_millis(120));

    let paste_result = cgevent_paste().or_else(|_| applescript_paste());

    match paste_result {
        Ok(()) => {
            // Let the target app read the clipboard before restoring it —
            // restoring too early makes the paste land the *old* clipboard.
            thread::sleep(Duration::from_millis(350));
            if let Some(ref original) = original_clipboard {
                restore_clipboard(original);
            }
            Ok(())
        }
        Err(e) => {
            // Keep the dictation on the clipboard as a manual fallback.
            Err(format!("{e} Your dictation is on the clipboard — press ⌘V to paste it."))
        }
    }
}
