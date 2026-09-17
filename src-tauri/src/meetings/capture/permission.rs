//! OWNER: WP5 (capture). System Audio Recording permission.
//!
//! The functions below are called by `commands.rs` and their signatures are
//! fixed. macOS reports a denial as silence, not as an error, so there are
//! two layers:
//!
//! 1. `check()`: best-effort `TCCAccessPreflight("kTCCServiceAudioCapture")`
//!    loaded with `dlopen`, behind the `private-tcc` cargo feature. A missing
//!    framework or symbol, or a build without the feature, is `unknown`.
//! 2. A watchdog on the system track: more than 20 s of exact zeros sets
//!    `RecordingStatus::system_audio_silent`. This module provides the
//!    detector; the session (WP7) owns the flag and the event.
//!
//! A denial never blocks a meeting: it continues mic-only.

use super::super::not_implemented;
use super::super::types::{PermissionState, PermissionStatus};

pub fn check() -> PermissionStatus {
    PermissionStatus {
        state: PermissionState::Unknown,
        detail: Some("System audio capture is not implemented yet (WP5 capture)".to_string()),
    }
}

/// Opens System Settings → Privacy & Security → Screen & System Audio
/// Recording.
pub fn open_settings() -> Result<(), String> {
    not_implemented("Opening the system audio settings", "WP5 capture")
}
