//! OWNER: WP5 (capture). The real `types::AudioSource`s.
//!
//! The functions below are called by `commands.rs` and their signatures are
//! fixed. Beyond them this module must provide what the session (WP7) needs
//! to build a meeting's sources, e.g. `open_mic()` and `open_system_tap()`,
//! each returning a `Box<dyn AudioSource>`; a failing system tap is an `Err`
//! the session turns into a mic-only meeting, never a blocked one.
//!
//! Follow the spike (`docs/spikes/TAP_CAPTURE_SPIKE.md` on branch
//! `spike/tap-capture`) for what works on real hardware.

// Scaffold: remove once WP5 implements this module.
#![allow(dead_code)]

pub mod device_watch;
pub mod mic;
pub mod permission;
pub mod system_tap;

/// The runtime gate for the whole feature: macOS 14.4+, the
/// `CATapDescription` class present and both tap symbols resolvable.
/// Dictation never depends on it.
pub fn is_supported() -> bool {
    false
}
