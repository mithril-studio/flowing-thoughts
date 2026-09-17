//! OWNER: WP4 (recording). From captured frames to chunk files on disk.
//! Nothing in the scaffold calls into this module yet; WP5 feeds it and WP7
//! drives it.
//!
//! Data flow per track:
//!
//! ```text
//! AudioSource --on_frames--> rtrb ring (~5 s) --writer thread--> resample
//!   --> timeline --> SampleSink (chunk_writer) --> <seq>.pcm + ChunkLedger
//! ```
//!
//! This module must provide:
//!
//! - A recorder per track that implements `types::AudioSourceHandler`. Its
//!   `on_frames` only copies into the ring; overflow is counted, never
//!   blocked on, and recorded as a gap.
//! - `pause` / `resume` (paused audio is dropped and shows up as a gap) and a
//!   `stop` that drains the ring and finishes the sink.
//! - `meetings_root()`: `~/Library/Application Support/FlowingThoughts/meetings`,
//!   and the layout under it, `<meeting_id>/<track>/<seq>.pcm`, where
//!   `<track>` is `TrackKind::as_str()`. Chunk rows store the path relative
//!   to the root.
//! - `delete_meeting_audio(meeting_id)` and `audio_bytes(meeting_id)` for the
//!   session (WP7) and the store's DTOs.
//! - A `types::TrackAudio` implementation over a track's `ChunkRecord`s, for
//!   long-form decoding. Gaps and deleted chunks read as silence.
//!
//! Format: raw `s16le`, 16 kHz (`types::TARGET_SAMPLE_RATE`), mono. No header
//! to finalize. The writer calls `write()` at least once a second and
//! `fsync`s when a chunk closes, because the app exits through `_exit(0)`.
//!
//! Tests need no hardware: fake `AudioSource`, fake `SampleSink`, fake
//! `ChunkLedger`, a temp directory.

// Nothing calls into this module until WP5 (capture) and WP7 (session) land.
// Remove once they do.
#![allow(dead_code)]

pub mod chunk_writer;
pub mod recovery;
pub mod resample;
pub mod timeline;
