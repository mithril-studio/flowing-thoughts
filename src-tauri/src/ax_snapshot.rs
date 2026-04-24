use std::process::Command;

/// AppleScript: read the `AXValue` of the focused UI element in the frontmost
/// app. Wrapped in a `try` block so any failure (no focus, non-AX app,
/// permission denied) yields an empty string rather than an AppleScript error.
const READ_SCRIPT: &str = r#"try
    tell application "System Events"
        set frontProc to first application process whose frontmost is true
        set focused to value of attribute "AXFocusedUIElement" of frontProc
        return value of attribute "AXValue" of focused
    end tell
on error
    return ""
end try"#;

/// Read the text contents of the currently focused text field, if the
/// frontmost app exposes it via Accessibility.
///
/// Returns `None` when the frontmost app isn't AX-compliant (most Electron
/// apps: Slack, VS Code), no text field is focused, or permission is missing.
/// Callers should treat `None` as "we can't see what the user sees" and fall
/// back to the Home-page edit flow.
pub fn read_focused_text_value() -> Option<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(READ_SCRIPT)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim_end_matches('\n');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
