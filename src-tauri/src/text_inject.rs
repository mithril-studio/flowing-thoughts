use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use rdev::{simulate, EventType, Key};

/// Inject text into the focused app.
///
/// Current strategy:
/// 1) copy text to system clipboard
/// 2) simulate Cmd+V paste
///
/// This requires Accessibility permission for key simulation.
pub fn inject_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Skipping empty text injection".to_string());
    }

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

    Ok(())
}
