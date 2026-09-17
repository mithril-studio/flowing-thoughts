//! OWNER: WP4 (recording). Repairing chunks after a crash or a quit.
//!
//! Raw PCM has no header to finalize, so a chunk the app never closed is
//! repaired from its length alone: `n_frames = file_len / 2`. A trailing odd
//! byte (a write torn by the crash) is ignored, an empty or missing file is 0
//! frames. Nothing here touches the DB or changes a chunk file.
//!
//! Two entry points:
//! - `recover_chunks`: from the store's `open` rows to the repaired records.
//!   Launch recovery in `session.rs` (WP7) applies the result through the
//!   store and queues the transcription job.
//! - `recover_meeting_dir`: the same from the `track.json` sidecars alone, for
//!   when the database is gone or never saw the chunk. It also repairs the
//!   sidecars, so the reader can use them afterwards.

use std::fs;
use std::path::{Path, PathBuf};

use super::sidecar::{self, TrackSidecar};
use super::{BYTES_PER_FRAME, CHUNK_EXTENSION};
use crate::meetings::types::{ChunkRecord, ChunkStatus, TrackKind};

/// Whole frames in a chunk file. A missing file has none.
pub fn frames_on_disk(path: &Path) -> Result<u64, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len() / BYTES_PER_FRAME),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(format!("Failed to read the size of {}: {e}", path.display())),
    }
}

/// Repairs the chunks still marked `open`: status `recovered`, `n_frames`
/// from the file length. Records in any other status are left out. `root` is
/// the meetings audio root the records' paths are relative to.
pub fn recover_chunks(open: &[ChunkRecord], root: &Path) -> Result<Vec<ChunkRecord>, String> {
    open.iter()
        .filter(|record| record.status == ChunkStatus::Open)
        .map(|record| {
            Ok(ChunkRecord {
                status: ChunkStatus::Recovered,
                n_frames: frames_on_disk(&root.join(&record.path))?,
                ..record.clone()
            })
        })
        .collect()
}

/// A chunk file the sidecar does not list: its position on the timeline is
/// unknown, so it is reported and otherwise left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanChunk {
    pub file: String,
    pub n_frames: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTrack {
    pub track_dir: PathBuf,
    /// From the directory name; `None` for a directory that is not a track.
    pub kind: Option<TrackKind>,
    /// The sidecar after repair. `None` when it is missing or unreadable.
    pub sidecar: Option<TrackSidecar>,
    /// The chunks this call repaired (they were `open`), as records.
    pub recovered: Vec<ChunkRecord>,
    pub orphans: Vec<OrphanChunk>,
}

/// Walks `<meeting_dir>/<track>/`, repairs every chunk its sidecar still
/// lists as `open`, and rewrites the sidecar when anything changed. Running
/// it again is a no-op.
pub fn recover_meeting_dir(meeting_dir: &Path) -> Result<Vec<RecoveredTrack>, String> {
    let entries = match fs::read_dir(meeting_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Failed to read {}: {e}", meeting_dir.display())),
    };
    let mut track_dirs: Vec<PathBuf> =
        entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
    track_dirs.sort();
    track_dirs.iter().map(|dir| recover_track_dir(dir)).collect()
}

fn recover_track_dir(track_dir: &Path) -> Result<RecoveredTrack, String> {
    let kind = track_dir.file_name().and_then(|name| name.to_str()).and_then(TrackKind::parse);
    // A sidecar that does not parse is treated like a missing one: every
    // chunk file becomes an orphan, nothing is guessed.
    let mut sidecar = sidecar::read(track_dir).unwrap_or_else(|e| {
        super::log("ERROR", &format!("Meetings: recovery: {e}"));
        None
    });

    let mut repaired_ids = Vec::new();
    if let Some(sidecar) = &mut sidecar {
        for chunk in sidecar.chunks.iter_mut().filter(|c| c.status == ChunkStatus::Open) {
            chunk.n_frames = frames_on_disk(&track_dir.join(&chunk.file))?;
            chunk.status = ChunkStatus::Recovered;
            repaired_ids.push(chunk.id.clone());
        }
        if !repaired_ids.is_empty() {
            sidecar::write_atomic(track_dir, sidecar)?;
        }
    }
    let recovered = sidecar
        .as_ref()
        .map(|sidecar| {
            sidecar
                .chunk_records()
                .into_iter()
                .filter(|record| repaired_ids.contains(&record.id))
                .collect()
        })
        .unwrap_or_default();

    let mut orphans = Vec::new();
    let files = fs::read_dir(track_dir)
        .map_err(|e| format!("Failed to read {}: {e}", track_dir.display()))?;
    for path in files.flatten().map(|entry| entry.path()) {
        if path.extension().and_then(|e| e.to_str()) != Some(CHUNK_EXTENSION) {
            continue;
        }
        let Some(file) = path.file_name().and_then(|name| name.to_str()).map(str::to_string) else {
            continue;
        };
        let listed = sidecar.as_ref().is_some_and(|s| s.chunks.iter().any(|c| c.file == file));
        if !listed {
            orphans.push(OrphanChunk { n_frames: frames_on_disk(&path)?, file });
        }
    }
    orphans.sort_by(|a, b| a.file.cmp(&b.file));

    Ok(RecoveredTrack { track_dir: track_dir.to_path_buf(), kind, sidecar, recovered, orphans })
}

#[cfg(test)]
mod tests {
    use super::super::chunk_writer::test_support::FakeLedger;
    use super::super::test_support::TempDir;
    use super::super::{ChunkWriter, ChunkWriterConfig};
    use super::*;
    use crate::meetings::types::SampleSink;
    use std::io::Write;

    const ORIGIN: u64 = 77_000_000_000;

    /// Records two chunks, then "dies" mid-write of the third: no `finish`,
    /// no `Drop`, exactly like `_exit(0)` or `kill -9`.
    fn record_then_die(tmp: &TempDir, kind: TrackKind, frames_in_open_chunk: usize) -> FakeLedger {
        let ledger = FakeLedger::default();
        let mut config = ChunkWriterConfig::new(tmp.path().to_path_buf(), "m1", "t1", kind, ORIGIN);
        config.chunk_frames = 1_000;
        let mut writer = ChunkWriter::new(config, Box::new(ledger.clone())).unwrap();
        writer.begin(ORIGIN).unwrap();
        writer.write(&vec![0.3; 2_000 + frames_in_open_chunk]).unwrap();
        std::mem::forget(writer);
        ledger
    }

    #[test]
    fn a_kill_mid_chunk_recovers_the_right_frame_count() {
        let tmp = TempDir::new("recovery-kill");
        let ledger = record_then_die(&tmp, TrackKind::Mic, 123);
        let records = ledger.records();
        assert_eq!(records.len(), 3);
        assert_eq!(records[2].status, ChunkStatus::Open);
        assert_eq!(records[2].n_frames, 0, "the store never heard how long it got");

        // The crash tore the last write: one stray byte at the end.
        let open_path = tmp.path().join(&records[2].path);
        fs::OpenOptions::new().append(true).open(&open_path).unwrap().write_all(&[0x7f]).unwrap();
        assert_eq!(fs::metadata(&open_path).unwrap().len(), 247);

        let repaired = recover_chunks(&records, tmp.path()).unwrap();
        assert_eq!(repaired.len(), 1, "closed chunks are left alone");
        assert_eq!(repaired[0].id, records[2].id);
        assert_eq!(repaired[0].status, ChunkStatus::Recovered);
        assert_eq!(repaired[0].n_frames, 123);
        assert_eq!(repaired[0].start_ms, records[2].start_ms);
        assert_eq!(fs::metadata(&open_path).unwrap().len(), 247, "the file is not touched");
    }

    #[test]
    fn missing_and_empty_files_are_zero_frames() {
        let tmp = TempDir::new("recovery-empty");
        let ledger = record_then_die(&tmp, TrackKind::Mic, 0);
        let mut records = ledger.records();
        // Rotation is lazy, so dying right at the boundary leaves chunk 1
        // open and full. Open an empty third one by hand.
        assert_eq!(records.len(), 2);
        fs::write(tmp.path().join("m1/mic/000002.pcm"), b"").unwrap();
        records.push(ChunkRecord {
            id: "empty".into(),
            seq: 2,
            path: "m1/mic/000002.pcm".into(),
            status: ChunkStatus::Open,
            ..records[1].clone()
        });
        records.push(ChunkRecord {
            id: "missing".into(),
            seq: 3,
            path: "m1/mic/000003.pcm".into(),
            status: ChunkStatus::Open,
            ..records[1].clone()
        });

        let repaired = recover_chunks(&records, tmp.path()).unwrap();
        let frames: Vec<(&str, u64)> = repaired.iter().map(|r| (r.id.as_str(), r.n_frames)).collect();
        assert_eq!(frames, [(records[1].id.as_str(), 1_000), ("empty", 0), ("missing", 0)]);
        assert!(repaired.iter().all(|r| r.status == ChunkStatus::Recovered));
    }

    #[test]
    fn a_meeting_directory_recovers_without_the_database() {
        let tmp = TempDir::new("recovery-dir");
        record_then_die(&tmp, TrackKind::Mic, 400);
        record_then_die(&tmp, TrackKind::System, 1);
        // A chunk file the sidecar never heard of, with an odd length.
        fs::write(tmp.path().join("m1/system/000009.pcm"), [0u8; 11]).unwrap();
        let meeting_dir = tmp.path().join("m1");

        let tracks = recover_meeting_dir(&meeting_dir).unwrap();
        assert_eq!(tracks.iter().map(|t| t.kind).collect::<Vec<_>>(), [Some(TrackKind::Mic), Some(TrackKind::System)]);

        let mic = &tracks[0];
        assert_eq!(mic.recovered.len(), 1);
        assert_eq!(mic.recovered[0].n_frames, 400);
        assert_eq!(mic.recovered[0].status, ChunkStatus::Recovered);
        assert_eq!(mic.recovered[0].path, "m1/mic/000002.pcm");
        assert_eq!(mic.recovered[0].start_ms, 125, "2000 frames in: the anchor survived the crash");
        assert!(mic.orphans.is_empty());
        let statuses: Vec<ChunkStatus> =
            mic.sidecar.as_ref().unwrap().chunks.iter().map(|c| c.status).collect();
        assert_eq!(statuses, [ChunkStatus::Closed, ChunkStatus::Closed, ChunkStatus::Recovered]);

        let system = &tracks[1];
        assert_eq!(system.recovered[0].n_frames, 1);
        assert_eq!(system.orphans, [OrphanChunk { file: "000009.pcm".into(), n_frames: 5 }]);

        // The repair is on disk, and a second pass finds nothing left to do.
        let on_disk = sidecar::read(&meeting_dir.join("mic")).unwrap().unwrap();
        assert_eq!(Some(on_disk), mic.sidecar);
        let again = recover_meeting_dir(&meeting_dir).unwrap();
        assert!(again.iter().all(|t| t.recovered.is_empty()));
        assert_eq!(again[1].orphans.len(), 1);

        assert!(recover_meeting_dir(&tmp.path().join("no-such-meeting")).unwrap().is_empty());
    }

    #[test]
    fn a_torn_sidecar_reports_the_files_as_orphans() {
        let tmp = TempDir::new("recovery-torn");
        record_then_die(&tmp, TrackKind::Mic, 10);
        fs::write(tmp.path().join("m1/mic").join(sidecar::SIDECAR_FILE), b"{ torn").unwrap();
        let tracks = recover_meeting_dir(&tmp.path().join("m1")).unwrap();
        assert!(tracks[0].sidecar.is_none() && tracks[0].recovered.is_empty());
        let orphans: Vec<(&str, u64)> =
            tracks[0].orphans.iter().map(|o| (o.file.as_str(), o.n_frames)).collect();
        assert_eq!(orphans, [("000000.pcm", 1_000), ("000001.pcm", 1_000), ("000002.pcm", 10)]);
    }
}
