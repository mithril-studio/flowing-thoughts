//! OWNER: WP4 (recording). `track.json`: the chunk list next to the chunk
//! files, so a track can be recovered and read even without the database.
//!
//! The chunk writer rewrites it on every chunk open and close. Every rewrite
//! is atomic (temp file, fsync, rename), so a crash leaves either the old or
//! the new list, never half of one. A chunk still marked `open` in it is one
//! the app never got to close: `recovery` repairs it from the file length.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::meetings::types::{ChunkRecord, ChunkStatus, TrackKind, TARGET_SAMPLE_RATE};

pub const SIDECAR_FILE: &str = "track.json";
const SIDECAR_TMP_FILE: &str = "track.json.tmp";
pub const SIDECAR_VERSION: u32 = 1;
pub const ENCODING_S16LE: &str = "s16le";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackSidecar {
    pub version: u32,
    pub meeting_id: String,
    pub track_id: String,
    pub track: TrackKind,
    /// Format of every chunk file: `s16le`, 16 000 Hz, 1 channel.
    pub encoding: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Mach host time of the meeting's origin. Host time restarts at boot, so
    /// it is only meaningful relative to the chunks' anchors.
    pub origin_host_ns: u64,
    /// RFC3339, when the track started recording.
    pub created_at: String,
    pub chunks: Vec<SidecarChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarChunk {
    pub id: String,
    pub seq: u32,
    /// File name inside the track directory: `000042.pcm`.
    pub file: String,
    pub status: ChunkStatus,
    pub anchor_host_ns: u64,
    /// Position of the first frame on the meeting timeline.
    pub start_ms: u64,
    /// Silence between the previous chunk's end and this chunk's start. 0 for
    /// a plain 60 s rotation.
    pub gap_before_ms: u64,
    /// Final once the chunk is `closed` or `recovered`; 0 while `open`.
    pub n_frames: u64,
}

impl TrackSidecar {
    pub fn new(meeting_id: &str, track_id: &str, track: TrackKind, origin_host_ns: u64) -> Self {
        Self {
            version: SIDECAR_VERSION,
            meeting_id: meeting_id.to_string(),
            track_id: track_id.to_string(),
            track,
            encoding: ENCODING_S16LE.to_string(),
            sample_rate: TARGET_SAMPLE_RATE,
            channels: 1,
            origin_host_ns,
            created_at: chrono::Utc::now().to_rfc3339(),
            chunks: Vec::new(),
        }
    }

    /// The chunks as the rows the store would hold, for reading a track
    /// without the database.
    pub fn chunk_records(&self) -> Vec<ChunkRecord> {
        self.chunks
            .iter()
            .map(|chunk| ChunkRecord {
                id: chunk.id.clone(),
                track_id: self.track_id.clone(),
                seq: chunk.seq,
                path: format!("{}/{}/{}", self.meeting_id, self.track.as_str(), chunk.file),
                status: chunk.status,
                anchor_host_ns: chunk.anchor_host_ns,
                start_ms: chunk.start_ms,
                n_frames: chunk.n_frames,
            })
            .collect()
    }
}

/// Replaces `<track_dir>/track.json` atomically.
pub fn write_atomic(track_dir: &Path, sidecar: &TrackSidecar) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(sidecar)
        .map_err(|e| format!("Failed to serialize {SIDECAR_FILE}: {e}"))?;
    let tmp = track_dir.join(SIDECAR_TMP_FILE);
    let mut file =
        fs::File::create(&tmp).map_err(|e| format!("Failed to create {SIDECAR_TMP_FILE}: {e}"))?;
    file.write_all(&json).map_err(|e| format!("Failed to write {SIDECAR_TMP_FILE}: {e}"))?;
    file.sync_all().map_err(|e| format!("Failed to sync {SIDECAR_TMP_FILE}: {e}"))?;
    drop(file);
    fs::rename(&tmp, track_dir.join(SIDECAR_FILE))
        .map_err(|e| format!("Failed to replace {SIDECAR_FILE}: {e}"))?;
    // Make the rename itself durable. Best effort: the data is safe either way.
    if let Ok(dir) = fs::File::open(track_dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// `None` when the track has no sidecar. A sidecar that does not parse is an
/// error: the caller decides whether to fall back to the bare files.
pub fn read(track_dir: &Path) -> Result<Option<TrackSidecar>, String> {
    let bytes = match fs::read(track_dir.join(SIDECAR_FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Failed to read {SIDECAR_FILE}: {e}")),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("Failed to parse {SIDECAR_FILE}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::TempDir;
    use super::*;

    fn sample() -> TrackSidecar {
        let mut sidecar = TrackSidecar::new("m1", "t1", TrackKind::System, u64::MAX - 5);
        sidecar.chunks.push(SidecarChunk {
            id: "c0".into(),
            seq: 0,
            file: "000000.pcm".into(),
            status: ChunkStatus::Closed,
            anchor_host_ns: u64::MAX - 3,
            start_ms: 0,
            gap_before_ms: 0,
            n_frames: 960_000,
        });
        sidecar
    }

    #[test]
    fn round_trips_and_replaces_atomically() {
        let tmp = TempDir::new("sidecar");
        assert_eq!(read(tmp.path()).unwrap(), None);

        let mut sidecar = sample();
        write_atomic(tmp.path(), &sidecar).unwrap();
        // Host times are full-range u64: they must survive JSON exactly.
        assert_eq!(read(tmp.path()).unwrap(), Some(sidecar.clone()));

        sidecar.chunks[0].status = ChunkStatus::Recovered;
        write_atomic(tmp.path(), &sidecar).unwrap();
        assert_eq!(read(tmp.path()).unwrap(), Some(sidecar));
        assert!(!tmp.path().join(SIDECAR_TMP_FILE).exists());

        let json = fs::read_to_string(tmp.path().join(SIDECAR_FILE)).unwrap();
        assert!(json.contains("\"encoding\": \"s16le\"") && json.contains("\"track\": \"system\""));
    }

    #[test]
    fn a_torn_sidecar_is_an_error_not_a_panic() {
        let tmp = TempDir::new("sidecar-torn");
        fs::write(tmp.path().join(SIDECAR_FILE), b"{\"version\": 1, \"meet").unwrap();
        assert!(read(tmp.path()).unwrap_err().contains("parse"));
    }

    #[test]
    fn chunk_records_use_the_root_relative_path() {
        let records = sample().chunk_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, "m1/system/000000.pcm");
        assert_eq!(records[0].track_id, "t1");
        assert_eq!(records[0].n_frames, 960_000);
    }
}
