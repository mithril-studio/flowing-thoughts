//! OWNER: WP3 (long-form decoding). Nothing in the scaffold calls into this
//! file yet; WP6's worker is its only caller.
//!
//! WP3 also owns the structured seam in `local_transcribe.rs`
//! (`TranscriptSegment`, `decode_segments`, `speech_ranges`,
//! `detect_language`, `pub(crate) get_or_load_context`), added without
//! changing any existing signature or test.
//!
//! This file must provide, as pure and testable pieces:
//!
//! - Window planning: read a `types::TrackAudio` in 5-minute blocks, take the
//!   VAD speech ranges, pack them into contiguous windows of at most 28 s,
//!   break at gaps over 3 s. Output is plain data (`seq`, `start_ms`,
//!   `end_ms`) ready for `transcript_windows`. A property worth a test: the
//!   windows cover every speech range and never overlap.
//! - Window decoding: decode one window with `no_context(true)`, an explicit
//!   temperature fallback and an abort callback wired to
//!   `inference_gate::should_preempt()`. Report "aborted" distinctly from
//!   "failed": an aborted window stays `pending`. Put the decoder behind a
//!   small trait so WP6 can test job resume with a fake.
//! - Timestamp mapping: segment times map linearly from window-relative back
//!   to the meeting timeline.
//! - Language: `auto` detects once per track on the first 30 s of speech and
//!   only chooses between `nl` and `en`; no double decode. The language used
//!   is stored on each window row, which is what makes detection resumable.
//! - Flagging, never deleting: `no_speech`, `outside_vad`, `repeat`,
//!   `prompt_echo` (`types::SuppressedReason`). `echo` belongs to `echo.rs`.
//!
//! Meetings must not inherit dictation's filters: no five-word floor, no
//! injection guard, no `sanitize_transcript`.
//!
//! The model is always a Whisper one (`sanitize_settings` guarantees it);
//! Parakeet has no timestamps.
