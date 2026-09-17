//! OWNER: WP5 (capture). The real `types::AudioSource`s.
//!
//! `is_supported()` is called by `commands.rs` and its signature is fixed.
//! The session (WP7) builds a meeting's sources with `open_mic()` and
//! `open_system_tap()`; a system tap that cannot be opened or started is an
//! `Err` the session turns into a mic-only meeting, never a blocked one.
//!
//! ```text
//! let origin = capture::host_now_ns();          // shared by both recorders
//! let mut mic = capture::open_mic();
//! mic.start(mic_handler)?;                      // 1. returns after its first callback
//! if let Ok(mut tap) = capture::open_system_tap() {
//!     let monitor = tap.monitor();              //    poll monitor.notice() for the UI
//!     tap.start(tap_handler).ok();              // 2. only now: see F2 below
//! }
//! ```
//!
//! What the spike (`docs/spikes/TAP_CAPTURE_SPIKE.md`, branch
//! `spike/tap-capture`) found and this module is built around:
//!
//! - F1: the tap's reported format is not what the IOProc delivers. The rate
//!   comes from the aggregate device, re-read after every rebuild.
//! - F2: opening a Bluetooth microphone flips the headset into call mode,
//!   which changes the output device and stalls the HAL for seconds. Start
//!   the microphone first; `MicSource::start` returns after its first
//!   callback.
//! - F3: exact zeros are normal when nothing plays (`permission.rs`).
//! - F4: device changes come in bursts (`device_watch.rs`). Rebuilds happen
//!   on the sources' own threads and never block the caller.
//! - F5: while the permission is undetermined the IOProc may never fire. The
//!   tap then reports `SystemAudioNotice::NotDelivering` and keeps retrying;
//!   `start` still returns.
//! - Found while building this, on the built-in speakers (macOS 26.4): as
//!   long as no process plays anything the output device does not run and the
//!   IOProc does not fire at all, permission granted or not. It starts by
//!   itself with the first sound and then keeps running. So the system track
//!   simply begins at the first sound, and "no callbacks" only counts as a
//!   fault while another process is playing (`system_tap::PlayingProbe`).
//!
//! | File | What |
//! |---|---|
//! | `mic.rs` | `MicSource`: cpal 0.15 on a thread of its own |
//! | `system_tap.rs` | `SystemTapSource`: process tap, aggregate device, IOProc; the OS gate |
//! | `device_watch.rs` | HAL listeners, `RebuildPlanner`, `OutputGate`, echo risk |
//! | `permission.rs` | preflight, `SilenceDetector`, the System Settings link |
//! | `hal.rs`, `clock.rs` | Core Audio property helpers, the mach host clock |

// Nothing calls `open_mic` / `open_system_tap` until the session (WP7) lands.
// Remove once it does.
#![allow(dead_code)]

pub mod clock;
pub mod device_watch;
#[cfg(target_os = "macos")]
pub(crate) mod hal;
pub mod mic;
pub mod permission;
pub mod system_tap;

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use super::types::{AudioFrames, AudioSourceHandler, Discontinuity};

#[allow(unused_imports)]
pub use clock::host_now_ns;
#[allow(unused_imports)]
pub use device_watch::output_is_builtin_speakers;
#[allow(unused_imports)]
pub use mic::MicSource;
#[allow(unused_imports)]
pub use permission::SystemAudioNotice;
#[allow(unused_imports)]
pub use system_tap::{SystemAudioMonitor, SystemTapSource};

/// The runtime gate for the whole feature: macOS 14.4+, the
/// `CATapDescription` class present and both tap symbols resolvable.
/// Dictation never depends on it.
pub fn is_supported() -> bool {
    support().is_ok()
}

/// `is_supported`, with the reason when it is not. Checked once.
pub fn support() -> Result<(), String> {
    static SUPPORT: OnceLock<Result<(), String>> = OnceLock::new();
    SUPPORT.get_or_init(system_tap::check_support).clone()
}

/// The microphone: the default input device, like dictation. Nothing is
/// opened until `start`.
pub fn open_mic() -> MicSource {
    MicSource::new()
}

/// System audio. `Err` when this Mac cannot do process taps; the meeting is
/// then mic-only.
pub fn open_system_tap() -> Result<SystemTapSource, String> {
    SystemTapSource::new()
}

/// `"14.4.1"` to `(14, 4, 1)`. Missing parts are 0.
pub fn parse_os_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.trim().trim_end_matches('\0').split('.');
    let major = parts.next()?.parse().ok()?;
    let mut rest = [0u32; 2];
    for slot in &mut rest {
        match parts.next() {
            Some(part) => *slot = part.parse().ok()?,
            None => break,
        }
    }
    Some((major, rest[0], rest[1]))
}

/// Process taps exist from 14.2; the product decision is 14.4 or later.
pub fn version_supports_taps(version: (u32, u32, u32)) -> bool {
    (version.0, version.1) >= (14, 4)
}

/// Where a source keeps the recorder's handler.
///
/// `AudioSourceHandler` takes `&mut self` from two places: `on_frames` on the
/// real-time audio thread, `on_discontinuity` on the source's control thread
/// (and the recorder behind it has a single-producer ring). The slot makes
/// the two exclusive without ever blocking the audio thread: it *tries* to
/// take the slot and drops the buffer when the control side holds it, which
/// lasts microseconds and in practice only happens while the stream is
/// stopped anyway. Dropped frames are reported as `Discontinuity::Dropped`.
pub(crate) struct HandlerSlot {
    locked: AtomicBool,
    closed: AtomicBool,
    contended_frames: AtomicU64,
    handler: UnsafeCell<Option<Box<dyn AudioSourceHandler>>>,
}

// SAFETY: `handler` is only touched while `locked` is held, and the handler
// itself is `Send`.
unsafe impl Send for HandlerSlot {}
unsafe impl Sync for HandlerSlot {}

impl HandlerSlot {
    pub fn new(handler: Box<dyn AudioSourceHandler>) -> Self {
        Self {
            locked: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            contended_frames: AtomicU64::new(0),
            handler: UnsafeCell::new(Some(handler)),
        }
    }

    fn try_lock(&self) -> bool {
        self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }

    /// Real-time side. No allocation, no blocking.
    #[inline]
    pub fn frames(&self, frames: AudioFrames<'_>) {
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        if !self.try_lock() {
            self.contended_frames.fetch_add(frames.frame_count() as u64, Ordering::Relaxed);
            return;
        }
        // SAFETY: the lock is held.
        if let Some(handler) = unsafe { &mut *self.handler.get() } {
            handler.on_frames(frames);
        }
        self.unlock();
    }

    /// Control side: waits for a callback in flight (at most one buffer).
    fn with_handler(&self, f: impl FnOnce(&mut Option<Box<dyn AudioSourceHandler>>)) {
        while !self.try_lock() {
            std::thread::sleep(Duration::from_micros(50));
        }
        // SAFETY: the lock is held.
        f(unsafe { &mut *self.handler.get() });
        self.unlock();
    }

    pub fn discontinuity(&self, discontinuity: Discontinuity, host_time_ns: u64) {
        self.with_handler(|handler| {
            let Some(handler) = handler else { return };
            let contended = self.contended_frames.swap(0, Ordering::Relaxed);
            if contended > 0 {
                handler.on_discontinuity(Discontinuity::Dropped { frames: contended }, host_time_ns);
            }
            handler.on_discontinuity(discontinuity, host_time_ns);
        });
    }

    /// Stops delivery for good and drops the handler, even if a stream that
    /// could not be torn down in time is still calling back.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.with_handler(|handler| *handler = None);
    }
}

#[cfg(test)]
pub mod fake {
    //! A scripted `AudioSource` for tests of the packages downstream of
    //! capture (the session, WP7). It owns the handler like a real source
    //! and delivers whatever the test says, stamped with whatever host time
    //! the test says.

    use crate::meetings::types::{
        AudioFrames, AudioSource, AudioSourceHandler, Discontinuity, SourceFormat, TrackKind,
    };

    pub struct FakeSource {
        kind: TrackKind,
        format: SourceFormat,
        device_name: Option<String>,
        handler: Option<Box<dyn AudioSourceHandler>>,
        /// `start` fails with this: a denied or missing device.
        pub fail_start: Option<String>,
        pub starts: usize,
        pub stops: usize,
    }

    impl FakeSource {
        pub fn new(kind: TrackKind, format: SourceFormat) -> Self {
            Self {
                kind,
                format,
                device_name: Some(format!("Fake {}", kind.as_str())),
                handler: None,
                fail_start: None,
                starts: 0,
                stops: 0,
            }
        }

        pub fn failing(kind: TrackKind, error: &str) -> Self {
            let mut source = Self::new(kind, SourceFormat { sample_rate: 48_000, channels: 1 });
            source.fail_start = Some(error.to_string());
            source
        }

        pub fn is_started(&self) -> bool {
            self.handler.is_some()
        }

        /// Delivers one buffer of interleaved samples. Panics when not
        /// started, like a test bug should.
        pub fn deliver(&mut self, samples: &[f32], host_time_ns: u64) {
            let format = self.format;
            self.handler
                .as_mut()
                .expect("FakeSource is not started")
                .on_frames(AudioFrames { samples, format, host_time_ns });
        }

        /// `seconds` of a constant value in 10 ms buffers starting at
        /// `host_time_ns`. Returns the host time after the last buffer.
        pub fn deliver_constant(&mut self, value: f32, seconds: f64, host_time_ns: u64) -> u64 {
            let frames = self.format.sample_rate as usize / 100;
            let samples = vec![value; frames * self.format.channels as usize];
            let mut host_ns = host_time_ns;
            for _ in 0..(seconds * 100.0).round() as usize {
                self.deliver(&samples, host_ns);
                host_ns += 10_000_000;
            }
            host_ns
        }

        /// A rebuild onto another device: announces the format, then uses it.
        pub fn change_format(&mut self, format: SourceFormat, host_time_ns: u64) {
            self.format = format;
            self.discontinuity(Discontinuity::FormatChanged { format }, host_time_ns);
        }

        pub fn discontinuity(&mut self, discontinuity: Discontinuity, host_time_ns: u64) {
            if let Some(handler) = self.handler.as_mut() {
                handler.on_discontinuity(discontinuity, host_time_ns);
            }
        }
    }

    impl AudioSource for FakeSource {
        fn kind(&self) -> TrackKind {
            self.kind
        }

        fn device_name(&self) -> Option<String> {
            self.device_name.clone()
        }

        fn format(&self) -> Option<SourceFormat> {
            self.handler.is_some().then_some(self.format)
        }

        fn start(&mut self, handler: Box<dyn AudioSourceHandler>) -> Result<SourceFormat, String> {
            if let Some(error) = &self.fail_start {
                return Err(error.clone());
            }
            self.starts += 1;
            self.handler = Some(handler);
            Ok(self.format)
        }

        fn stop(&mut self) -> Result<(), String> {
            if self.handler.take().is_some() {
                self.stops += 1;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use crate::meetings::types::{AudioFrames, AudioSourceHandler, Discontinuity, SourceFormat};

    /// The hardware tests share one microphone, one output device and one
    /// pair of ears: one at a time, however `cargo test` is run.
    pub fn hardware_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[derive(Debug, Default)]
    pub struct Captured {
        pub buffers: usize,
        pub frames: usize,
        pub nonzero_samples: usize,
        pub first_host_ns: Option<u64>,
        pub last_host_ns: u64,
        pub formats: Vec<SourceFormat>,
        pub discontinuities: Vec<Discontinuity>,
    }

    /// Records what a source delivered. It locks, which a real handler must
    /// not do on the audio thread; good enough for a test.
    #[derive(Clone, Default)]
    pub struct CapturingHandler(pub Arc<Mutex<Captured>>);

    impl AudioSourceHandler for CapturingHandler {
        fn on_frames(&mut self, frames: AudioFrames<'_>) {
            let mut captured = self.0.lock().unwrap();
            captured.buffers += 1;
            captured.frames += frames.frame_count();
            captured.nonzero_samples += frames.samples.iter().filter(|s| **s != 0.0).count();
            captured.first_host_ns.get_or_insert(frames.host_time_ns);
            captured.last_host_ns = frames.host_time_ns;
            if captured.formats.last() != Some(&frames.format) {
                captured.formats.push(frames.format);
            }
        }

        fn on_discontinuity(&mut self, discontinuity: Discontinuity, _host_time_ns: u64) {
            self.0.lock().unwrap().discontinuities.push(discontinuity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeSource;
    use super::test_support::CapturingHandler;
    use super::*;
    use crate::meetings::types::{AudioSource, SourceFormat, TrackKind};
    use std::sync::Arc;

    #[test]
    fn os_versions_parse() {
        assert_eq!(parse_os_version("14.4.1"), Some((14, 4, 1)));
        assert_eq!(parse_os_version("14.4"), Some((14, 4, 0)));
        assert_eq!(parse_os_version("26.4.1\n"), Some((26, 4, 1)));
        assert_eq!(parse_os_version("15\0"), Some((15, 0, 0)));
        assert_eq!(parse_os_version("13.6.9"), Some((13, 6, 9)));
        for bad in ["", "fourteen", "14.x", "14..1", "-1.2"] {
            assert_eq!(parse_os_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_gate_opens_at_14_4() {
        for (version, supported) in [
            ((12, 7, 6), false),
            ((13, 4, 0), false),
            ((14, 0, 0), false),
            ((14, 2, 0), false),
            ((14, 3, 9), false),
            ((14, 4, 0), true),
            ((14, 10, 0), true),
            ((15, 0, 0), true),
            ((26, 4, 1), true),
        ] {
            assert_eq!(version_supports_taps(version), supported, "{version:?}");
        }
    }

    #[test]
    fn support_is_stable_and_explains_itself() {
        assert_eq!(is_supported(), support().is_ok());
        if let Err(reason) = support() {
            assert!(!reason.is_empty());
            assert!(open_system_tap().is_err());
        }
    }

    const MONO_16K: SourceFormat = SourceFormat { sample_rate: 16_000, channels: 1 };

    #[test]
    fn the_slot_delivers_frames_and_discontinuities_in_order() {
        let handler = CapturingHandler::default();
        let slot = HandlerSlot::new(Box::new(handler.clone()));
        slot.frames(AudioFrames { samples: &[0.5; 160], format: MONO_16K, host_time_ns: 7 });
        slot.discontinuity(Discontinuity::Stalled, 8);
        let captured = handler.0.lock().unwrap();
        assert_eq!((captured.buffers, captured.frames, captured.first_host_ns), (1, 160, Some(7)));
        assert_eq!(captured.discontinuities, vec![Discontinuity::Stalled]);
    }

    #[test]
    fn a_contended_buffer_is_dropped_and_reported_not_waited_for() {
        let handler = CapturingHandler::default();
        let slot = HandlerSlot::new(Box::new(handler.clone()));
        // The control side holds the slot while the audio thread calls in.
        assert!(slot.try_lock());
        slot.frames(AudioFrames { samples: &[0.5; 320], format: MONO_16K, host_time_ns: 1 });
        slot.unlock();
        assert_eq!(handler.0.lock().unwrap().buffers, 0);

        slot.discontinuity(Discontinuity::FormatChanged { format: MONO_16K }, 2);
        assert_eq!(
            handler.0.lock().unwrap().discontinuities,
            vec![
                Discontinuity::Dropped { frames: 320 },
                Discontinuity::FormatChanged { format: MONO_16K }
            ]
        );
    }

    #[test]
    fn a_closed_slot_drops_the_handler_and_ignores_late_callbacks() {
        let handler = CapturingHandler::default();
        let slot = HandlerSlot::new(Box::new(handler.clone()));
        assert_eq!(Arc::strong_count(&handler.0), 2);
        slot.close();
        assert_eq!(Arc::strong_count(&handler.0), 1, "the handler was dropped");
        slot.frames(AudioFrames { samples: &[0.5; 16], format: MONO_16K, host_time_ns: 1 });
        slot.discontinuity(Discontinuity::Stalled, 2);
        assert_eq!(handler.0.lock().unwrap().buffers, 0);
    }

    #[test]
    fn the_slot_survives_a_real_race() {
        let handler = CapturingHandler::default();
        let slot = Arc::new(HandlerSlot::new(Box::new(handler.clone())));
        let audio = {
            let slot = slot.clone();
            std::thread::spawn(move || {
                for n in 0..20_000u64 {
                    slot.frames(AudioFrames { samples: &[0.25; 8], format: MONO_16K, host_time_ns: n });
                }
            })
        };
        for n in 0..200 {
            slot.discontinuity(Discontinuity::Stalled, n);
        }
        audio.join().unwrap();
        slot.discontinuity(Discontinuity::Stalled, 0);
        let captured = handler.0.lock().unwrap();
        let dropped: u64 = captured
            .discontinuities
            .iter()
            .map(|d| match d {
                Discontinuity::Dropped { frames } => *frames,
                _ => 0,
            })
            .sum();
        assert_eq!(captured.frames as u64 + dropped, 160_000, "every frame is delivered or accounted for");
    }

    /// The whole path the session (WP7) will wire: both real sources, in the
    /// order it must start them, into WP4's recorder.
    #[test]
    #[ignore = "needs a microphone, audio output, both permissions, and plays a sound"]
    fn both_sources_record_through_the_recorder_on_one_timeline() {
        let _hardware = super::test_support::hardware_lock();
        use crate::meetings::recording::{start_track, RecorderConfig};
        use crate::meetings::types::{SampleSink, TARGET_SAMPLE_RATE};
        use std::sync::Mutex;

        #[derive(Default)]
        struct Runs(Mutex<Vec<(u64, Vec<f32>)>>);
        struct Sink(Arc<Runs>);
        impl SampleSink for Sink {
            fn begin(&mut self, anchor_host_ns: u64) -> Result<(), String> {
                self.0 .0.lock().unwrap().push((anchor_host_ns, Vec::new()));
                Ok(())
            }
            fn write(&mut self, samples: &[f32]) -> Result<(), String> {
                self.0 .0.lock().unwrap().last_mut().unwrap().1.extend_from_slice(samples);
                Ok(())
            }
            fn finish(&mut self) -> Result<(), String> {
                Ok(())
            }
        }

        let origin = host_now_ns();
        let (mic_runs, tap_runs) = (Arc::new(Runs::default()), Arc::new(Runs::default()));
        let (mut mic_recorder, mic_handler) =
            start_track(TrackKind::Mic, Box::new(Sink(mic_runs.clone())), origin, RecorderConfig::default()).unwrap();
        let (mut tap_recorder, tap_handler) =
            start_track(TrackKind::System, Box::new(Sink(tap_runs.clone())), origin, RecorderConfig::default()).unwrap();

        let mut mic: Box<dyn AudioSource> = Box::new(open_mic());
        mic.start(Box::new(mic_handler)).expect("microphone starts");
        let mut tap = open_system_tap().expect("process taps are supported");
        let monitor = tap.monitor();
        tap.start(Box::new(tap_handler)).expect("the tap starts");

        std::thread::sleep(Duration::from_secs(1));
        let sound_at = host_now_ns();
        let _ = std::process::Command::new("afplay").arg("/System/Library/Sounds/Submarine.aiff").status();
        std::thread::sleep(Duration::from_secs(2));
        mic.stop().unwrap();
        tap.stop().unwrap();
        let (mic_status, tap_status) = (mic_recorder.stop(), tap_recorder.stop());
        println!("recorder: mic {mic_status:?}\nrecorder: tap {tap_status:?}\nrecorder: notice {:?}", monitor.notice());

        assert_eq!((mic_status.error, tap_status.error), (None, None));
        assert_eq!((mic_status.overflow_frames, tap_status.overflow_frames), (0, 0));
        let rate = TARGET_SAMPLE_RATE as u64;
        assert!(mic_status.written_frames > 3 * rate, "the microphone recorded throughout");
        assert!(tap_status.written_frames > rate, "the tap recorded from the sound on");

        // The sound sits where the host clock says it was played: the tap
        // track's first loud sample comes shortly after `afplay` was launched
        // (its start-up and the sound's soft attack included). A sanity check
        // of the timeline, not a measurement of alignment.
        let runs = tap_runs.0.lock().unwrap();
        let loud_at = runs.iter().find_map(|(anchor, samples)| {
            let index = samples.iter().position(|s| s.abs() > 0.01)?;
            Some(anchor + index as u64 * 1_000_000_000 / rate)
        });
        let loud_at = loud_at.expect("the tap track holds the sound");
        let offset_ms = (loud_at as i64 - sound_at as i64) / 1_000_000;
        println!("recorder: the sound is {offset_ms} ms after afplay was launched");
        assert!((0..=1_500).contains(&offset_ms), "{offset_ms} ms");
        assert!(mic_runs.0.lock().unwrap()[0].0 >= origin);
    }

    #[test]
    fn the_fake_source_behaves_like_a_source() {
        let handler = CapturingHandler::default();
        let mut source = FakeSource::new(TrackKind::System, SourceFormat { sample_rate: 48_000, channels: 2 });
        assert_eq!(source.format(), None);
        let format = source.start(Box::new(handler.clone())).unwrap();
        assert_eq!(source.format(), Some(format));
        let end = source.deliver_constant(0.1, 0.5, 1_000);
        assert_eq!(end, 1_000 + 500_000_000);
        source.change_format(SourceFormat { sample_rate: 24_000, channels: 2 }, end);
        source.deliver_constant(0.1, 0.1, end);
        source.stop().unwrap();
        source.stop().unwrap();
        assert_eq!((source.starts, source.stops), (1, 1));

        let captured = handler.0.lock().unwrap();
        assert_eq!(captured.frames, 24_000 + 2_400);
        assert_eq!(captured.formats.len(), 2);
        assert_eq!(captured.discontinuities.len(), 1);

        let mut denied = FakeSource::failing(TrackKind::System, "no permission");
        assert_eq!(denied.start(Box::new(CapturingHandler::default())).unwrap_err(), "no permission");
        assert!(!denied.is_started());
    }
}
