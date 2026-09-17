//! OWNER: WP4 (recording). Native format to 16 kHz mono, at capture time.
//!
//! Must provide a streaming resampler on `rubato` 5: interleaved f32 at any
//! `types::SourceFormat` in, mono f32 at `types::TARGET_SAMPLE_RATE` out.
//!
//! - Downmix by averaging channels, then resample.
//! - A format change mid-meeting only reconfigures the resampler; the tail of
//!   the old one is flushed first so no audio is lost at the seam.
//! - 16 kHz mono input is passed through untouched.
//!
//! Tests: a synthetic sine keeps its frequency and (roughly) its amplitude at
//! 48 k and 44.1 k stereo; output length matches the ratio over a long run.
