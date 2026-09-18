//! OWNER: WP4 (recording). Reading a track back: 16 kHz mono f32 for a range
//! of the meeting timeline, across chunk files.
//!
//! `read_spans` returns only the audio that exists, each span with its
//! position, so a caller can tell a gap from recorded silence and decide what
//! to do with it. `types::TrackAudio` (what long-form decoding consumes) is
//! built on it and reads gaps, deleted chunks and missing files as silence.
//!
//! Chunks are placed by their `start_ms`. Where drift makes one chunk run a
//! few ms into the next, the later chunk wins, so the timeline never doubles.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::recovery::frames_on_disk;
use super::BYTES_PER_FRAME;
use crate::meetings::types::{ChunkRecord, ChunkStatus, TrackAudio, TARGET_SAMPLE_RATE};

const FRAMES_PER_MS: u64 = TARGET_SAMPLE_RATE as u64 / 1_000;

pub fn s16_to_f32(sample: i16) -> f32 {
    sample as f32 / 32_768.0
}

/// A stretch of recorded audio on the meeting timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSpan {
    /// Timeline position of the first sample, in 16 kHz frames.
    pub start_frame: u64,
    pub samples: Vec<f32>,
}

// Only the tests look at a span this way.
#[cfg(test)]
impl AudioSpan {
    pub fn start_ms(&self) -> u64 {
        self.start_frame / FRAMES_PER_MS
    }

    pub fn end_frame(&self) -> u64 {
        self.start_frame + self.samples.len() as u64
    }
}

pub struct ChunkTrackAudio {
    root: PathBuf,
    /// Sorted by position on the timeline.
    chunks: Vec<ChunkRecord>,
}

impl ChunkTrackAudio {
    /// `root` is the meetings audio root the records' paths are relative to;
    /// `chunks` are one track's rows, in any order.
    pub fn new(root: PathBuf, mut chunks: Vec<ChunkRecord>) -> Self {
        chunks.sort_by_key(|chunk| (chunk.start_ms, chunk.seq));
        Self { root, chunks }
    }

    /// The same from the track's `track.json`, without the database. Run
    /// `recovery::recover_meeting_dir` first if the track may have open chunks.
    /// Nothing in the app reads a track that way yet: the tests use it to
    /// prove that the sidecar alone is enough to get the audio back.
    #[cfg(test)]
    pub fn from_sidecar(
        root: &Path,
        meeting_id: &str,
        kind: crate::meetings::types::TrackKind,
    ) -> Result<Self, String> {
        use super::{sidecar, track_dir};
        let dir = track_dir(root, meeting_id, kind)?;
        let sidecar = sidecar::read(&dir)?
            .ok_or_else(|| format!("No {} in {}", sidecar::SIDECAR_FILE, dir.display()))?;
        Ok(Self::new(root.to_path_buf(), sidecar.chunk_records()))
    }

    /// Frames of a chunk that can actually be read right now.
    fn readable_frames(&self, chunk: &ChunkRecord) -> Result<u64, String> {
        let on_disk = || frames_on_disk(&self.root.join(&chunk.path));
        match chunk.status {
            ChunkStatus::Deleted => Ok(0),
            // Still being written: whatever has reached the file.
            ChunkStatus::Open => on_disk(),
            ChunkStatus::Closed | ChunkStatus::Recovered => Ok(chunk.n_frames.min(on_disk()?)),
        }
    }

    /// Frames a chunk occupies on the timeline, readable or not.
    fn timeline_frames(&self, chunk: &ChunkRecord) -> u64 {
        match chunk.status {
            ChunkStatus::Open => self.readable_frames(chunk).unwrap_or(0),
            _ => chunk.n_frames,
        }
    }

    fn end_frame(&self) -> u64 {
        self.chunks
            .iter()
            .map(|chunk| chunk.start_ms * FRAMES_PER_MS + self.timeline_frames(chunk))
            .max()
            .unwrap_or(0)
    }

    /// The recorded audio inside `[start_ms, start_ms + len_ms)`, in timeline
    /// order. Gaps are simply not there: compare each span's `start_frame`
    /// with the previous span's `end_frame`.
    pub fn read_spans(&self, start_ms: u64, len_ms: u64) -> Result<Vec<AudioSpan>, String> {
        let window_start = start_ms * FRAMES_PER_MS;
        let window_end = window_start + len_ms * FRAMES_PER_MS;
        let mut spans = Vec::new();
        for (i, chunk) in self.chunks.iter().enumerate() {
            let chunk_start = chunk.start_ms * FRAMES_PER_MS;
            if chunk_start >= window_end {
                break;
            }
            let mut chunk_end = chunk_start + self.readable_frames(chunk)?;
            if let Some(next) = self.chunks.get(i + 1) {
                chunk_end = chunk_end.min((next.start_ms * FRAMES_PER_MS).max(chunk_start));
            }
            let from = chunk_start.max(window_start);
            let to = chunk_end.min(window_end);
            if from >= to {
                continue;
            }
            let samples = read_frames(&self.root.join(&chunk.path), from - chunk_start, to - from)?;
            if !samples.is_empty() {
                spans.push(AudioSpan {
                    start_frame: from,
                    samples,
                });
            }
        }
        Ok(spans)
    }
}

/// Up to `frames` frames starting `offset` frames into a chunk file. Shorter
/// when the file is; empty when it is gone.
fn read_frames(path: &Path, offset: u64, frames: u64) -> Result<Vec<f32>, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Failed to open {}: {e}", path.display())),
    };
    file.seek(SeekFrom::Start(offset * BYTES_PER_FRAME))
        .map_err(|e| format!("Failed to seek in {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(frames * BYTES_PER_FRAME)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    Ok(bytes
        .chunks_exact(BYTES_PER_FRAME as usize)
        .map(|pair| s16_to_f32(i16::from_le_bytes([pair[0], pair[1]])))
        .collect())
}

impl TrackAudio for ChunkTrackAudio {
    fn duration_ms(&self) -> u64 {
        self.end_frame().div_ceil(FRAMES_PER_MS)
    }

    fn read(&mut self, start_ms: u64, len_ms: u64) -> Result<Vec<f32>, String> {
        let window_start = start_ms * FRAMES_PER_MS;
        let window_end = (window_start + len_ms * FRAMES_PER_MS).min(self.end_frame());
        if window_end <= window_start {
            return Ok(Vec::new());
        }
        let mut samples = vec![0.0; (window_end - window_start) as usize];
        for span in self.read_spans(start_ms, len_ms)? {
            let at = (span.start_frame - window_start) as usize;
            let len = span.samples.len().min(samples.len() - at);
            samples[at..at + len].copy_from_slice(&span.samples[..len]);
        }
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::super::chunk_writer::test_support::FakeLedger;
    use super::super::chunk_writer::ChunkWriter;
    use super::super::test_support::TempDir;
    use super::super::ChunkWriterConfig;
    use super::*;
    use crate::meetings::types::SampleSink;
    use crate::meetings::types::TrackKind;

    const ORIGIN: u64 = 9_000_000_000;
    const MS: u64 = 1_000_000;

    /// A ramp that makes every frame identifiable: frame `n` of the recording
    /// holds `n / 5000`, a step of six s16 levels per frame.
    fn ramp(first: usize, len: usize) -> Vec<f32> {
        (first..first + len).map(|n| n as f32 / 5_000.0).collect()
    }

    fn assert_ramp(samples: &[f32], first: usize) {
        for (i, sample) in samples.iter().enumerate() {
            let expected = (first + i) as f32 / 5_000.0;
            assert!(
                (sample - expected).abs() < 8e-5,
                "frame {}: {sample} vs {expected}",
                first + i
            );
        }
    }

    /// Chunks of 100 ms (1600 frames):
    /// - 0..250 ms recorded (frames 0..4000 of the ramp), rotating at 100 and 200 ms,
    /// - a gap until 1000 ms,
    /// - 1000..1050 ms recorded (frames 4000..4800).
    fn recorded(tmp: &TempDir) -> ChunkTrackAudio {
        let ledger = FakeLedger::default();
        let mut config = ChunkWriterConfig::new(
            tmp.path().to_path_buf(),
            "m1",
            "t1",
            TrackKind::System,
            ORIGIN,
        );
        config.chunk_frames = 1_600;
        let mut writer = ChunkWriter::new(config, Box::new(ledger.clone())).unwrap();
        writer.begin(ORIGIN).unwrap();
        writer.write(&ramp(0, 4_000)).unwrap();
        writer.begin(ORIGIN + 1_000 * MS).unwrap();
        writer.write(&ramp(4_000, 800)).unwrap();
        writer.finish().unwrap();
        let mut records = ledger.records();
        assert_eq!(
            records.iter().map(|r| r.start_ms).collect::<Vec<_>>(),
            [0, 100, 200, 1_000]
        );
        records.reverse(); // any order in
        ChunkTrackAudio::new(tmp.path().to_path_buf(), records)
    }

    #[test]
    fn reads_the_right_samples_across_a_chunk_border() {
        let tmp = TempDir::new("reader-border");
        let mut audio = recorded(&tmp);
        // 90..130 ms straddles the 100 ms border between chunk 0 and 1.
        let samples = audio.read(90, 40).unwrap();
        assert_eq!(samples.len(), 640);
        assert_ramp(&samples, 1_440);

        // As spans: two pieces that touch, one per chunk file.
        let spans = audio.read_spans(90, 40).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start_frame, spans[0].samples.len()), (1_440, 160));
        assert_eq!(spans[0].end_frame(), spans[1].start_frame);
        assert_eq!(spans[1].start_ms(), 100);
    }

    #[test]
    fn a_gap_is_a_gap_in_spans_and_silence_in_track_audio() {
        let tmp = TempDir::new("reader-gap");
        let mut audio = recorded(&tmp);
        assert_eq!(audio.duration_ms(), 1_050);

        // 200..1050 ms: 50 ms of audio, 750 ms of nothing, 50 ms of audio.
        let spans = audio.read_spans(200, 850).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start_frame, spans[0].samples.len()), (3_200, 800));
        assert_eq!(
            (spans[1].start_frame, spans[1].samples.len()),
            (16_000, 800)
        );
        assert_ramp(&spans[0].samples, 3_200);
        assert_ramp(&spans[1].samples, 4_000);

        let samples = audio.read(200, 850).unwrap();
        assert_eq!(samples.len(), 850 * 16);
        assert_ramp(&samples[..800], 3_200);
        assert!(
            samples[800..12_800].iter().all(|s| *s == 0.0),
            "the gap reads as silence"
        );
        assert_ramp(&samples[12_800..], 4_000);

        // Entirely inside the gap.
        assert!(audio.read_spans(400, 200).unwrap().is_empty());
        assert_eq!(audio.read(400, 200).unwrap(), vec![0.0; 3_200]);
    }

    #[test]
    fn past_the_end_is_shorter() {
        let tmp = TempDir::new("reader-end");
        let mut audio = recorded(&tmp);
        let samples = audio.read(1_040, 500).unwrap();
        assert_eq!(samples.len(), 160);
        assert_ramp(&samples, 4_640);
        assert!(audio.read(1_050, 100).unwrap().is_empty());
        assert!(audio.read(60_000, 100).unwrap().is_empty());
        let mut empty = ChunkTrackAudio::new(tmp.path().to_path_buf(), Vec::new());
        assert_eq!(empty.duration_ms(), 0);
        assert!(empty.read(0, 1_000).unwrap().is_empty());
    }

    #[test]
    fn deleted_and_missing_audio_reads_as_silence_but_keeps_the_timeline() {
        let tmp = TempDir::new("reader-deleted");
        let mut audio = recorded(&tmp);
        audio.chunks[1].status = ChunkStatus::Deleted;
        std::fs::remove_file(tmp.path().join(&audio.chunks[2].path)).unwrap();

        assert_eq!(audio.duration_ms(), 1_050);
        let samples = audio.read(0, 300).unwrap();
        assert_eq!(samples.len(), 4_800);
        assert_ramp(&samples[..1_600], 0);
        assert!(samples[1_600..].iter().all(|s| *s == 0.0));
        assert_eq!(audio.read_spans(0, 300).unwrap().len(), 1);
    }

    #[test]
    fn an_overlapping_chunk_is_clipped_where_the_next_one_starts() {
        let tmp = TempDir::new("reader-overlap");
        let mut audio = recorded(&tmp);
        // Drift: chunk 1 claims to start 5 ms before chunk 0 ends.
        audio.chunks[1].start_ms = 95;
        let spans = audio.read_spans(0, 150).unwrap();
        assert_eq!(spans[0].samples.len(), 95 * 16);
        assert_eq!(spans[1].start_frame, 95 * 16);
        assert_ramp(&spans[1].samples[..10], 1_600);
        assert_eq!(audio.read(0, 150).unwrap().len(), 2_400);
    }

    #[test]
    fn reads_open_and_recovered_chunks_from_the_sidecar() {
        let tmp = TempDir::new("reader-sidecar");
        let mut config =
            ChunkWriterConfig::new(tmp.path().to_path_buf(), "m1", "t1", TrackKind::Mic, ORIGIN);
        config.chunk_frames = 1_600;
        let mut writer = ChunkWriter::new(config, Box::new(FakeLedger::default())).unwrap();
        writer.begin(ORIGIN).unwrap();
        writer.write(&ramp(0, 2_000)).unwrap();
        std::mem::forget(writer); // crash with chunk 1 open

        // Live, before recovery: the open chunk reads up to what is on disk.
        let mut live = ChunkTrackAudio::from_sidecar(tmp.path(), "m1", TrackKind::Mic).unwrap();
        assert_eq!(live.duration_ms(), 125);
        assert_ramp(&live.read(0, 200).unwrap(), 0);

        super::super::recovery::recover_meeting_dir(&tmp.path().join("m1")).unwrap();
        let mut recovered =
            ChunkTrackAudio::from_sidecar(tmp.path(), "m1", TrackKind::Mic).unwrap();
        assert_eq!(recovered.chunks[1].status, ChunkStatus::Recovered);
        let samples = recovered.read(0, 200).unwrap();
        assert_eq!(samples.len(), 2_000);
        assert_ramp(&samples, 0);

        assert!(ChunkTrackAudio::from_sidecar(tmp.path(), "m1", TrackKind::System).is_err());
    }
}
