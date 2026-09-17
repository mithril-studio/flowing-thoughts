//! OWNER: WP4 (recording). Host-time bookkeeping for one track.
//!
//! Must provide the pure logic that compares each buffer's `host_time_ns`
//! with where the previous one ended:
//!
//! - A gap over 100 ms (a stall, a device rebuild, ring overflow, a pause)
//!   closes the chunk and starts a new anchor: `SampleSink::begin`.
//! - Anything smaller is drift and is tolerated; it stays bounded to a few
//!   ms per chunk because every chunk is re-anchored.
//! - Host time to meeting-timeline milliseconds, given the meeting's origin.
//!
//! Tests: contiguous buffers never re-anchor, a 250 ms hole does, jitter of a
//! few ms does not, overlap (a timestamp slightly in the past) does not.
