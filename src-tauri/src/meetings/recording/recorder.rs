//! OWNER: WP4 (recording). One track from an `AudioSource` to a `SampleSink`.
//!
//! ```text
//! audio callback                      writer thread (one per track)
//! RecorderHandler::on_frames  --->    drain -> Timeline -> StreamResampler -> SampleSink
//!   copy into two rtrb rings            (every ~20 ms)
//! ```
//!
//! - The callback side only copies: samples into one lock-free ring, a small
//!   packet (host time, format, length) into another. No allocation, locks,
//!   I/O or logging. When a ring is full the buffer is dropped and counted;
//!   the next packet tells the writer about the loss, which the timeline
//!   turns into a gap (or a few ms of padding when the hole is short).
//!   Memory is bounded by the rings: about 5 s of audio per track.
//! - `pause` drops audio at the callback, and the writer closes the open
//!   chunk once it has written what came before; `resume` starts a new one.
//!   Paused time is a gap, never silence on disk.
//! - A sink error (disk full) or a resampler error ends the track in an error
//!   state: `status().error`. The callback keeps returning immediately and
//!   nothing panics. The other track and the meeting carry on.
//! - `stop` drains the ring, flushes the resampler and finishes the sink.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use super::chunk_writer::{ChunkWriter, ChunkWriterConfig};
use super::resample::StreamResampler;
use super::timeline::{frames_to_ns, ns_to_frames, Placement, Timeline, GAP_THRESHOLD_MS};
use super::{log, CHUNK_FRAMES};
use crate::meetings::types::{
    AudioFrames, AudioSourceHandler, ChunkLedger, Discontinuity, SampleSink, SourceFormat,
    TrackKind, TARGET_SAMPLE_RATE,
};

#[derive(Debug, Clone)]
pub struct RecorderConfig {
    /// Capacity of the sample ring, in interleaved samples at the source's
    /// native format. The default holds 5 s of 48 kHz stereo.
    pub ring_samples: usize,
    /// Capacity of the packet ring: one packet per callback.
    pub ring_packets: usize,
    /// How often the writer thread drains the rings.
    pub poll_interval: Duration,
    /// Must match the sink's chunk size (`ChunkWriterConfig::chunk_frames`):
    /// the recorder re-anchors the sink exactly when a chunk is full.
    pub chunk_frames: u64,
    pub gap_threshold_ms: u64,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            ring_samples: 5 * 48_000 * 2,
            ring_packets: 4_096,
            poll_interval: Duration::from_millis(20),
            chunk_frames: CHUNK_FRAMES,
            gap_threshold_ms: GAP_THRESHOLD_MS,
        }
    }
}

/// What the callback tells the writer thread, in order.
#[derive(Debug, Clone, Copy)]
enum Packet {
    /// `n_samples` interleaved samples are waiting in the sample ring.
    Frames {
        host_time_ns: u64,
        n_samples: usize,
        format: SourceFormat,
        loss_before: bool,
    },
    Discontinuity {
        kind: Discontinuity,
    },
}

#[derive(Default)]
struct Shared {
    paused: AtomicBool,
    stop: AtomicBool,
    failed: AtomicBool,
    finished: AtomicBool,
    overflow_frames: AtomicU64,
    source_dropped_frames: AtomicU64,
    written_frames: AtomicU64,
    padded_frames: AtomicU64,
    runs: AtomicU64,
    /// Only the writer thread and `status()` touch this, never the callback.
    error: Mutex<Option<String>>,
}

/// A snapshot of one track's recorder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderStatus {
    /// Native frames dropped because the ring was full (the writer stalled).
    pub overflow_frames: u64,
    /// Native frames the source itself reported lost.
    pub source_dropped_frames: u64,
    /// 16 kHz frames handed to the sink.
    pub written_frames: u64,
    /// Of those, silence inserted to bridge short, known losses (about).
    pub padded_frames: u64,
    /// Gaps on the timeline so far: runs started after the first.
    pub gaps: u64,
    pub paused: bool,
    /// The writer thread has finished the sink and exited.
    pub finished: bool,
    /// Set once the track failed (disk full, write error). It records no more.
    pub error: Option<String>,
}

/// The callback side. Give it to `AudioSource::start`.
pub struct RecorderHandler {
    samples: Producer<f32>,
    packets: Producer<Packet>,
    shared: Arc<Shared>,
    /// Something was dropped since the last packet that made it through.
    loss_pending: bool,
}

impl RecorderHandler {
    /// Interleaved samples the ring can hold: the bound on buffered audio.
    #[cfg(test)]
    pub fn ring_capacity(&self) -> usize {
        self.samples.buffer().capacity()
    }
}

impl AudioSourceHandler for RecorderHandler {
    fn on_frames(&mut self, frames: AudioFrames<'_>) {
        let channels = frames.format.channels as usize;
        if channels == 0 || self.shared.failed.load(Ordering::Relaxed) {
            return;
        }
        let n_frames = frames.samples.len() / channels;
        if n_frames == 0 {
            return;
        }
        if self.shared.paused.load(Ordering::Relaxed) {
            self.loss_pending = true;
            return;
        }
        let samples = &frames.samples[..n_frames * channels];
        if self.packets.is_full() || self.samples.slots() < samples.len() {
            self.shared
                .overflow_frames
                .fetch_add(n_frames as u64, Ordering::Relaxed);
            self.loss_pending = true;
            return;
        }
        // Both were checked above and this is the only producer.
        if self.samples.push_entire_slice(samples).is_err() {
            self.loss_pending = true;
            return;
        }
        let packet = Packet::Frames {
            host_time_ns: frames.host_time_ns,
            n_samples: samples.len(),
            format: frames.format,
            loss_before: self.loss_pending,
        };
        if self.packets.push(packet).is_ok() {
            self.loss_pending = false;
        }
    }

    fn on_discontinuity(&mut self, discontinuity: Discontinuity, _host_time_ns: u64) {
        if let Discontinuity::Dropped { frames } = discontinuity {
            self.shared
                .source_dropped_frames
                .fetch_add(frames, Ordering::Relaxed);
        }
        if self
            .packets
            .push(Packet::Discontinuity {
                kind: discontinuity,
            })
            .is_err()
        {
            self.loss_pending = true;
        }
    }
}

/// The control side. The session (WP7) keeps one per track.
pub struct TrackRecorder {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl TrackRecorder {
    /// Audio from here on is dropped; the open chunk is closed.
    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::Relaxed);
    }

    /// Recording continues in a new chunk; the pause shows up as a gap.
    pub fn resume(&self) {
        self.shared.paused.store(false, Ordering::Relaxed);
    }

    pub fn status(&self) -> RecorderStatus {
        let shared = &self.shared;
        RecorderStatus {
            overflow_frames: shared.overflow_frames.load(Ordering::Relaxed),
            source_dropped_frames: shared.source_dropped_frames.load(Ordering::Relaxed),
            written_frames: shared.written_frames.load(Ordering::Relaxed),
            padded_frames: shared.padded_frames.load(Ordering::Relaxed),
            gaps: shared.runs.load(Ordering::Relaxed).saturating_sub(1),
            paused: shared.paused.load(Ordering::Relaxed),
            finished: shared.finished.load(Ordering::Relaxed),
            error: shared
                .error
                .lock()
                .map(|e| e.clone())
                .unwrap_or_else(|e| e.into_inner().clone()),
        }
    }

    /// Drains what the callback delivered so far, flushes the resampler,
    /// finishes the sink and joins the writer thread. Stop the source first
    /// so nothing arrives afterwards. Idempotent.
    pub fn stop(&mut self) -> RecorderStatus {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                set_error(&self.shared, "The recording thread panicked".to_string());
            }
        }
        self.status()
    }
}

impl Drop for TrackRecorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn set_error(shared: &Shared, error: String) {
    shared.failed.store(true, Ordering::Relaxed);
    let mut slot = shared.error.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = Some(error);
    }
}

/// Starts the writer thread for one track. `origin_host_ns` is the meeting's
/// origin, shared by both tracks. Returns the control side and the handler
/// for `AudioSource::start`.
pub fn start_track(
    kind: TrackKind,
    sink: Box<dyn SampleSink>,
    origin_host_ns: u64,
    config: RecorderConfig,
) -> Result<(TrackRecorder, RecorderHandler), String> {
    if config.ring_samples == 0 || config.ring_packets == 0 || config.chunk_frames == 0 {
        return Err("Recorder rings and chunk size must not be zero".to_string());
    }
    let (sample_producer, sample_consumer) = RingBuffer::<f32>::new(config.ring_samples);
    let (packet_producer, packet_consumer) = RingBuffer::<Packet>::new(config.ring_packets);
    let shared = Arc::new(Shared::default());

    let mut writer = Writer {
        kind,
        samples: sample_consumer,
        packets: packet_consumer,
        sink,
        resampler: StreamResampler::new(),
        timeline: Timeline::with_gap_threshold_ms(origin_host_ns, config.gap_threshold_ms),
        shared: shared.clone(),
        chunk_frames: config.chunk_frames,
        poll_interval: config.poll_interval,
        chunk_open: false,
        chunk_pos: 0,
        chunk_anchor_ns: 0,
        native: Vec::new(),
        out: Vec::new(),
    };
    let thread = std::thread::Builder::new()
        .name(format!("meeting-writer-{}", kind.as_str()))
        .spawn(move || writer.run())
        .map_err(|e| format!("Failed to start the recording thread: {e}"))?;

    Ok((
        TrackRecorder {
            shared: shared.clone(),
            thread: Some(thread),
        },
        RecorderHandler {
            samples: sample_producer,
            packets: packet_producer,
            shared,
            loss_pending: false,
        },
    ))
}

/// `start_track` writing chunk files: the usual way to record a track. Keeps
/// the recorder's and the chunk writer's chunk size in step.
pub fn record_to_disk(
    writer: ChunkWriterConfig,
    ledger: Box<dyn ChunkLedger>,
    mut config: RecorderConfig,
) -> Result<(TrackRecorder, RecorderHandler), String> {
    config.chunk_frames = writer.chunk_frames;
    let (kind, origin_host_ns) = (writer.kind, writer.origin_host_ns);
    let sink = ChunkWriter::new(writer, ledger)?;
    start_track(kind, Box::new(sink), origin_host_ns, config)
}

struct Writer {
    kind: TrackKind,
    samples: Consumer<f32>,
    packets: Consumer<Packet>,
    sink: Box<dyn SampleSink>,
    resampler: StreamResampler,
    timeline: Timeline,
    shared: Arc<Shared>,
    chunk_frames: u64,
    poll_interval: Duration,
    /// Whether the sink has a chunk open, the frames written to it, and its
    /// anchor.
    chunk_open: bool,
    chunk_pos: u64,
    chunk_anchor_ns: u64,
    /// Reused buffers: one callback's native samples, resampled output.
    native: Vec<f32>,
    out: Vec<f32>,
}

impl Writer {
    fn run(&mut self) {
        if let Err(e) = self.record() {
            log(
                "ERROR",
                &format!(
                    "Meetings: {} track stopped recording: {e}",
                    self.kind.as_str()
                ),
            );
            set_error(&self.shared, e);
            // Keep what reached the disk: close the chunk if that still works.
            let _ = self.sink.finish();
        }
        self.shared.finished.store(true, Ordering::Relaxed);
    }

    fn record(&mut self) -> Result<(), String> {
        loop {
            // Read the flags before draining: whatever the callback delivered
            // before `stop()` or `pause()` is in the ring by now. Paused audio
            // never gets here; the callback drops it.
            let stopping = self.shared.stop.load(Ordering::Relaxed);
            let paused = self.shared.paused.load(Ordering::Relaxed);
            self.drain()?;
            if paused || stopping {
                self.end_run()?;
            }
            if stopping {
                return Ok(());
            }
            std::thread::sleep(self.poll_interval);
        }
    }

    fn drain(&mut self) -> Result<(), String> {
        while let Ok(packet) = self.packets.pop() {
            match packet {
                Packet::Frames {
                    host_time_ns,
                    n_samples,
                    format,
                    loss_before,
                } => {
                    self.native.resize(n_samples, 0.0);
                    if self.samples.pop_entire_slice(&mut self.native).is_err() {
                        return Err("Recorder ring out of step with its packets".to_string());
                    }
                    if loss_before {
                        self.timeline.note_loss();
                    }
                    self.on_frames(host_time_ns, format)?;
                }
                Packet::Discontinuity { kind } => match kind {
                    // The frames carry their format; the resampler follows.
                    Discontinuity::FormatChanged { .. } => {}
                    // How much is missing shows in the next host time.
                    Discontinuity::Dropped { .. } | Discontinuity::Stalled => {
                        self.timeline.note_loss()
                    }
                },
            }
        }
        Ok(())
    }

    fn on_frames(&mut self, host_time_ns: u64, format: SourceFormat) -> Result<(), String> {
        match self.timeline.classify(host_time_ns) {
            Placement::Contiguous => {}
            Placement::Pad { pad_ns } => {
                if let Some(current) = self.resampler.format() {
                    let frames = ns_to_frames(pad_ns, current.sample_rate) as usize;
                    self.resampler.push_silence(frames, &mut self.out)?;
                    self.shared
                        .padded_frames
                        .fetch_add(ns_to_frames(pad_ns, TARGET_SAMPLE_RATE), Ordering::Relaxed);
                    self.timeline
                        .pad(frames_to_ns(frames as u64, current.sample_rate));
                }
            }
            Placement::NewRun => {
                // The old run's tail belongs to the old chunk.
                self.end_run()?;
                let start = self.timeline.start_run(host_time_ns);
                self.begin_chunk(start.anchor_host_ns)?;
                self.shared.runs.fetch_add(1, Ordering::Relaxed);
            }
        }
        let frames = (self.native.len() / format.channels.max(1) as usize) as u64;
        self.timeline
            .advance(host_time_ns, frames, format.sample_rate);
        self.resampler.push(&self.native, format, &mut self.out)?;
        self.emit()
    }

    /// Ends the current run: the resampler's tail goes to the open chunk,
    /// which is then closed. The next buffer starts a new run.
    fn end_run(&mut self) -> Result<(), String> {
        if self.timeline.in_run() {
            self.resampler.flush(&mut self.out)?;
            self.emit()?;
            self.timeline.end_run();
        }
        if self.chunk_open {
            self.chunk_open = false;
            self.sink.finish()?;
        }
        Ok(())
    }

    fn begin_chunk(&mut self, anchor_host_ns: u64) -> Result<(), String> {
        self.chunk_open = true;
        self.sink.begin(anchor_host_ns)?;
        self.chunk_anchor_ns = anchor_host_ns;
        self.chunk_pos = 0;
        Ok(())
    }

    /// Hands `out` to the sink. When the chunk is full, the next one is
    /// anchored where the host clock says that sample was captured, not where
    /// the sample count says: that is what bounds drift to a chunk.
    fn emit(&mut self) -> Result<(), String> {
        let out = std::mem::take(&mut self.out);
        let result = self.emit_samples(&out);
        self.out = out;
        self.out.clear();
        result
    }

    fn emit_samples(&mut self, mut rest: &[f32]) -> Result<(), String> {
        while !rest.is_empty() {
            if self.chunk_pos == self.chunk_frames {
                let chunk_ns = frames_to_ns(self.chunk_frames, TARGET_SAMPLE_RATE);
                let anchor = match self.timeline.rotate(chunk_ns) {
                    Some(start) => start.anchor_host_ns,
                    None => self.chunk_anchor_ns + chunk_ns,
                };
                self.begin_chunk(anchor)?;
            }
            let take = ((self.chunk_frames - self.chunk_pos).min(rest.len() as u64)) as usize;
            self.sink.write(&rest[..take])?;
            self.chunk_pos += take as u64;
            self.shared
                .written_frames
                .fetch_add(take as u64, Ordering::Relaxed);
            rest = &rest[take..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::chunk_writer::test_support::FakeLedger;
    use super::super::test_support::TempDir;
    use super::super::{sidecar, ChunkTrackAudio};
    use super::*;
    use crate::meetings::types::{AudioSource, TrackAudio};
    use std::f64::consts::TAU;
    use std::time::Instant;

    const ORIGIN: u64 = 3_000_000_000_000;
    const MS: u64 = 1_000_000;
    const TONE_HZ: f64 = 330.0;
    const STEREO_48K: SourceFormat = SourceFormat {
        sample_rate: 48_000,
        channels: 2,
    };

    /// A capture source driven by the test: it owns the handler like a real
    /// one and delivers buffers with whatever host time the test says. That
    /// host time is the recorder's only clock.
    struct FakeSource {
        handler: Option<Box<dyn AudioSourceHandler>>,
        format: SourceFormat,
        /// Seconds of tone delivered so far, to keep the phase continuous.
        tone_s: f64,
        /// Host time of the next buffer.
        host_ns: u64,
    }

    impl FakeSource {
        fn new(format: SourceFormat) -> Self {
            Self {
                handler: None,
                format,
                tone_s: 0.0,
                host_ns: ORIGIN,
            }
        }

        /// Delivers `count` 10 ms buffers of tone. `stamp_ns` is how far the
        /// host clock moves per buffer: 10 ms unless the test wants drift.
        fn deliver_stamped(&mut self, count: usize, stamp_ns: u64) {
            let frames = self.format.sample_rate as usize / 100;
            let mut samples = Vec::with_capacity(frames * self.format.channels as usize);
            for _ in 0..count {
                samples.clear();
                for n in 0..frames {
                    let t = self.tone_s + n as f64 / self.format.sample_rate as f64;
                    let value = 0.5 * (TAU * TONE_HZ * t).sin() as f32;
                    samples.extend(std::iter::repeat(value).take(self.format.channels as usize));
                }
                self.handler.as_mut().unwrap().on_frames(AudioFrames {
                    samples: &samples,
                    format: self.format,
                    host_time_ns: self.host_ns,
                });
                self.tone_s += 0.01;
                self.host_ns += stamp_ns;
            }
        }

        fn deliver(&mut self, count: usize) {
            self.deliver_stamped(count, 10 * MS);
        }

        /// Time passes without audio.
        fn skip_ms(&mut self, ms: u64) {
            self.host_ns += ms * MS;
            self.tone_s += ms as f64 / 1_000.0;
        }
    }

    impl AudioSource for FakeSource {
        fn kind(&self) -> TrackKind {
            TrackKind::System
        }
        fn device_name(&self) -> Option<String> {
            Some("Fake".into())
        }
        fn format(&self) -> Option<SourceFormat> {
            self.handler.as_ref().map(|_| self.format)
        }
        fn start(&mut self, handler: Box<dyn AudioSourceHandler>) -> Result<SourceFormat, String> {
            self.handler = Some(handler);
            Ok(self.format)
        }
        fn stop(&mut self) -> Result<(), String> {
            self.handler = None;
            Ok(())
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    enum SinkCall {
        Begin(u64),
        Write(usize),
        Finish,
    }

    /// A `SampleSink` that remembers its calls. It can be slow, and it can
    /// run out of disk.
    #[derive(Clone, Default)]
    struct FakeSink {
        calls: Arc<Mutex<Vec<SinkCall>>>,
        write_delay: Option<Duration>,
        fail_after_frames: Option<usize>,
    }

    impl FakeSink {
        fn calls(&self) -> Vec<SinkCall> {
            self.calls.lock().unwrap().clone()
        }
        fn begins(&self) -> Vec<u64> {
            self.calls()
                .into_iter()
                .filter_map(|c| {
                    if let SinkCall::Begin(anchor) = c {
                        Some(anchor)
                    } else {
                        None
                    }
                })
                .collect()
        }
        fn frames(&self) -> usize {
            self.calls()
                .iter()
                .map(|c| if let SinkCall::Write(n) = c { *n } else { 0 })
                .sum()
        }
    }

    impl SampleSink for FakeSink {
        fn begin(&mut self, anchor_host_ns: u64) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Begin(anchor_host_ns));
            Ok(())
        }
        fn write(&mut self, samples: &[f32]) -> Result<(), String> {
            if let Some(delay) = self.write_delay {
                std::thread::sleep(delay);
            }
            if self
                .fail_after_frames
                .is_some_and(|limit| self.frames() + samples.len() > limit)
            {
                return Err("No space left on device (os error 28)".to_string());
            }
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Write(samples.len()));
            Ok(())
        }
        fn finish(&mut self) -> Result<(), String> {
            self.calls.lock().unwrap().push(SinkCall::Finish);
            Ok(())
        }
    }

    fn fast(chunk_frames: u64) -> RecorderConfig {
        RecorderConfig {
            poll_interval: Duration::from_millis(1),
            chunk_frames,
            ..RecorderConfig::default()
        }
    }

    fn wait_until(what: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn disk_recorder(
        tmp: &TempDir,
        chunk_frames: u64,
        source: &mut FakeSource,
    ) -> (TrackRecorder, FakeLedger) {
        let ledger = FakeLedger::default();
        let mut writer =
            ChunkWriterConfig::new(tmp.path().to_path_buf(), "m1", "t1", source.kind(), ORIGIN);
        writer.chunk_frames = chunk_frames;
        let (recorder, handler) =
            record_to_disk(writer, Box::new(ledger.clone()), fast(CHUNK_FRAMES)).unwrap();
        assert_eq!(source.start(Box::new(handler)).unwrap(), source.format);
        (recorder, ledger)
    }

    fn max_tone_error(samples: &[f32], first_frame: u64) -> f64 {
        samples
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let t = (first_frame + i as u64) as f64 / TARGET_SAMPLE_RATE as f64;
                (*s as f64 - 0.5 * (TAU * TONE_HZ * t).sin()).abs()
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn records_a_source_to_rotating_chunk_files() {
        let tmp = TempDir::new("recorder-disk");
        let mut source = FakeSource::new(STEREO_48K);
        // 1 s chunks instead of 60 s; the clock is the buffers' host time.
        let (mut recorder, ledger) = disk_recorder(&tmp, 16_000, &mut source);
        source.deliver(250);
        source.stop().unwrap();
        let status = recorder.stop();

        assert_eq!(status.error, None);
        assert!(status.finished);
        assert_eq!((status.overflow_frames, status.gaps), (0, 0));
        assert_eq!(status.written_frames, 40_000);
        let records = ledger.records();
        assert_eq!(
            records.iter().map(|r| r.n_frames).collect::<Vec<_>>(),
            [16_000, 16_000, 8_000]
        );
        assert_eq!(
            records.iter().map(|r| r.start_ms).collect::<Vec<_>>(),
            [0, 1_000, 2_000]
        );
        assert_eq!(records[1].anchor_host_ns, ORIGIN + 1_000 * MS);

        // Read back across both borders: the tone is intact and in phase, so
        // rotation neither dropped nor duplicated a sample.
        let mut audio = ChunkTrackAudio::new(tmp.path().to_path_buf(), records);
        assert_eq!(audio.duration_ms(), 2_500);
        let samples = audio.read(500, 1_800).unwrap();
        let error = max_tone_error(&samples, 8_000);
        assert!(error < 0.01, "max error {error}");
    }

    #[test]
    fn a_hole_in_host_time_becomes_a_new_chunk_with_a_gap() {
        let tmp = TempDir::new("recorder-gap");
        let mut source = FakeSource::new(STEREO_48K);
        let (mut recorder, ledger) = disk_recorder(&tmp, CHUNK_FRAMES, &mut source);
        source.deliver(50);
        source.skip_ms(250);
        source.deliver(50);
        source.skip_ms(40); // jitter-sized: not a gap
        source.deliver(10);
        let status = recorder.stop();

        assert_eq!(status.gaps, 1);
        let records = ledger.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].n_frames, 8_000);
        assert_eq!(records[1].n_frames, 9_600, "nothing zero-filled");
        assert_eq!(records[1].start_ms, 750);
        assert_eq!(records[1].anchor_host_ns, ORIGIN + 750 * MS);
        let sidecar = sidecar::read(&tmp.path().join("m1/system"))
            .unwrap()
            .unwrap();
        assert_eq!(
            sidecar
                .chunks
                .iter()
                .map(|c| c.gap_before_ms)
                .collect::<Vec<_>>(),
            [0, 250]
        );

        // The audio after the gap sits where it was captured.
        let mut audio = ChunkTrackAudio::new(tmp.path().to_path_buf(), records);
        let after = audio.read(800, 400).unwrap();
        assert!(max_tone_error(&after, 800 * 16) < 0.01);
        assert!(audio.read(550, 150).unwrap().iter().all(|s| *s == 0.0));
    }

    #[test]
    fn pause_and_resume_produce_a_gap_not_silence() {
        let sink = FakeSink::default();
        let mut source = FakeSource::new(STEREO_48K);
        let (mut recorder, handler) = start_track(
            TrackKind::Mic,
            Box::new(sink.clone()),
            ORIGIN,
            fast(CHUNK_FRAMES),
        )
        .unwrap();
        source.start(Box::new(handler)).unwrap();

        source.deliver(30);
        recorder.pause();
        assert!(recorder.status().paused);
        // The chunk is closed while paused, with everything from before.
        wait_until("the pause to close the chunk", || {
            sink.calls().last() == Some(&SinkCall::Finish)
        });
        assert_eq!(sink.frames(), 4_800);
        source.deliver(500); // 5 s of audio nobody wants
        recorder.resume();
        source.deliver(30);
        let status = recorder.stop();

        assert_eq!(status.error, None);
        assert_eq!(
            status.written_frames, 9_600,
            "paused audio is not on disk, nor is silence"
        );
        assert_eq!(status.overflow_frames, 0, "a pause is not an overflow");
        assert_eq!(status.gaps, 1);
        assert_eq!(sink.begins(), [ORIGIN, ORIGIN + 5_300 * MS]);
    }

    #[test]
    fn a_format_change_mid_meeting_stays_in_one_chunk() {
        let sink = FakeSink::default();
        let mut source = FakeSource::new(STEREO_48K);
        let (mut recorder, handler) = start_track(
            TrackKind::System,
            Box::new(sink.clone()),
            ORIGIN,
            fast(CHUNK_FRAMES),
        )
        .unwrap();
        source.start(Box::new(handler)).unwrap();
        source.deliver(100);
        // AirPods: the source rebuilds at 44.1 kHz mono, without a hole.
        let format = SourceFormat {
            sample_rate: 44_100,
            channels: 1,
        };
        source
            .handler
            .as_mut()
            .unwrap()
            .on_discontinuity(Discontinuity::FormatChanged { format }, source.host_ns);
        source.format = format;
        source.deliver(100);
        let status = recorder.stop();

        assert_eq!(status.error, None);
        assert_eq!(sink.begins(), [ORIGIN], "no new chunk");
        assert!(
            (status.written_frames as i64 - 32_000).abs() <= 1,
            "{}",
            status.written_frames
        );
    }

    #[test]
    fn rotation_follows_the_host_clock_not_the_sample_count() {
        let sink = FakeSink::default();
        let mut source = FakeSource::new(STEREO_48K);
        let (mut recorder, handler) =
            start_track(TrackKind::Mic, Box::new(sink.clone()), ORIGIN, fast(16_000)).unwrap();
        source.start(Box::new(handler)).unwrap();
        // A device clock 0.5 % slow: every 10 ms of samples takes 10.05 ms.
        source.deliver_stamped(350, 10 * MS + 50_000);
        let status = recorder.stop();

        assert_eq!(status.gaps, 0, "drift is never a gap");
        let begins = sink.begins();
        assert_eq!(begins.len(), 4);
        for (i, anchor) in begins.iter().enumerate() {
            let real = ORIGIN + i as u64 * 1_005 * MS;
            assert!(
                anchor.abs_diff(real) < MS,
                "chunk {i}: off by {} ns",
                anchor.abs_diff(real)
            );
        }
    }

    #[test]
    fn a_slow_sink_drops_and_counts_instead_of_growing() {
        let sink = FakeSink {
            write_delay: Some(Duration::from_millis(40)),
            ..FakeSink::default()
        };
        let mut source = FakeSource::new(STEREO_48K);
        // A ring of 0.25 s, and a disk that manages one write per 40 ms.
        // Threshold 0: every loss is a gap, none is padded, so the frame
        // accounting below is exact.
        let config = RecorderConfig {
            ring_samples: 24_000,
            gap_threshold_ms: 0,
            ..fast(CHUNK_FRAMES)
        };
        let (mut recorder, handler) =
            start_track(TrackKind::System, Box::new(sink.clone()), ORIGIN, config).unwrap();
        assert_eq!(
            handler.ring_capacity(),
            24_000,
            "the only place audio can queue up"
        );
        source.start(Box::new(handler)).unwrap();

        // 30 s of audio arrives as fast as the callback can deliver it. The
        // callback must never wait for the sink.
        let started = Instant::now();
        source.deliver(1_500);
        let mut callback_time = started.elapsed();
        // Let the writer catch up a little, so the second burst finds room
        // again and its first buffer lands after a hole.
        wait_until("the first write", || recorder.status().written_frames > 0);
        std::thread::sleep(Duration::from_millis(400));
        let started = Instant::now();
        source.deliver(1_500);
        callback_time += started.elapsed();
        let status = recorder.stop();

        assert_eq!(status.error, None);
        assert!(status.overflow_frames > 0, "the ring overflowed");
        assert!(
            callback_time < Duration::from_secs(5),
            "callbacks blocked: {callback_time:?}"
        );
        // Every frame is accounted for: written or counted as dropped.
        let delivered = 3_000 * 480;
        let accepted = delivered - status.overflow_frames;
        let written_native = status.written_frames * 3;
        let slack = 3 * (status.gaps + 1) * 2;
        assert!(
            written_native.abs_diff(accepted) <= slack,
            "{written_native} vs {accepted}"
        );
        // And the drops are gaps on the timeline, not a shifted recording.
        assert!(status.gaps > 0);
        assert_eq!(sink.begins().len() as u64, status.gaps + 1);
    }

    #[test]
    fn a_short_known_loss_is_padded_to_stay_aligned() {
        let sink = FakeSink::default();
        let mut source = FakeSource::new(STEREO_48K);
        let (mut recorder, handler) = start_track(
            TrackKind::System,
            Box::new(sink.clone()),
            ORIGIN,
            fast(CHUNK_FRAMES),
        )
        .unwrap();
        source.start(Box::new(handler)).unwrap();
        source.deliver(50);
        // An xrun: the driver lost 20 ms and says so.
        source
            .handler
            .as_mut()
            .unwrap()
            .on_discontinuity(Discontinuity::Dropped { frames: 960 }, source.host_ns);
        source.skip_ms(20);
        source.deliver(50);
        let status = recorder.stop();

        assert_eq!(status.source_dropped_frames, 960);
        assert_eq!(status.gaps, 0);
        assert_eq!(sink.begins(), [ORIGIN]);
        assert_eq!(
            status.written_frames, 16_320,
            "1 s of audio + 20 ms of padding"
        );
        assert_eq!(status.padded_frames, 320);
    }

    #[test]
    fn a_full_disk_is_an_error_state_not_a_panic() {
        let sink = FakeSink {
            fail_after_frames: Some(8_000),
            ..FakeSink::default()
        };
        let mut source = FakeSource::new(STEREO_48K);
        let (mut recorder, handler) = start_track(
            TrackKind::Mic,
            Box::new(sink.clone()),
            ORIGIN,
            fast(CHUNK_FRAMES),
        )
        .unwrap();
        source.start(Box::new(handler)).unwrap();
        source.deliver(100);
        wait_until("the error state", || recorder.status().error.is_some());

        let status = recorder.status();
        assert!(
            status.error.as_deref().unwrap().contains("No space left"),
            "{status:?}"
        );
        assert_eq!(
            sink.calls().last(),
            Some(&SinkCall::Finish),
            "what was written is closed"
        );
        // The audio path carries on unharmed and the ring does not fill up.
        source.deliver(1_000);
        source
            .handler
            .as_mut()
            .unwrap()
            .on_discontinuity(Discontinuity::Stalled, source.host_ns);
        recorder.pause();
        recorder.resume();
        let status = recorder.stop();
        assert!(status.finished && status.error.is_some());
        assert_eq!(status.overflow_frames, 0);
        assert_eq!(recorder.stop(), status, "stop is idempotent");
    }

    #[test]
    fn rejects_empty_rings_and_survives_odd_buffers() {
        let bad = RecorderConfig {
            ring_samples: 0,
            ..RecorderConfig::default()
        };
        assert!(start_track(TrackKind::Mic, Box::new(FakeSink::default()), ORIGIN, bad).is_err());

        let sink = FakeSink::default();
        let (mut recorder, mut handler) = start_track(
            TrackKind::Mic,
            Box::new(sink.clone()),
            ORIGIN,
            fast(CHUNK_FRAMES),
        )
        .unwrap();
        let mono_16k = SourceFormat {
            sample_rate: 16_000,
            channels: 1,
        };
        let no_channels = SourceFormat {
            sample_rate: 16_000,
            channels: 0,
        };
        handler.on_frames(AudioFrames {
            samples: &[],
            format: mono_16k,
            host_time_ns: ORIGIN,
        });
        handler.on_frames(AudioFrames {
            samples: &[0.1; 8],
            format: no_channels,
            host_time_ns: ORIGIN,
        });
        // Seven samples of stereo: three frames and a stray sample.
        let stereo_16k = SourceFormat {
            sample_rate: 16_000,
            channels: 2,
        };
        handler.on_frames(AudioFrames {
            samples: &[0.1; 7],
            format: stereo_16k,
            host_time_ns: ORIGIN,
        });
        let status = recorder.stop();
        assert_eq!(status.error, None);
        assert_eq!(status.written_frames, 3);
        assert_eq!(
            sink.calls(),
            [
                SinkCall::Begin(ORIGIN),
                SinkCall::Write(3),
                SinkCall::Finish
            ]
        );
    }
}
