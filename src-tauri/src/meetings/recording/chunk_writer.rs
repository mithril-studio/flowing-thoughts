//! OWNER: WP4 (recording). The real `types::SampleSink`: rolling 60 s chunk
//! files of raw `s16le` 16 kHz mono at `<root>/<meeting_id>/<track>/<seq:06>.pcm`.
//!
//! - `begin(anchor)` closes the open chunk and opens `<seq + 1>.pcm` anchored
//!   at that host time. The recorder calls it for every new run and, with a
//!   drift-corrected anchor, exactly when a chunk reaches 60 s. A chunk that
//!   fills up without a `begin` rotates by itself, with the anchor advanced by
//!   the frames written, so a file never grows past 60 s.
//! - Every open and close goes through a `types::ChunkLedger` (`open`, then
//!   `closed` with the final `n_frames`) and into the `track.json` sidecar.
//!   `start_ms` is the anchor minus the meeting's `origin_host_ns`. A failing
//!   ledger is logged and does not stop the audio: the files and the sidecar
//!   are enough to recover from.
//! - f32 to s16 with clamping. Every `write` goes straight to the file with a
//!   plain `write_all`: no `BufWriter`, nothing held back in user space, so
//!   `_exit(0)` or a crash loses nothing that was handed to the sink. The
//!   recorder calls `write` many times a second. `fsync` on close.
//!
//! Runs on the writer thread. File errors (disk full) come back as `Err` and
//! become the recorder's error state; they never panic.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use super::sidecar::{self, SidecarChunk, TrackSidecar};
use super::timeline::{frames_to_ns, gap_between_ms, host_to_timeline_ms};
use super::{chunk_file_name, chunk_rel_path, log, track_dir, CHUNK_FRAMES};
use crate::meetings::types::{
    ChunkLedger, ChunkRecord, ChunkStatus, SampleSink, TrackKind, TARGET_SAMPLE_RATE,
};

#[derive(Debug, Clone)]
pub struct ChunkWriterConfig {
    /// The meetings audio root: `recording::meetings_root()` outside tests.
    pub root: PathBuf,
    pub meeting_id: String,
    pub track_id: String,
    pub kind: TrackKind,
    /// Host time of the meeting's origin: timeline position 0.
    pub origin_host_ns: u64,
    /// Frames per chunk file. `CHUNK_FRAMES` (60 s); tests use less.
    pub chunk_frames: u64,
}

impl ChunkWriterConfig {
    pub fn new(
        root: PathBuf,
        meeting_id: &str,
        track_id: &str,
        kind: TrackKind,
        origin_host_ns: u64,
    ) -> Self {
        Self {
            root,
            meeting_id: meeting_id.to_string(),
            track_id: track_id.to_string(),
            kind,
            origin_host_ns,
            chunk_frames: CHUNK_FRAMES,
        }
    }
}

struct OpenChunk {
    record: ChunkRecord,
    file: File,
    frames: u64,
}

pub struct ChunkWriter {
    config: ChunkWriterConfig,
    dir: PathBuf,
    ledger: Box<dyn ChunkLedger>,
    sidecar: TrackSidecar,
    open: Option<OpenChunk>,
    next_seq: u32,
    /// Reused s16le conversion buffer.
    bytes: Vec<u8>,
}

pub fn f32_to_s16(sample: f32) -> i16 {
    // NaN casts to 0.
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

impl ChunkWriter {
    /// Creates the track directory and an empty sidecar. Refuses a track that
    /// already has one: recorded audio is never overwritten.
    pub fn new(config: ChunkWriterConfig, ledger: Box<dyn ChunkLedger>) -> Result<Self, String> {
        if config.chunk_frames == 0 {
            return Err("Chunk size must be at least one frame".to_string());
        }
        let dir = track_dir(&config.root, &config.meeting_id, config.kind)?;
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create track directory: {e}"))?;
        if sidecar::read(&dir)?.is_some() {
            return Err(format!(
                "Track '{}' of meeting {} was already recorded",
                config.kind.as_str(),
                config.meeting_id
            ));
        }
        let sidecar = TrackSidecar::new(
            &config.meeting_id,
            &config.track_id,
            config.kind,
            config.origin_host_ns,
        );
        sidecar::write_atomic(&dir, &sidecar)?;
        Ok(Self {
            config,
            dir,
            ledger,
            sidecar,
            open: None,
            next_seq: 0,
            bytes: Vec::new(),
        })
    }

    /// Frames in the chunk being written, 0 when none is open.
    #[cfg(test)]
    pub fn open_chunk_frames(&self) -> u64 {
        self.open.as_ref().map(|chunk| chunk.frames).unwrap_or(0)
    }

    fn open_chunk(&mut self, anchor_host_ns: u64) -> Result<(), String> {
        let seq = self.next_seq;
        let path = self.dir.join(chunk_file_name(seq));
        // create_new: a file that is somehow already there is someone's audio.
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("Failed to create chunk file {}: {e}", chunk_file_name(seq)))?;
        self.next_seq += 1;

        let start_ms = host_to_timeline_ms(self.config.origin_host_ns, anchor_host_ns);
        let record = ChunkRecord {
            id: uuid::Uuid::new_v4().to_string(),
            track_id: self.config.track_id.clone(),
            seq,
            path: chunk_rel_path(&self.config.meeting_id, self.config.kind, seq),
            status: ChunkStatus::Open,
            anchor_host_ns,
            start_ms,
            n_frames: 0,
        };
        let gap_before_ms = self
            .sidecar
            .chunks
            .last()
            .map(|prev| {
                gap_between_ms(
                    prev.anchor_host_ns,
                    prev.n_frames,
                    TARGET_SAMPLE_RATE,
                    anchor_host_ns,
                )
            })
            .unwrap_or(0);
        self.sidecar.chunks.push(SidecarChunk {
            id: record.id.clone(),
            seq,
            file: chunk_file_name(seq),
            status: ChunkStatus::Open,
            anchor_host_ns,
            start_ms,
            gap_before_ms,
            n_frames: 0,
        });
        // From here on the file exists, so the chunk counts as open even if
        // the bookkeeping below fails.
        let sidecar_result = sidecar::write_atomic(&self.dir, &self.sidecar);
        if let Err(e) = self.ledger.chunk_opened(&record) {
            log(
                "ERROR",
                &format!("Meetings: chunk ledger failed on open (seq {seq}): {e}"),
            );
        }
        self.open = Some(OpenChunk {
            record,
            file,
            frames: 0,
        });
        sidecar_result
    }

    fn close_chunk(&mut self) -> Result<(), String> {
        let Some(chunk) = self.open.take() else {
            return Ok(());
        };
        let sync_result = chunk.file.sync_all().map_err(|e| {
            format!(
                "Failed to sync chunk file {}: {e}",
                chunk_file_name(chunk.record.seq)
            )
        });
        drop(chunk.file);

        if let Some(entry) = self
            .sidecar
            .chunks
            .iter_mut()
            .find(|c| c.id == chunk.record.id)
        {
            entry.status = ChunkStatus::Closed;
            entry.n_frames = chunk.frames;
        }
        let sidecar_result = sidecar::write_atomic(&self.dir, &self.sidecar);
        if let Err(e) = self.ledger.chunk_closed(&chunk.record.id, chunk.frames) {
            log(
                "ERROR",
                &format!(
                    "Meetings: chunk ledger failed on close (seq {}): {e}",
                    chunk.record.seq
                ),
            );
        }
        sync_result.and(sidecar_result)
    }
}

impl SampleSink for ChunkWriter {
    fn begin(&mut self, anchor_host_ns: u64) -> Result<(), String> {
        self.close_chunk()?;
        self.open_chunk(anchor_host_ns)
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        let mut rest = samples;
        while !rest.is_empty() {
            let Some(chunk) = &self.open else {
                return Err("Chunk writer: write before begin".to_string());
            };
            let room = self.config.chunk_frames - chunk.frames;
            if room == 0 {
                // Full without a `begin`: rotate, anchored by sample count.
                let anchor =
                    chunk.record.anchor_host_ns + frames_to_ns(chunk.frames, TARGET_SAMPLE_RATE);
                self.begin(anchor)?;
                continue;
            }
            let take = (room.min(rest.len() as u64)) as usize;
            self.bytes.clear();
            self.bytes.extend(
                rest[..take]
                    .iter()
                    .flat_map(|s| f32_to_s16(*s).to_le_bytes()),
            );
            let chunk = self.open.as_mut().expect("checked above");
            chunk.file.write_all(&self.bytes).map_err(|e| {
                format!(
                    "Failed to write chunk file {}: {e}",
                    chunk_file_name(chunk.record.seq)
                )
            })?;
            chunk.frames += take as u64;
            rest = &rest[take..];
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        self.close_chunk()
    }
}

impl Drop for ChunkWriter {
    fn drop(&mut self) {
        let _ = self.close_chunk();
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use crate::meetings::types::{ChunkLedger, ChunkRecord};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum LedgerCall {
        Opened(ChunkRecord),
        Closed { id: String, n_frames: u64 },
    }

    /// A `ChunkLedger` that remembers its calls; clones share the list.
    #[derive(Clone, Default)]
    pub struct FakeLedger {
        pub calls: Arc<Mutex<Vec<LedgerCall>>>,
        pub fail: bool,
    }

    impl FakeLedger {
        pub fn calls(&self) -> Vec<LedgerCall> {
            self.calls.lock().unwrap().clone()
        }

        pub fn opened(&self) -> Vec<ChunkRecord> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    LedgerCall::Opened(record) => Some(record),
                    LedgerCall::Closed { .. } => None,
                })
                .collect()
        }

        /// The records as the store would hold them after the calls so far.
        pub fn records(&self) -> Vec<ChunkRecord> {
            let mut records = self.opened();
            for call in self.calls() {
                if let LedgerCall::Closed { id, n_frames } = call {
                    let record = records.iter_mut().find(|r| r.id == id).unwrap();
                    record.status = crate::meetings::types::ChunkStatus::Closed;
                    record.n_frames = n_frames;
                }
            }
            records
        }
    }

    impl ChunkLedger for FakeLedger {
        fn chunk_opened(&mut self, chunk: &ChunkRecord) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(LedgerCall::Opened(chunk.clone()));
            if self.fail {
                Err("ledger down".into())
            } else {
                Ok(())
            }
        }

        fn chunk_closed(&mut self, chunk_id: &str, n_frames: u64) -> Result<(), String> {
            self.calls.lock().unwrap().push(LedgerCall::Closed {
                id: chunk_id.to_string(),
                n_frames,
            });
            if self.fail {
                Err("ledger down".into())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::TempDir;
    use super::test_support::{FakeLedger, LedgerCall};
    use super::*;

    const ORIGIN: u64 = 1_000_000_000_000;
    const SECOND: u64 = 1_000_000_000;

    fn writer(tmp: &TempDir, chunk_frames: u64) -> (ChunkWriter, FakeLedger) {
        let ledger = FakeLedger::default();
        let mut config =
            ChunkWriterConfig::new(tmp.path().to_path_buf(), "m1", "t1", TrackKind::Mic, ORIGIN);
        config.chunk_frames = chunk_frames;
        (
            ChunkWriter::new(config, Box::new(ledger.clone())).unwrap(),
            ledger,
        )
    }

    fn file_len(tmp: &TempDir, seq: u32) -> u64 {
        fs::metadata(tmp.path().join("m1").join("mic").join(chunk_file_name(seq)))
            .unwrap()
            .len()
    }

    #[test]
    fn converts_with_clamping() {
        assert_eq!(f32_to_s16(0.0), 0);
        assert_eq!(f32_to_s16(1.0), i16::MAX);
        assert_eq!(f32_to_s16(-1.0), -i16::MAX);
        assert_eq!(f32_to_s16(7.5), i16::MAX);
        assert_eq!(f32_to_s16(-7.5), -i16::MAX);
        assert_eq!(f32_to_s16(0.5), 16_384);
        assert_eq!(f32_to_s16(f32::NAN), 0);
    }

    #[test]
    fn rotates_at_sixty_seconds_with_the_anchor_advanced() {
        let tmp = TempDir::new("chunks-60s");
        let (mut writer, ledger) = writer(&tmp, CHUNK_FRAMES);
        writer.begin(ORIGIN + 2 * SECOND).unwrap();
        // 130 s in one-second writes: two full chunks and a 10 s one.
        let second = vec![0.25f32; TARGET_SAMPLE_RATE as usize];
        for _ in 0..130 {
            writer.write(&second).unwrap();
        }
        writer.finish().unwrap();

        let records = ledger.records();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records.iter().map(|r| r.n_frames).collect::<Vec<_>>(),
            [960_000, 960_000, 160_000]
        );
        assert_eq!(
            records.iter().map(|r| r.start_ms).collect::<Vec<_>>(),
            [2_000, 62_000, 122_000]
        );
        assert_eq!(records[1].anchor_host_ns, ORIGIN + 62 * SECOND);
        assert_eq!(records[2].path, "m1/mic/000002.pcm");
        for record in &records {
            assert_eq!(record.status, ChunkStatus::Closed);
            assert_eq!(
                file_len(&tmp, record.seq),
                2 * record.n_frames,
                "bytes = 2 x n_frames"
            );
        }
    }

    #[test]
    fn rotates_on_begin_and_reports_to_the_ledger_in_order() {
        let tmp = TempDir::new("chunks-begin");
        let (mut writer, ledger) = writer(&tmp, 1_000);
        writer.begin(ORIGIN).unwrap();
        writer.write(&[0.5; 300]).unwrap();
        // A 250 ms hole after 300 frames (18.75 ms).
        writer.begin(ORIGIN + 268_750_000).unwrap();
        writer.write(&[-0.5; 1_500]).unwrap();
        writer.finish().unwrap();
        writer.finish().unwrap();

        let calls = ledger.calls();
        let opened = ledger.opened();
        assert_eq!(opened.len(), 3);
        assert_eq!(
            calls,
            vec![
                LedgerCall::Opened(opened[0].clone()),
                LedgerCall::Closed {
                    id: opened[0].id.clone(),
                    n_frames: 300
                },
                LedgerCall::Opened(opened[1].clone()),
                LedgerCall::Closed {
                    id: opened[1].id.clone(),
                    n_frames: 1_000
                },
                LedgerCall::Opened(opened[2].clone()),
                LedgerCall::Closed {
                    id: opened[2].id.clone(),
                    n_frames: 500
                },
            ]
        );
        assert!(opened
            .iter()
            .all(|r| r.status == ChunkStatus::Open && r.n_frames == 0));
        assert_eq!(opened.iter().map(|r| r.seq).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(opened[1].start_ms, 268);

        // The sidecar tells the same story, plus the gap.
        let sidecar = sidecar::read(&tmp.path().join("m1").join("mic"))
            .unwrap()
            .unwrap();
        assert_eq!(sidecar.origin_host_ns, ORIGIN);
        assert_eq!(sidecar.chunk_records(), ledger.records());
        assert_eq!(
            sidecar
                .chunks
                .iter()
                .map(|c| c.gap_before_ms)
                .collect::<Vec<_>>(),
            [0, 250, 0]
        );

        // s16le on disk.
        let bytes = fs::read(tmp.path().join("m1/mic/000000.pcm")).unwrap();
        assert_eq!(bytes.len(), 600);
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 16_384);
    }

    #[test]
    fn every_write_reaches_the_file_before_the_chunk_closes() {
        let tmp = TempDir::new("chunks-eager");
        let (mut writer, _ledger) = writer(&tmp, CHUNK_FRAMES);
        writer.begin(ORIGIN).unwrap();
        writer.write(&[0.1; 160]).unwrap();
        assert_eq!(file_len(&tmp, 0), 320, "nothing is held back in user space");
        assert_eq!(writer.open_chunk_frames(), 160);
        let sidecar = sidecar::read(&tmp.path().join("m1").join("mic"))
            .unwrap()
            .unwrap();
        assert_eq!(
            sidecar.chunks[0].status,
            ChunkStatus::Open,
            "the open chunk is already listed"
        );
    }

    #[test]
    fn errors_are_results_not_panics() {
        let tmp = TempDir::new("chunks-errors");
        let (mut writer, _ledger) = writer(&tmp, 1_000);
        assert!(writer
            .write(&[0.0; 4])
            .unwrap_err()
            .contains("before begin"));

        // The next chunk file is already there: refuse to overwrite it.
        fs::write(tmp.path().join("m1/mic/000000.pcm"), b"audio").unwrap();
        assert!(writer.begin(ORIGIN).unwrap_err().contains("000000.pcm"));
        assert_eq!(
            fs::read(tmp.path().join("m1/mic/000000.pcm")).unwrap(),
            b"audio"
        );

        // A second writer for the same track is refused as well.
        let config =
            ChunkWriterConfig::new(tmp.path().to_path_buf(), "m1", "t1", TrackKind::Mic, ORIGIN);
        assert!(ChunkWriter::new(config, Box::new(FakeLedger::default())).is_err());

        // A root that cannot hold directories.
        let file_root = tmp.path().join("not-a-dir");
        fs::write(&file_root, b"x").unwrap();
        let config = ChunkWriterConfig::new(file_root, "m1", "t1", TrackKind::Mic, ORIGIN);
        assert!(ChunkWriter::new(config, Box::new(FakeLedger::default())).is_err());
    }

    #[test]
    fn a_failing_ledger_does_not_stop_the_audio() {
        let tmp = TempDir::new("chunks-ledger");
        let ledger = FakeLedger {
            fail: true,
            ..FakeLedger::default()
        };
        let config =
            ChunkWriterConfig::new(tmp.path().to_path_buf(), "m1", "t1", TrackKind::Mic, ORIGIN);
        let mut writer = ChunkWriter::new(config, Box::new(ledger.clone())).unwrap();
        writer.begin(ORIGIN).unwrap();
        writer.write(&[0.5; 100]).unwrap();
        writer.finish().unwrap();
        assert_eq!(file_len(&tmp, 0), 200);
        assert_eq!(ledger.calls().len(), 2);
        let sidecar = sidecar::read(&tmp.path().join("m1").join("mic"))
            .unwrap()
            .unwrap();
        assert_eq!(sidecar.chunks[0].n_frames, 100);
    }
}
