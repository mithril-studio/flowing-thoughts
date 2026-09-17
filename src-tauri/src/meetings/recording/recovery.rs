//! OWNER: WP4 (recording). Repairing chunks after a crash or a quit.
//!
//! Must provide a function from the `open` `types::ChunkRecord`s plus the
//! meetings root to the repaired records: status `recovered`,
//! `n_frames = file_len / 2` (a trailing odd byte is dropped; a missing file
//! is 0 frames). Pure apart from reading file sizes — it never touches the
//! DB. Launch recovery in `session.rs` (WP7) applies the result through the
//! store and queues the transcription job.
//!
//! Tests: a chunk left open by a simulated `_exit(0)` mid-write, a missing
//! file, an empty file.
