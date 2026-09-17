//! OWNER: WP5 (capture). The microphone as a `types::AudioSource`.
//!
//! - `cpal` 0.15, the same input-device choice as dictation
//!   (`settings.microphone.input_device`). Do not upgrade cpal.
//! - `cpal::Stream` is `!Send`: park it on a thread of its own.
//! - Runs next to a dictation capture in `audio.rs`; the two must not
//!   disturb each other.
//! - Stamp frames with mach host time from the callback's capture timestamp,
//!   on the same clock as the system tap.
