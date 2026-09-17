//! OWNER: WP5 (capture). System audio as a `types::AudioSource`, through a
//! Core Audio process tap (`objc2-core-audio` 0.3).
//!
//! - Resolve `AudioHardwareCreateProcessTap` / `AudioHardwareDestroyProcessTap`
//!   with `libc::dlsym`, never through the crate's `extern` declarations, so
//!   the binary still loads on macOS 12/13. `nm -um <binary> | grep ProcessTap`
//!   must print nothing.
//! - Global stereo tap, unmuted, private. Aggregate device with a real main
//!   sub-device, drift compensation and tap auto-start on.
//! - Rebuild when the default output device changes (`device_watch.rs`) and
//!   report it as `Discontinuity::FormatChanged`.
//! - The IOProc only forwards to the handler. Nothing else on that thread.
