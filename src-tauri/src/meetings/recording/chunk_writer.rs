//! OWNER: WP4 (recording). The real `types::SampleSink`.
//!
//! Must provide a sink that writes rolling 60 s chunk files:
//!
//! - `begin(anchor)` closes the open chunk and opens `<seq + 1>.pcm` anchored
//!   at that host time; a chunk reaching 60 s rotates the same way, with the
//!   anchor advanced by the frames written.
//! - Every open and close goes through a `types::ChunkLedger` (`open`, then
//!   `closed` with the final `n_frames`). `start_ms` is the anchor minus the
//!   meeting's `origin_host_ns`.
//! - f32 to s16 with clamping. `write()` to the file at least every second,
//!   `fsync` on close.
//!
//! Tests: rotation at 60 s, rotation on `begin`, ledger calls in order, bytes
//! on disk = 2 x `n_frames`.
