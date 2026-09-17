//! OWNER: WP5 (capture). The microphone as a `types::AudioSource`.
//!
//! - `cpal` 0.15, the default input device, like dictation (`audio.rs`). Do
//!   not upgrade cpal.
//! - `cpal::Stream` is `!Send`: it lives on the `meeting-mic` thread, which
//!   also does every rebuild. `start` returns after the first callback,
//!   because the system tap must not be built before that (spike finding F2).
//! - Runs next to a dictation capture in `audio.rs`. Each is its own cpal
//!   stream (its own AUHAL instance) on the device; they share nothing.
//! - Rebuilds on a default-input change, a sample-rate change, a stream error
//!   or a stall, after the rules in `device_watch.rs`, and never before the
//!   output side is running again (F4).
//!
//! Timestamps. cpal 0.15.3 builds `timestamp().callback` from the render
//! callback's `mHostTime`, so it is on the mach host clock, and the spike's
//! data shows it is the time of the buffer's *first frame* (it lags "now" by
//! one buffer plus about a millisecond). `timestamp().capture` subtracts the
//! buffer once more and is one buffer early. `StreamInstant` is opaque, so
//! the absolute value is not available: `MicClock` anchors the first callback
//! on the host clock and advances by the difference between `callback`
//! instants, which keeps the device's jitter-free spacing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::clock::{self, Timebase};
use super::device_watch::{
    self, output_gate, Action, DeviceEvent, Hold, PlannerConfig, RebuildPlanner, RebuildReason,
};
use super::HandlerSlot;
use crate::meetings::types::{
    AudioFrames, AudioSource, AudioSourceHandler, Discontinuity, SourceFormat, TrackKind,
};

/// Opening a Bluetooth microphone took up to 9.9 s in the spike.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long `stop` waits for the thread before it lets it finish on its own.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// No callbacks for this long, without any device event, is a stall.
const STALL_TIMEOUT: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(250);
const FIRST_CALLBACK_TICK: Duration = Duration::from_millis(5);
const HELD_TICK: Duration = Duration::from_millis(25);
/// The anchor is off by more than this: the clocks were torn apart. Re-anchor.
const RESYNC_NS: u64 = 500_000_000;

fn log(level: &str, message: &str) {
    #[cfg(not(test))]
    let _ = crate::storage::append_log(level, message);
    #[cfg(test)]
    let _ = (level, message);
}

/// Host time of a buffer's first frame, from the device's own spacing.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct MicClock {
    /// Host time of the first callback's first frame.
    anchor_ns: Option<u64>,
}

impl MicClock {
    /// `now_ns`: the host clock in the callback. `buffer_ns`: the buffer's
    /// duration. `device_elapsed_ns`: the callback instant minus the first
    /// callback's instant.
    ///
    /// "Now minus the buffer" is the latest the first frame can have been
    /// captured; it is late by the scheduling delay of this callback. The
    /// anchor keeps the smallest delay seen, so stamps converge on the device
    /// times within a few callbacks and are then jitter-free. Real-time safe.
    pub fn stamp(&mut self, now_ns: u64, buffer_ns: u64, device_elapsed_ns: u64) -> u64 {
        let latest = now_ns.saturating_sub(buffer_ns);
        let Some(anchor_ns) = self.anchor_ns else {
            self.anchor_ns = Some(latest.saturating_sub(device_elapsed_ns));
            return latest;
        };
        let stamped = anchor_ns.saturating_add(device_elapsed_ns);
        if stamped > latest || latest - stamped > RESYNC_NS {
            self.anchor_ns = Some(latest.saturating_sub(device_elapsed_ns));
            return latest;
        }
        stamped
    }
}

enum Msg {
    Device(DeviceEvent),
    StreamError,
    Stop,
}

#[derive(Default)]
struct Shared {
    callbacks: AtomicU64,
    format: Mutex<Option<SourceFormat>>,
    device_name: Mutex<Option<String>>,
}

impl Shared {
    fn set(&self, format: Option<SourceFormat>, device_name: Option<String>) {
        *self.format.lock().unwrap_or_else(|e| e.into_inner()) = format;
        if device_name.is_some() {
            *self.device_name.lock().unwrap_or_else(|e| e.into_inner()) = device_name;
        }
    }
}

struct Worker {
    tx: Sender<Msg>,
    thread: JoinHandle<()>,
    slot: Arc<HandlerSlot>,
}

pub struct MicSource {
    shared: Arc<Shared>,
    worker: Option<Worker>,
}

impl MicSource {
    pub fn new() -> Self {
        Self { shared: Arc::new(Shared::default()), worker: None }
    }
}

impl Default for MicSource {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioSource for MicSource {
    fn kind(&self) -> TrackKind {
        TrackKind::Mic
    }

    fn device_name(&self) -> Option<String> {
        self.shared.device_name.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn format(&self) -> Option<SourceFormat> {
        *self.shared.format.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Blocks until the microphone has delivered its first buffer (seconds,
    /// for a Bluetooth headset). Call it from a thread that may block.
    fn start(&mut self, handler: Box<dyn AudioSourceHandler>) -> Result<SourceFormat, String> {
        if self.worker.is_some() {
            return Err("The microphone is already recording".to_string());
        }
        let slot = Arc::new(HandlerSlot::new(handler));
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = {
            let (slot, shared, tx) = (slot.clone(), self.shared.clone(), tx.clone());
            std::thread::Builder::new()
                .name("meeting-mic".to_string())
                .spawn(move || run(slot, shared, rx, tx, ready_tx))
                .map_err(|e| format!("Failed to start the microphone thread: {e}"))?
        };
        self.worker = Some(Worker { tx, thread, slot });
        let result = match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err("The microphone did not start in time".to_string()),
        };
        if result.is_err() {
            let _ = self.stop();
        }
        result
    }

    fn stop(&mut self) -> Result<(), String> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let _ = worker.tx.send(Msg::Stop);
        // Nothing reaches the recorder after this, even if the thread is
        // stuck in a HAL call for a while longer.
        worker.slot.close();
        join_with_timeout(worker.thread, STOP_TIMEOUT, "microphone");
        self.shared.set(None, None);
        Ok(())
    }
}

impl Drop for MicSource {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Joins `thread`, but not for ever: a HAL call can hang for seconds while a
/// Bluetooth device goes away. A thread that misses the deadline finishes and
/// cleans up on its own.
pub(crate) fn join_with_timeout(thread: JoinHandle<()>, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            log("WARN", &format!("Meetings: the {what} thread is still shutting down; not waiting for it"));
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if thread.join().is_err() {
        log("ERROR", &format!("Meetings: the {what} thread panicked"));
    }
}

struct Built {
    /// Dropping it stops the stream.
    _stream: cpal::Stream,
    format: SourceFormat,
    /// The HAL's id of the device, to tell a real change from a flap back.
    device: Option<u32>,
    device_rate: Option<f64>,
}

fn hal_default_input() -> (Option<u32>, Option<f64>) {
    #[cfg(target_os = "macos")]
    {
        let device = super::hal::default_input_device().ok();
        (device, device.and_then(super::hal::nominal_sample_rate))
    }
    #[cfg(not(target_os = "macos"))]
    (None, None)
}

fn build(slot: &Arc<HandlerSlot>, shared: &Arc<Shared>, errors: &Sender<Msg>) -> Result<Built, String> {
    let (hal_device, device_rate) = hal_default_input();
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| {
        "Microphone access required. Open System Settings > Privacy & Security > Microphone and enable FlowingThoughts.".to_string()
    })?;
    let name = device.name().ok();
    let config = device
        .default_input_config()
        .map_err(|e| format!("Failed to read the microphone's format: {e}"))?;
    let format = SourceFormat { sample_rate: config.sample_rate().0, channels: config.channels() };
    if format.sample_rate == 0 || format.channels == 0 {
        return Err("The microphone reports an empty format".to_string());
    }
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

    let errors = errors.clone();
    let on_error = move |_err: cpal::StreamError| {
        let _ = errors.send(Msg::StreamError);
    };
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let mut deliver = deliverer(slot.clone(), shared.clone(), format);
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], info: &cpal::InputCallbackInfo| deliver(data, info),
                on_error,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let mut deliver = deliverer(slot.clone(), shared.clone(), format);
            let mut scratch = Vec::<f32>::with_capacity(SCRATCH_SAMPLES);
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], info: &cpal::InputCallbackInfo| {
                    scratch.clear();
                    scratch.extend(data.iter().map(|s| *s as f32 / 32_768.0));
                    deliver(&scratch, info)
                },
                on_error,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let mut deliver = deliverer(slot.clone(), shared.clone(), format);
            let mut scratch = Vec::<f32>::with_capacity(SCRATCH_SAMPLES);
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], info: &cpal::InputCallbackInfo| {
                    scratch.clear();
                    scratch.extend(data.iter().map(|s| (*s as f32 - 32_768.0) / 32_768.0));
                    deliver(&scratch, info)
                },
                on_error,
                None,
            )
        }
        other => return Err(format!("Unsupported microphone sample format: {other:?}")),
    }
    .map_err(|e| format!("Failed to open the microphone: {e}"))?;
    stream.play().map_err(|e| format!("Failed to start the microphone: {e}"))?;

    shared.set(Some(format), name);
    Ok(Built { _stream: stream, format, device: hal_device, device_rate })
}

/// Room for the largest buffer a device is likely to hand over, so the
/// integer-format callbacks do not allocate.
const SCRATCH_SAMPLES: usize = 1 << 15;

/// The body of the audio callback: stamp, hand to the slot, count.
fn deliverer(
    slot: Arc<HandlerSlot>,
    shared: Arc<Shared>,
    format: SourceFormat,
) -> impl FnMut(&[f32], &cpal::InputCallbackInfo) + Send + 'static {
    let timebase: Timebase = clock::timebase();
    let mut mic_clock = MicClock::default();
    let mut first: Option<cpal::StreamInstant> = None;
    move |samples, info| {
        let now_ns = clock::now_ns(timebase);
        let callback = info.timestamp().callback;
        let elapsed = first.and_then(|first| callback.duration_since(&first));
        let elapsed_ns = match elapsed {
            Some(elapsed) => elapsed.as_nanos() as u64,
            // The first callback, or the device clock went backwards.
            None => {
                first = Some(callback);
                mic_clock = MicClock::default();
                0
            }
        };
        let frames = samples.len() / format.channels as usize;
        let buffer_ns = clock::frames_to_ns(frames as u64, format.sample_rate);
        let host_time_ns = mic_clock.stamp(now_ns, buffer_ns, elapsed_ns);
        slot.frames(AudioFrames { samples, format, host_time_ns });
        shared.callbacks.fetch_add(1, Ordering::Relaxed);
    }
}

/// Whether a device event needs a rebuild at all. The default device often
/// flaps away and back; a stream that is still delivering from the device
/// that is the default again, at the same rate, is left alone.
fn still_valid(built: &Built, delivering: bool) -> bool {
    let (device, rate) = hal_default_input();
    delivering && device.is_some() && device == built.device && rate == built.device_rate
}

fn run(
    slot: Arc<HandlerSlot>,
    shared: Arc<Shared>,
    msgs: Receiver<Msg>,
    msg_tx: Sender<Msg>,
    ready: Sender<Result<SourceFormat, String>>,
) {
    let mut ready = Some(ready);
    let started = Instant::now();
    let mut built = match build(&slot, &shared, &msg_tx) {
        Ok(built) => Some(built),
        Err(e) => {
            log("ERROR", &format!("Meetings: microphone: {e}"));
            let _ = ready.take().map(|r| r.send(Err(e)));
            return;
        }
    };
    log(
        "INFO",
        &format!(
            "Meetings: microphone opened in {} ms ({} Hz, {} ch)",
            started.elapsed().as_millis(),
            built.as_ref().map_or(0, |b| b.format.sample_rate),
            built.as_ref().map_or(0, |b| b.format.channels),
        ),
    );

    #[cfg(target_os = "macos")]
    let _subscription = {
        let tx = msg_tx.clone();
        device_watch::subscribe(move |event| {
            if !matches!(event, DeviceEvent::DefaultOutputChanged) {
                let _ = tx.send(Msg::Device(event));
            }
        })
        .map_err(|e| log("WARN", &format!("Meetings: microphone: {e}")))
        .ok()
    };
    let mut watched = None;
    rewatch(&mut watched, built.as_ref().and_then(|b| b.device));

    let mut planner = RebuildPlanner::new(PlannerConfig::default(), Instant::now());
    let mut seen_callbacks = shared.callbacks.load(Ordering::Relaxed);
    // When the current stream last delivered. `None`: not yet.
    let mut last_progress: Option<Instant> = None;
    let mut event_at: Option<Instant> = None;

    loop {
        let now = Instant::now();
        let tick = if planner.is_awaiting_first_callback() {
            FIRST_CALLBACK_TICK
        } else if output_gate().is_busy() {
            // Held back by the output side: look again soon.
            HELD_TICK
        } else {
            TICK
        };
        // Never zero: a rebuild that is held back must not spin.
        let timeout = planner.next_deadline(now).map_or(tick, |d| d.clamp(FIRST_CALLBACK_TICK, tick));
        match msgs.recv_timeout(timeout) {
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Msg::StreamError) => {
                event_at.get_or_insert(Instant::now());
                planner.on_event(Instant::now(), RebuildReason::StreamError);
            }
            Ok(Msg::Device(event)) => {
                let reason = match event {
                    DeviceEvent::DefaultInputChanged => Some(RebuildReason::DefaultDeviceChanged),
                    DeviceEvent::SampleRateChanged { device } if Some(device) == watched => {
                        Some(RebuildReason::SampleRateChanged)
                    }
                    _ => None,
                };
                if let Some(reason) = reason {
                    event_at.get_or_insert(Instant::now());
                    planner.on_event(Instant::now(), reason);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        let now = Instant::now();
        let callbacks = shared.callbacks.load(Ordering::Relaxed);
        if callbacks != seen_callbacks {
            seen_callbacks = callbacks;
            last_progress = Some(now);
            if planner.is_waiting_for_callbacks() {
                planner.on_first_callback();
                if let (Some(ready), Some(built)) = (ready.take(), built.as_ref()) {
                    let _ = ready.send(Ok(built.format));
                }
                if let Some(event_at) = event_at.take() {
                    log("INFO", &format!("Meetings: microphone recovered {} ms after the device event", event_at.elapsed().as_millis()));
                }
            }
        } else if planner.is_running()
            && last_progress.is_some_and(|at| now.duration_since(at) > STALL_TIMEOUT)
        {
            log("WARN", "Meetings: microphone stalled; rebuilding");
            slot.discontinuity(Discontinuity::Stalled, clock::host_now_ns());
            event_at.get_or_insert(now);
            planner.on_event(now, RebuildReason::Stalled);
        }

        match planner.poll(now, Hold { settle: output_gate().is_busy(), retry: false }) {
            Action::Wait => {}
            Action::Degraded => {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(Err("The microphone did not deliver any audio".to_string()));
                    break;
                }
                log("WARN", "Meetings: microphone is not delivering audio; will retry");
                slot.discontinuity(Discontinuity::Stalled, clock::host_now_ns());
            }
            Action::Rebuild(reason) => {
                let delivering =
                    last_progress.is_some_and(|at| now.duration_since(at) < Duration::from_millis(600));
                let device_event = matches!(
                    reason,
                    RebuildReason::DefaultDeviceChanged | RebuildReason::SampleRateChanged
                );
                if device_event && built.as_ref().is_some_and(|b| still_valid(b, delivering)) {
                    // Flapped back to the device the stream is still on.
                    planner.on_built(now, true);
                    planner.on_first_callback();
                    event_at = None;
                    continue;
                }
                let t = Instant::now();
                // Stop the old stream before opening the new one.
                built = None;
                let result = build(&slot, &shared, &msg_tx);
                seen_callbacks = shared.callbacks.load(Ordering::Relaxed);
                last_progress = None;
                planner.on_built(Instant::now(), result.is_ok());
                match result {
                    Ok(new) => {
                        log(
                            "INFO",
                            &format!(
                                "Meetings: microphone rebuilt in {} ms ({}; {} Hz, {} ch)",
                                t.elapsed().as_millis(),
                                reason.as_str(),
                                new.format.sample_rate,
                                new.format.channels
                            ),
                        );
                        slot.discontinuity(
                            Discontinuity::FormatChanged { format: new.format },
                            clock::host_now_ns(),
                        );
                        rewatch(&mut watched, new.device);
                        built = Some(new);
                    }
                    Err(e) => {
                        log("WARN", &format!("Meetings: microphone rebuild failed ({}): {e}", reason.as_str()));
                        shared.set(None, None);
                    }
                }
            }
        }
    }

    drop(built);
    rewatch(&mut watched, None);
}

/// Moves the sample-rate listener to `device`.
fn rewatch(watched: &mut Option<u32>, device: Option<u32>) {
    if *watched == device {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(old) = *watched {
            device_watch::unwatch_sample_rate(old);
        }
        if let Some(new) = device {
            device_watch::watch_sample_rate(new);
        }
    }
    *watched = device;
}

#[cfg(test)]
mod tests {
    use super::super::test_support::CapturingHandler;
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn the_first_stamp_is_now_minus_the_buffer() {
        let mut mic_clock = MicClock::default();
        assert_eq!(mic_clock.stamp(1_000 * MS, 20 * MS, 0), 980 * MS);
    }

    #[test]
    fn later_stamps_follow_the_device_spacing_not_the_callback_jitter() {
        let mut mic_clock = MicClock::default();
        // First callback 1.3 ms after its buffer ended.
        let t0 = 5_000 * MS;
        assert_eq!(mic_clock.stamp(t0, 20 * MS, 0), t0 - 20 * MS);
        // The next ones arrive up to 3 ms later than that: the stamps do not
        // move with them.
        for (n, jitter_ms) in [(1u64, 3u64), (2, 1), (3, 2)] {
            let now = t0 + n * 20 * MS + jitter_ms * MS;
            assert_eq!(mic_clock.stamp(now, 20 * MS, n * 20 * MS), t0 - 20 * MS + n * 20 * MS);
        }
    }

    #[test]
    fn a_late_first_callback_is_corrected_by_the_first_prompt_one() {
        let mut mic_clock = MicClock::default();
        // Buffer n holds the audio from t0 + 20n ms and is due 21 ms later.
        let t0 = 5_000 * MS;
        // The first callback was held up for 40 ms, so the next two come
        // right behind it. Each stamp is as early as its callback allows.
        assert_eq!(mic_clock.stamp(t0 + 61 * MS, 20 * MS, 0), t0 + 41 * MS);
        assert_eq!(mic_clock.stamp(t0 + 62 * MS, 20 * MS, 20 * MS), t0 + 42 * MS);
        assert_eq!(mic_clock.stamp(t0 + 63 * MS, 20 * MS, 40 * MS), t0 + 43 * MS);
        // Back on time: the anchor settles 1 ms late instead of 41 ms, and
        // jitter no longer moves the stamps.
        assert_eq!(mic_clock.stamp(t0 + 81 * MS, 20 * MS, 60 * MS), t0 + 61 * MS);
        assert_eq!(mic_clock.stamp(t0 + 104 * MS, 20 * MS, 80 * MS), t0 + 81 * MS);
        assert_eq!(mic_clock.stamp(t0 + 122 * MS, 20 * MS, 100 * MS), t0 + 101 * MS);
    }

    #[test]
    fn clocks_that_were_torn_apart_are_re_anchored() {
        let mut mic_clock = MicClock::default();
        let t0 = 5_000 * MS;
        mic_clock.stamp(t0, 20 * MS, 0);
        // The host clock is suddenly 2 s ahead of the device spacing.
        let now = t0 + 2_020 * MS;
        assert_eq!(mic_clock.stamp(now, 20 * MS, 20 * MS), now - 20 * MS);
        assert_eq!(mic_clock.stamp(now + 20 * MS, 20 * MS, 40 * MS), now);
    }

    #[test]
    fn stop_without_start_is_fine_and_a_new_source_has_no_format() {
        let mut mic = open();
        assert_eq!(mic.kind(), TrackKind::Mic);
        assert_eq!(mic.format(), None);
        mic.stop().unwrap();
        mic.stop().unwrap();
    }

    fn open() -> MicSource {
        super::super::open_mic()
    }

    #[test]
    #[ignore = "needs a microphone and the Microphone permission"]
    fn five_seconds_from_the_microphone_are_not_all_zeros() {
        let _hardware = super::super::test_support::hardware_lock();
        let handler = CapturingHandler::default();
        let mut mic = open();
        let before = clock::host_now_ns();
        let format = mic.start(Box::new(handler.clone())).expect("microphone starts");
        assert_eq!(mic.format(), Some(format));
        assert!(mic.device_name().is_some());
        std::thread::sleep(Duration::from_secs(5));
        mic.stop().unwrap();
        let after = clock::host_now_ns();

        let captured = handler.0.lock().unwrap();
        let seconds = captured.frames as f64 / format.sample_rate as f64;
        println!(
            "mic: {:?} {format:?}: {} buffers, {seconds:.2} s, {} non-zero samples",
            mic.device_name(),
            captured.buffers,
            captured.nonzero_samples
        );
        assert!((4.5..=5.5).contains(&seconds), "{seconds} s of audio in 5 s");
        assert!(captured.nonzero_samples > 0, "the microphone delivered only zeros");
        // Stamps are on the host clock: between start and stop, in order.
        let first = captured.first_host_ns.unwrap();
        assert!(first + 1_000_000_000 > before && captured.last_host_ns < after);
        assert!(captured.last_host_ns - first > 4_000_000_000);
    }

    #[test]
    #[ignore = "needs a microphone and the Microphone permission"]
    fn two_streams_on_the_same_microphone_coexist() {
        let _hardware = super::super::test_support::hardware_lock();
        // What a dictation during a meeting amounts to: a second cpal input
        // stream on the device, opened and closed while ours keeps running.
        let handler = CapturingHandler::default();
        let mut mic = open();
        mic.start(Box::new(handler.clone())).expect("microphone starts");
        std::thread::sleep(Duration::from_secs(1));
        let dictation = crate::audio::start_recording().expect("dictation starts next to the meeting");
        std::thread::sleep(Duration::from_secs(2));
        drop(dictation);
        let buffers_before = handler.0.lock().unwrap().buffers;
        std::thread::sleep(Duration::from_secs(1));
        let buffers_after = handler.0.lock().unwrap().buffers;
        mic.stop().unwrap();
        assert!(buffers_after > buffers_before, "the meeting stream survived the dictation stream");
        let captured = handler.0.lock().unwrap();
        assert!(
            !captured.discontinuities.iter().any(|d| matches!(d, Discontinuity::Stalled)),
            "{:?}",
            captured.discontinuities
        );
    }
}
