//! OWNER: WP5 (capture). System audio as a `types::AudioSource`, through a
//! Core Audio process tap (`objc2-core-audio` 0.3).
//!
//! - `AudioHardwareCreateProcessTap` / `AudioHardwareDestroyProcessTap` are
//!   resolved with `libc::dlsym`, never through the crate's `extern`
//!   declarations, so the binary still loads on macOS 12/13.
//!   `nm -um <binary> | grep ProcessTap` must print nothing.
//! - Global stereo tap, unmuted, private. Aggregate device with the default
//!   output as a real main sub-device, drift compensation and tap auto-start
//!   on. The dictionaries follow cpal 0.18's `loopback.rs` (Apache-2.0).
//! - The IOProc is a plain C function that hands the buffer to the
//!   `HandlerSlot` and counts. Nothing else happens on that thread.
//! - Everything else happens on the `meeting-tap` thread: building, the
//!   rebuild on an output change (`device_watch.rs`), the stall and
//!   first-callback timeouts, and the once-a-second look at the track that
//!   feeds `permission::SilenceDetector`. The HAL can block for seconds while
//!   a Bluetooth device comes or goes; only that thread ever waits for it.
//! - The format comes from the aggregate's nominal sample rate, re-read after
//!   every rebuild (spike finding F1), and from the buffers the IOProc
//!   actually gets. `kAudioTapPropertyFormat` is only trusted for "float32"
//!   and "interleaved or not".
//! - The tap and the aggregate are always destroyed on stop. Aggregates a
//!   previous run of this process left behind are recognised by their UID
//!   prefix and destroyed at start.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use super::permission::SystemAudioNotice;
use crate::meetings::types::SourceFormat;

/// Aggregate devices are named `<prefix><pid>.<n>`, so leftovers can be told
/// apart from everything else on the system, and from a live second copy of
/// the app.
pub const AGGREGATE_UID_PREFIX: &str = "com.flowingthoughts.meetings.tap-aggregate.";

/// The pid in an aggregate UID of ours. `None` for every other device.
pub fn aggregate_owner_pid(uid: &str) -> Option<u32> {
    let (pid, instance) = uid.strip_prefix(AGGREGATE_UID_PREFIX)?.split_once('.')?;
    instance.parse::<u32>().ok()?;
    pid.parse().ok()
}

/// Whether the device with `uid` is one of our aggregates that nobody uses
/// any more: made by this process but not in `live` (left by a start that
/// went wrong), or made by a process that is gone.
pub fn is_leaked_aggregate(
    uid: &str,
    own_pid: u32,
    live: &[String],
    pid_is_alive: impl Fn(u32) -> bool,
) -> bool {
    match aggregate_owner_pid(uid) {
        Some(pid) if pid == own_pid => !live.iter().any(|l| l == uid),
        Some(pid) => !pid_is_alive(pid),
        None => false,
    }
}

/// State the IOProc, the tap thread and the session all look at.
#[derive(Default)]
struct Shared {
    callbacks: AtomicU64,
    /// Callbacks that held at least one non-zero sample.
    nonzero_callbacks: AtomicU64,
    rebuilds: AtomicU64,
    notice: AtomicU8,
    echo_risk: AtomicBool,
    /// The output device the aggregate is built on, for the event filter.
    output_device: AtomicU32,
    format: Mutex<Option<SourceFormat>>,
    device_name: Mutex<Option<String>>,
}

/// The session's view of the system track while it records. Cheap to clone
/// and to poll; none of it touches the HAL.
#[derive(Clone)]
pub struct SystemAudioMonitor(Arc<Shared>);

impl SystemAudioMonitor {
    /// Anything but `None` is `RecordingStatus::system_audio_silent`. It is a
    /// notice, never an error: the meeting continues with the microphone.
    pub fn notice(&self) -> SystemAudioNotice {
        SystemAudioNotice::from_u8(self.0.notice.load(Ordering::Relaxed))
    }

    /// The output the tap is built on is the built-in speakers. Follows
    /// device changes during the meeting.
    pub fn echo_risk(&self) -> bool {
        self.0.echo_risk.load(Ordering::Relaxed)
    }

    /// Audio callbacks so far. The three counters below are what the hardware
    /// tests assert on; the session only needs the notice and the echo risk.
    #[cfg(test)]
    pub fn callbacks(&self) -> u64 {
        self.0.callbacks.load(Ordering::Relaxed)
    }

    /// Whether anything but digital silence has arrived yet.
    #[cfg(test)]
    pub fn heard_audio(&self) -> bool {
        self.0.nonzero_callbacks.load(Ordering::Relaxed) > 0
    }

    /// Rebuilds after the initial build (device changes, retries).
    #[cfg(test)]
    pub fn rebuilds(&self) -> u64 {
        self.0.rebuilds.load(Ordering::Relaxed)
    }
}

#[cfg(target_os = "macos")]
pub use imp::{check_support, destroy_leaked_aggregates, SystemTapSource};

#[cfg(not(target_os = "macos"))]
pub use stub::{check_support, destroy_leaked_aggregates, SystemTapSource};

#[cfg(not(target_os = "macos"))]
mod stub {
    use super::{Shared, SystemAudioMonitor};
    use crate::meetings::types::{AudioSource, AudioSourceHandler, SourceFormat, TrackKind};
    use std::sync::Arc;

    pub fn check_support() -> Result<(), String> {
        Err("system audio capture needs macOS".to_string())
    }

    pub fn destroy_leaked_aggregates() {}

    pub struct SystemTapSource(Arc<Shared>);

    impl SystemTapSource {
        pub fn new() -> Result<Self, String> {
            check_support().map(|_| Self(Arc::default()))
        }

        pub fn monitor(&self) -> SystemAudioMonitor {
            SystemAudioMonitor(self.0.clone())
        }
    }

    impl AudioSource for SystemTapSource {
        fn kind(&self) -> TrackKind {
            TrackKind::System
        }
        fn device_name(&self) -> Option<String> {
            None
        }
        fn format(&self) -> Option<SourceFormat> {
            None
        }
        fn start(&mut self, _handler: Box<dyn AudioSourceHandler>) -> Result<SourceFormat, String> {
            check_support().map(|_| unreachable!())
        }
        fn stop(&mut self) -> Result<(), String> {
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::UnsafeCell;
    use std::ffi::{c_void, CStr};
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2::AnyThread;
    use objc2_core_audio::{
        kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
        kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
        kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
        kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioSubDeviceUIDKey,
        kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
        AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID,
        AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
        AudioHardwareDestroyAggregateDevice, AudioObjectID, CATapDescription, CATapMuteBehavior,
    };
    use objc2_core_audio_types::{
        kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, AudioBuffer, AudioBufferList,
        AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags,
    };
    use objc2_core_foundation::{
        kCFAllocatorDefault, kCFTypeArrayCallBacks, kCFTypeDictionaryKeyCallBacks,
        kCFTypeDictionaryValueCallBacks, CFArray, CFDictionary, CFMutableDictionary, CFRetained,
        CFString,
    };
    use objc2_foundation::{NSArray, NSNumber, NSString};

    use super::super::clock::{self, Timebase};
    use super::super::device_watch::{
        self, is_builtin_speakers, output_gate, Action, DeviceEvent, Hold, PlannerConfig,
        RebuildPlanner, RebuildReason,
    };
    use super::super::hal::{self, status_str, OSStatus};
    use super::super::mic::join_with_timeout;
    use super::super::permission::{self, Observation, SilenceDetector, SystemAudioNotice};
    use super::super::{parse_os_version, version_supports_taps, HandlerSlot};
    use super::{is_leaked_aggregate, Shared, SystemAudioMonitor, AGGREGATE_UID_PREFIX};
    use crate::meetings::types::{
        AudioFrames, AudioSource, AudioSourceHandler, Discontinuity, PermissionState, SourceFormat,
        TrackKind,
    };

    /// `AudioDeviceCreateIOProcID` blocked for up to 6 s in the spike while a
    /// Bluetooth headset changed mode.
    const START_TIMEOUT: Duration = Duration::from_secs(20);
    const STOP_TIMEOUT: Duration = Duration::from_secs(5);
    /// No callbacks for this long, without any device event, is a stall.
    const STALL_TIMEOUT: Duration = Duration::from_secs(2);
    const TICK: Duration = Duration::from_millis(250);
    const FIRST_CALLBACK_TICK: Duration = Duration::from_millis(5);
    /// How often the silence detector looks at the track.
    const OBSERVE_EVERY: Duration = Duration::from_secs(1);
    /// While the tap delivers only zeros, "is anybody playing?" is asked this
    /// often. It is the expensive question (see `hal`).
    const PLAYING_CHECK_WHILE_SILENT: Duration = Duration::from_secs(5);
    /// An idle output device answers the question for free; the full check
    /// still runs this often, for audio that plays on another device.
    const FULL_CHECK_WHILE_IDLE: Duration = Duration::from_secs(10);
    /// Preflight is asked on every n-th observation.
    const PREFLIGHT_EVERY: u32 = 5;
    /// Most samples a callback can interleave without allocating.
    const SCRATCH_SAMPLES: usize = 1 << 15;

    fn log(level: &str, message: &str) {
        #[cfg(not(test))]
        let _ = crate::storage::append_log(level, message);
        #[cfg(test)]
        let _ = (level, message);
    }

    type CreateTapFn =
        unsafe extern "C" fn(*const CATapDescription, *mut AudioObjectID) -> OSStatus;
    type DestroyTapFn = unsafe extern "C" fn(AudioObjectID) -> OSStatus;

    #[derive(Clone, Copy)]
    struct TapApi {
        create: CreateTapFn,
        destroy: DestroyTapFn,
    }

    fn os_version() -> Result<(u32, u32, u32), String> {
        let mut buffer = [0u8; 32];
        let mut size = buffer.len();
        // SAFETY: `buffer` holds `size` bytes; the name is NUL-terminated.
        let status = unsafe {
            libc::sysctlbyname(
                c"kern.osproductversion".as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return Err("the macOS version cannot be read".to_string());
        }
        let text = String::from_utf8_lossy(&buffer[..size.min(buffer.len())]).to_string();
        parse_os_version(&text).ok_or_else(|| format!("unexpected macOS version '{}'", text.trim()))
    }

    /// The runtime gate: macOS 14.4+, the `CATapDescription` class, both tap
    /// symbols. The symbols are looked up by name so that nothing in the
    /// binary imports them.
    fn resolve_api() -> Result<TapApi, String> {
        let version = os_version()?;
        if !version_supports_taps(version) {
            return Err(format!(
                "macOS {}.{}.{} is older than 14.4",
                version.0, version.1, version.2
            ));
        }
        if AnyClass::get(c"CATapDescription").is_none() {
            return Err("the CATapDescription class is missing".to_string());
        }
        // SAFETY: NUL-terminated names; the signatures are those of
        // `AudioHardwareCreateProcessTap` and `AudioHardwareDestroyProcessTap`.
        unsafe {
            let create = libc::dlsym(
                libc::RTLD_DEFAULT,
                c"AudioHardwareCreateProcessTap".as_ptr(),
            );
            let destroy = libc::dlsym(
                libc::RTLD_DEFAULT,
                c"AudioHardwareDestroyProcessTap".as_ptr(),
            );
            if create.is_null() || destroy.is_null() {
                return Err("the process tap functions are missing".to_string());
            }
            Ok(TapApi {
                create: std::mem::transmute::<*mut c_void, CreateTapFn>(create),
                destroy: std::mem::transmute::<*mut c_void, DestroyTapFn>(destroy),
            })
        }
    }

    pub fn check_support() -> Result<(), String> {
        resolve_api().map(|_| ())
    }

    enum Msg {
        Device(DeviceEvent),
        Stop,
    }

    struct Worker {
        tx: Sender<Msg>,
        thread: JoinHandle<()>,
        slot: Arc<HandlerSlot>,
    }

    pub struct SystemTapSource {
        api: TapApi,
        shared: Arc<Shared>,
        worker: Option<Worker>,
    }

    impl SystemTapSource {
        /// `Err` when this Mac cannot do process taps. Nothing is created
        /// until `start`.
        pub fn new() -> Result<Self, String> {
            super::super::support()?;
            Ok(Self {
                api: resolve_api()?,
                shared: Arc::default(),
                worker: None,
            })
        }

        /// Take it before boxing the source as a `dyn AudioSource`.
        pub fn monitor(&self) -> SystemAudioMonitor {
            SystemAudioMonitor(self.shared.clone())
        }
    }

    impl AudioSource for SystemTapSource {
        fn kind(&self) -> TrackKind {
            TrackKind::System
        }

        fn device_name(&self) -> Option<String> {
            self.shared
                .device_name
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        fn format(&self) -> Option<SourceFormat> {
            *self.shared.format.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Start the microphone first and wait for it (spike finding F2).
        ///
        /// Returns once the tap delivers, or after the first-callback timeout
        /// when it does not (permission still undetermined): the source then
        /// keeps retrying in the background and says so through
        /// `SystemAudioMonitor::notice()`. `Err` means the tap or the
        /// aggregate could not be created at all. Blocks for up to a few
        /// seconds; call it from a thread that may block.
        fn start(&mut self, handler: Box<dyn AudioSourceHandler>) -> Result<SourceFormat, String> {
            if self.worker.is_some() {
                return Err("System audio is already recording".to_string());
            }
            let slot = Arc::new(HandlerSlot::new(handler));
            let (tx, rx) = mpsc::channel();
            let (ready_tx, ready_rx) = mpsc::channel();
            let thread = {
                let (api, slot, shared, tx) =
                    (self.api, slot.clone(), self.shared.clone(), tx.clone());
                std::thread::Builder::new()
                    .name("meeting-tap".to_string())
                    .spawn(move || run(api, slot, shared, rx, tx, ready_tx))
                    .map_err(|e| format!("Failed to start the system audio thread: {e}"))?
            };
            self.worker = Some(Worker { tx, thread, slot });
            let result = match ready_rx.recv_timeout(START_TIMEOUT) {
                Ok(result) => result,
                Err(_) => Err("System audio did not start in time".to_string()),
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
            // stuck in a HAL call for a while longer. It still destroys the
            // tap and the aggregate when it gets out.
            worker.slot.close();
            join_with_timeout(worker.thread, STOP_TIMEOUT, "system audio");
            *self.shared.format.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.shared
                .notice
                .store(SystemAudioNotice::None.as_u8(), Ordering::Relaxed);
            Ok(())
        }
    }

    impl Drop for SystemTapSource {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    /// What the IOProc works with. One per build; freed after the IOProc is
    /// gone.
    struct TapCtx {
        slot: Arc<HandlerSlot>,
        shared: Arc<Shared>,
        timebase: Timebase,
        /// The aggregate's nominal rate (F1), not the tap's reported one.
        sample_rate: u32,
        /// Trailing input buffers that belong to the tap: one when the tap is
        /// interleaved, one per channel when it is not.
        tap_buffers: usize,
        /// Only the IOProc touches it: non-interleaved buffers are
        /// interleaved here. Allocated up front.
        scratch: UnsafeCell<Vec<f32>>,
    }

    /// Real-time thread. Finds the tap's buffer, stamps it with the device's
    /// host time and hands it over. No locks, no allocation, no logging.
    unsafe extern "C-unwind" fn io_proc(
        _device: AudioObjectID,
        _now: NonNull<AudioTimeStamp>,
        input: NonNull<AudioBufferList>,
        input_time: NonNull<AudioTimeStamp>,
        _output: NonNull<AudioBufferList>,
        _output_time: NonNull<AudioTimeStamp>,
        client: *mut c_void,
    ) -> OSStatus {
        let ctx = &*(client as *const TapCtx);
        let list = input.as_ref();
        let n = list.mNumberBuffers as usize;
        if n == 0 {
            return 0;
        }
        let buffers: &[AudioBuffer] = std::slice::from_raw_parts(list.mBuffers.as_ptr(), n);
        // The sub-device's own input streams (a headset's microphone) come
        // first, the tap's streams last.
        let tap = &buffers[n - ctx.tap_buffers.clamp(1, n)..];

        let (samples, channels): (&[f32], u16) = if tap.len() == 1 {
            let buffer = &tap[0];
            if buffer.mData.is_null() {
                return 0;
            }
            let samples = std::slice::from_raw_parts(
                buffer.mData as *const f32,
                buffer.mDataByteSize as usize / std::mem::size_of::<f32>(),
            );
            (samples, buffer.mNumberChannels.max(1) as u16)
        } else {
            let scratch = &mut *ctx.scratch.get();
            let frames = tap
                .iter()
                .map(|b| {
                    if b.mData.is_null() {
                        0
                    } else {
                        b.mDataByteSize as usize / 4
                    }
                })
                .min()
                .unwrap_or(0)
                .min(scratch.capacity() / tap.len());
            scratch.clear();
            for frame in 0..frames {
                for buffer in tap {
                    scratch.push(*(buffer.mData as *const f32).add(frame));
                }
            }
            (scratch.as_slice(), tap.len() as u16)
        };
        if samples.is_empty() {
            return 0;
        }

        let frames = samples.len() as u64 / channels as u64;
        let time = input_time.as_ref();
        let host_time_ns = if time.mFlags.contains(AudioTimeStampFlags::HostTimeValid)
            && time.mHostTime != 0
        {
            clock::ticks_to_ns(ctx.timebase, time.mHostTime)
        } else {
            clock::now_ns(ctx.timebase).saturating_sub(clock::frames_to_ns(frames, ctx.sample_rate))
        };
        ctx.slot.frames(AudioFrames {
            samples,
            format: SourceFormat {
                sample_rate: ctx.sample_rate,
                channels,
            },
            host_time_ns,
        });
        if samples.iter().any(|s| *s != 0.0) {
            ctx.shared.nonzero_callbacks.fetch_add(1, Ordering::Relaxed);
        }
        ctx.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        0
    }

    struct Tap {
        id: AudioObjectID,
        uid: Retained<NSString>,
    }

    fn create_tap(api: TapApi) -> Result<Tap, String> {
        let exclude = NSArray::<NSNumber>::new();
        // SAFETY: plain Objective-C messages on a freshly allocated object;
        // the class is known to exist (`resolve_api`).
        let description = unsafe {
            let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &exclude,
            );
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
            description.setPrivate(true);
            description.setName(&NSString::from_str("FlowingThoughts Meeting Tap"));
            description
        };
        let mut id: AudioObjectID = 0;
        // SAFETY: a valid description and out-pointer.
        let status = unsafe { (api.create)(Retained::as_ptr(&description), &mut id) };
        if status != 0 {
            return Err(format!(
                "Failed to create the system audio tap: {}",
                status_str(status)
            ));
        }
        // SAFETY: plain getters.
        let uid = unsafe { description.UUID().UUIDString() };
        Ok(Tap { id, uid })
    }

    fn destroy_tap(api: TapApi, tap: Tap) {
        // SAFETY: `tap.id` came from `create_tap` and is destroyed once.
        let status = unsafe { (api.destroy)(tap.id) };
        if status != 0 {
            log(
                "WARN",
                &format!(
                    "Meetings: destroying the system audio tap failed: {}",
                    status_str(status)
                ),
            );
        }
    }

    fn cf_key(key: &'static CStr) -> CFRetained<CFString> {
        CFString::from_str(key.to_str().unwrap_or_default())
    }

    unsafe fn dict_new() -> Result<CFRetained<CFMutableDictionary>, String> {
        CFMutableDictionary::new(
            kCFAllocatorDefault,
            0,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
        .ok_or_else(|| "CFDictionaryCreateMutable failed".to_string())
    }

    unsafe fn dict_set<T>(dict: &CFMutableDictionary, key: &'static CStr, value: &T) {
        CFMutableDictionary::set_value(
            Some(dict),
            &*cf_key(key) as *const CFString as *const c_void,
            value as *const T as *const c_void,
        );
    }

    unsafe fn array_of_one(dict: &CFMutableDictionary) -> Result<CFRetained<CFArray>, String> {
        let items = [dict as *const CFMutableDictionary as *const c_void];
        CFArray::new(
            kCFAllocatorDefault,
            items.as_ptr() as *mut *const c_void,
            1,
            &kCFTypeArrayCallBacks,
        )
        .ok_or_else(|| "CFArrayCreate failed".to_string())
    }

    /// UIDs of the aggregates this process has built and not yet destroyed.
    fn live_aggregates() -> std::sync::MutexGuard<'static, Vec<String>> {
        static LIVE: Mutex<Vec<String>> = Mutex::new(Vec::new());
        LIVE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Unique in this process, across sources.
    fn next_aggregate_uid() -> String {
        static INSTANCE: AtomicU32 = AtomicU32::new(0);
        let instance = INSTANCE.fetch_add(1, Ordering::Relaxed);
        format!("{AGGREGATE_UID_PREFIX}{}.{instance}", std::process::id())
    }

    fn aggregate_description(
        tap_uid: &NSString,
        output_uid: &CFString,
        aggregate_uid: &str,
    ) -> Result<CFRetained<CFDictionary>, String> {
        // SAFETY: every key is a CFString and every value a CF/NS object that
        // outlives the `set_value` call, which retains it.
        unsafe {
            let yes = NSNumber::new_bool(true);
            let no = NSNumber::new_bool(false);

            let sub_tap = dict_new()?;
            dict_set(&sub_tap, kAudioSubTapUIDKey, tap_uid);
            dict_set(&sub_tap, kAudioSubTapDriftCompensationKey, &*yes);
            let taps = array_of_one(&sub_tap)?;

            let sub_device = dict_new()?;
            dict_set(&sub_device, kAudioSubDeviceUIDKey, output_uid);
            let sub_devices = array_of_one(&sub_device)?;

            let name = CFString::from_str("FlowingThoughts Meeting Audio");
            let uid = CFString::from_str(aggregate_uid);

            let dict = dict_new()?;
            dict_set(&dict, kAudioAggregateDeviceNameKey, &*name);
            dict_set(&dict, kAudioAggregateDeviceUIDKey, &*uid);
            dict_set(&dict, kAudioAggregateDeviceMainSubDeviceKey, output_uid);
            dict_set(&dict, kAudioAggregateDeviceIsPrivateKey, &*yes);
            dict_set(&dict, kAudioAggregateDeviceIsStackedKey, &*no);
            dict_set(&dict, kAudioAggregateDeviceTapAutoStartKey, &*yes);
            dict_set(&dict, kAudioAggregateDeviceSubDeviceListKey, &*sub_devices);
            dict_set(&dict, kAudioAggregateDeviceTapListKey, &*taps);
            Ok(CFRetained::cast_unchecked::<CFDictionary>(dict))
        }
    }

    /// Destroys aggregates an earlier start of this process, or a process
    /// that is gone, left behind. (coreaudiod reclaims a dead process's
    /// private aggregates by itself; this covers what it does not.)
    pub fn destroy_leaked_aggregates() {
        let own_pid = std::process::id();
        // SAFETY: signal 0 only checks that the process exists.
        let alive = |pid: u32| unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
        for device in hal::all_devices() {
            let Ok(uid) = hal::device_uid(device) else {
                continue;
            };
            let live = live_aggregates().clone();
            if is_leaked_aggregate(&uid.to_string(), own_pid, &live, alive) {
                // SAFETY: an aggregate device id the HAL just listed.
                let status = unsafe { AudioHardwareDestroyAggregateDevice(device) };
                log(
                    "INFO",
                    &format!(
                        "Meetings: removed a leftover audio aggregate ({})",
                        status_str(status)
                    ),
                );
            }
        }
    }

    struct Built {
        aggregate: AudioObjectID,
        aggregate_uid: String,
        proc_id: AudioDeviceIOProcID,
        ctx: *mut TapCtx,
        output_device: AudioObjectID,
        output_rate: Option<f64>,
        format: SourceFormat,
    }

    fn build(tap: &Tap, slot: &Arc<HandlerSlot>, shared: &Arc<Shared>) -> Result<Built, String> {
        let output_device = hal::default_output_device()?;
        let output_uid = hal::device_uid(output_device)
            .map_err(|e| format!("The output device has no UID: {}", status_str(e)))?;
        let output_name = hal::device_name(output_device);
        let output_rate = hal::nominal_sample_rate(output_device);

        // SAFETY: the property is an AudioStreamBasicDescription.
        let tap_format: AudioStreamBasicDescription =
            unsafe { hal::get_prop(tap.id, kAudioTapPropertyFormat) }
                .map_err(|e| format!("The system audio tap has no format: {}", status_str(e)))?;
        if tap_format.mFormatFlags & kAudioFormatFlagIsFloat == 0
            || tap_format.mBitsPerChannel != 32
        {
            return Err(format!(
                "The system audio tap is not float32 (flags {:#x}, {} bits)",
                tap_format.mFormatFlags, tap_format.mBitsPerChannel
            ));
        }
        let tap_channels = tap_format.mChannelsPerFrame.clamp(1, 8) as usize;
        let non_interleaved = tap_format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
        let tap_buffers = if non_interleaved { tap_channels } else { 1 };

        let aggregate_uid = next_aggregate_uid();
        let description = aggregate_description(&tap.uid, &output_uid, &aggregate_uid)?;
        let mut aggregate: AudioObjectID = 0;
        // Registered before it exists, so that another source starting right
        // now does not take it for a leftover.
        live_aggregates().push(aggregate_uid.clone());
        let forget = |uid: &str| live_aggregates().retain(|live| live != uid);
        // SAFETY: a valid dictionary and out-pointer.
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(&description, NonNull::from(&mut aggregate))
        };
        if status != 0 {
            forget(&aggregate_uid);
            return Err(format!(
                "Failed to create the audio aggregate: {}",
                status_str(status)
            ));
        }
        let destroy_aggregate = || {
            // SAFETY: created above, destroyed once.
            unsafe { AudioHardwareDestroyAggregateDevice(aggregate) };
            forget(&aggregate_uid);
        };

        // F1: the IOProc delivers at the aggregate's rate, which follows the
        // main sub-device (24 kHz for AirPods in call mode), whatever the tap
        // says about itself.
        let rate = hal::nominal_sample_rate(aggregate)
            .or(output_rate)
            .unwrap_or(tap_format.mSampleRate);
        if !(8_000.0..=768_000.0).contains(&rate) {
            destroy_aggregate();
            return Err(format!(
                "The audio aggregate reports a sample rate of {rate}"
            ));
        }
        let format = SourceFormat {
            sample_rate: rate.round() as u32,
            channels: tap_channels as u16,
        };

        let ctx = Box::into_raw(Box::new(TapCtx {
            slot: slot.clone(),
            shared: shared.clone(),
            timebase: clock::timebase(),
            sample_rate: format.sample_rate,
            tap_buffers,
            scratch: UnsafeCell::new(Vec::with_capacity(SCRATCH_SAMPLES)),
        }));
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: `ctx` stays alive until after the IOProc is destroyed.
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                aggregate,
                Some(io_proc),
                ctx as *mut c_void,
                NonNull::from(&mut proc_id),
            )
        };
        if status != 0 {
            destroy_aggregate();
            // SAFETY: the IOProc was never registered.
            drop(unsafe { Box::from_raw(ctx) });
            return Err(format!(
                "Failed to attach to the audio aggregate: {}",
                status_str(status)
            ));
        }
        // SAFETY: a valid device and IOProc id.
        let status = unsafe { AudioDeviceStart(aggregate, proc_id) };
        if status != 0 {
            // SAFETY: as above; the IOProc never ran.
            unsafe {
                AudioDeviceDestroyIOProcID(aggregate, proc_id);
                destroy_aggregate();
                drop(Box::from_raw(ctx));
            }
            return Err(format!(
                "Failed to start the audio aggregate: {}",
                status_str(status)
            ));
        }

        shared.output_device.store(output_device, Ordering::Relaxed);
        shared.echo_risk.store(
            is_builtin_speakers(
                hal::transport_type(output_device),
                hal::output_data_source(output_device),
                output_name.as_deref(),
            ),
            Ordering::Relaxed,
        );
        *shared.format.lock().unwrap_or_else(|e| e.into_inner()) = Some(format);
        *shared.device_name.lock().unwrap_or_else(|e| e.into_inner()) = output_name;
        device_watch::watch_sample_rate(output_device);
        Ok(Built {
            aggregate,
            aggregate_uid,
            proc_id,
            ctx,
            output_device,
            output_rate,
            format,
        })
    }

    /// Stops and destroys the IOProc and the aggregate. The context goes to
    /// `retired` instead of being freed: a callback may still be in flight.
    fn teardown(built: Built, retired: &mut Vec<*mut TapCtx>) {
        device_watch::unwatch_sample_rate(built.output_device);
        // SAFETY: ids from `build`, each destroyed once.
        let statuses = unsafe {
            [
                AudioDeviceStop(built.aggregate, built.proc_id),
                AudioDeviceDestroyIOProcID(built.aggregate, built.proc_id),
                AudioHardwareDestroyAggregateDevice(built.aggregate),
            ]
        };
        if statuses.iter().any(|s| *s != 0) {
            // Normal when the device under the aggregate has vanished.
            let text: Vec<String> = statuses.iter().map(|s| status_str(*s)).collect();
            log(
                "INFO",
                &format!(
                    "Meetings: system audio teardown statuses: {}",
                    text.join(", ")
                ),
            );
        }
        live_aggregates().retain(|live| *live != built.aggregate_uid);
        retired.push(built.ctx);
    }

    /// Frees the contexts of IOProcs that were destroyed at least one grace
    /// period ago.
    fn free_retired(retired: &mut Vec<*mut TapCtx>) {
        for ctx in retired.drain(..) {
            // SAFETY: from `Box::into_raw` in `build`; its IOProc was
            // destroyed and the grace period has passed.
            drop(unsafe { Box::from_raw(ctx) });
        }
    }

    /// Nothing runs I/O on the output device, not even our aggregate: nothing
    /// is playing on it, and the IOProc is not being called.
    fn output_is_idle(built: &Built) -> bool {
        hal::device_is_running_somewhere(built.output_device) == Some(false)
    }

    /// Whether a device event needs a rebuild at all: the default output often
    /// flaps away and back.
    fn still_valid(built: &Built, delivering: bool) -> bool {
        delivering
            && hal::default_output_device().ok() == Some(built.output_device)
            && hal::nominal_sample_rate(built.output_device) == built.output_rate
    }

    /// "Is another process playing audio right now?", rationed. It feeds the
    /// `SilenceDetector`; nothing else waits for it.
    ///
    /// Found on hardware (built-in speakers, macOS 26): while no process plays
    /// anything the output device does not run and the IOProc does not fire at
    /// all; it starts by itself the moment something plays, and then keeps
    /// running for as long as the aggregate exists. So "no callbacks" and
    /// "only zeros" are faults only while another process is playing.
    struct PlayingProbe {
        playing: Option<bool>,
        checked_at: Instant,
        full_checked_at: Instant,
    }

    impl PlayingProbe {
        fn new(output_device: Option<AudioObjectID>) -> Self {
            let now = Instant::now();
            let mut probe = Self {
                playing: None,
                checked_at: now,
                full_checked_at: now,
            };
            probe.check(now, output_device);
            probe
        }

        fn check(&mut self, now: Instant, output_device: Option<AudioObjectID>) {
            // An output device nobody runs means nobody plays on it: one cheap
            // read. The process list is only walked when that is not the
            // answer, and now and then for audio that plays on another device
            // (every time, once that was the case).
            let device_idle =
                output_device.and_then(hal::device_is_running_somewhere) == Some(false);
            let full_is_due = self.playing.is_none()
                || self.playing == Some(true)
                || now.duration_since(self.full_checked_at) >= FULL_CHECK_WHILE_IDLE;
            if device_idle && !full_is_due {
                self.playing = Some(false);
            } else {
                self.playing = hal::other_process_running_output();
                self.full_checked_at = now;
            }
            self.checked_at = now;
        }

        fn check_if_older(
            &mut self,
            now: Instant,
            max_age: Duration,
            output_device: Option<AudioObjectID>,
        ) {
            if now.duration_since(self.checked_at) >= max_age {
                self.check(now, output_device);
            }
        }
    }

    fn run(
        api: TapApi,
        slot: Arc<HandlerSlot>,
        shared: Arc<Shared>,
        msgs: Receiver<Msg>,
        msg_tx: Sender<Msg>,
        ready: Sender<Result<SourceFormat, String>>,
    ) {
        let mut ready = Some(ready);
        let mut retired: Vec<*mut TapCtx> = Vec::new();
        let started = Instant::now();
        destroy_leaked_aggregates();

        // Listen before building, so no change between "read the default
        // output" and "listening" is missed. Output events mark the gate on
        // the HAL's thread, because this one may be stuck inside a HAL call.
        let subscription = {
            let shared = shared.clone();
            device_watch::subscribe(move |event| {
                let relevant = match event {
                    DeviceEvent::DefaultOutputChanged => true,
                    DeviceEvent::SampleRateChanged { device } => {
                        device == shared.output_device.load(Ordering::Relaxed)
                    }
                    DeviceEvent::DefaultInputChanged => false,
                };
                if relevant {
                    output_gate().note_event();
                    let _ = msg_tx.send(Msg::Device(event));
                }
            })
        };
        if let Err(e) = &subscription {
            log("WARN", &format!("Meetings: system audio: {e}"));
        }

        let mut tap = match create_tap(api) {
            Ok(tap) => Some(tap),
            Err(e) => {
                log("ERROR", &format!("Meetings: {e}"));
                let _ = ready.take().map(|r| r.send(Err(e)));
                return;
            }
        };
        let mut built = match build(tap.as_ref().expect("just created"), &slot, &shared) {
            Ok(built) => Some(built),
            Err(e) => {
                log("ERROR", &format!("Meetings: system audio: {e}"));
                if let Some(tap) = tap.take() {
                    destroy_tap(api, tap);
                }
                let _ = ready.take().map(|r| r.send(Err(e)));
                return;
            }
        };
        log(
            "INFO",
            &format!(
                "Meetings: system audio tap built in {} ms ({} Hz, {} ch)",
                started.elapsed().as_millis(),
                built.as_ref().map_or(0, |b| b.format.sample_rate),
                built.as_ref().map_or(0, |b| b.format.channels),
            ),
        );

        let mut planner = RebuildPlanner::new(PlannerConfig::default(), Instant::now());
        // The output device is not running, so nothing plays on it and the
        // IOProc will not fire (see `PlayingProbe`): there is no first
        // callback to wait for.
        if built.as_ref().is_some_and(output_is_idle) {
            if let (Some(ready), Some(built)) = (ready.take(), built.as_ref()) {
                let _ = ready.send(Ok(built.format));
            }
        }
        let mut probe = PlayingProbe::new(built.as_ref().map(|b| b.output_device));
        let mut seen_callbacks = shared.callbacks.load(Ordering::Relaxed);
        // When the current build last delivered. `None`: not yet.
        let mut last_progress: Option<Instant> = None;
        let mut event_at: Option<Instant> = None;
        // Output events up to this generation are dealt with once the track
        // delivers again, or has given up.
        let mut gate_generation = output_gate().generation();

        let mut detector = SilenceDetector::default();
        let mut observed_at = Instant::now();
        let mut observed_callbacks = seen_callbacks;
        let mut observed_nonzero = shared.nonzero_callbacks.load(Ordering::Relaxed);
        let mut observations = 0u32;
        let mut permission_state = PermissionState::Unknown;

        loop {
            let now = Instant::now();
            let tick = if planner.is_awaiting_first_callback() {
                FIRST_CALLBACK_TICK
            } else {
                TICK
            };
            // Never zero: a rebuild that is held back must not spin.
            let timeout = planner
                .next_deadline(now)
                .map_or(tick, |d| d.clamp(FIRST_CALLBACK_TICK, tick));
            match msgs.recv_timeout(timeout) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Msg::Device(event)) => {
                    let reason = match event {
                        DeviceEvent::SampleRateChanged { .. } => RebuildReason::SampleRateChanged,
                        _ => RebuildReason::DefaultDeviceChanged,
                    };
                    event_at.get_or_insert(Instant::now());
                    planner.on_event(Instant::now(), reason);
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
                    output_gate().settle(gate_generation);
                    if let (Some(ready), Some(built)) = (ready.take(), built.as_ref()) {
                        let _ = ready.send(Ok(built.format));
                    }
                    if let Some(event_at) = event_at.take() {
                        log(
                            "INFO",
                            &format!(
                                "Meetings: system audio recovered {} ms after the device event",
                                event_at.elapsed().as_millis()
                            ),
                        );
                    }
                }
            } else {
                // Callbacks stopped while the output device runs: a stall.
                // While it does not run there is nothing to deliver.
                let stalled = planner.is_running()
                    && last_progress.is_some_and(|at| now.duration_since(at) > STALL_TIMEOUT);
                if stalled && !built.as_ref().is_some_and(output_is_idle) {
                    log("WARN", "Meetings: system audio stalled; rebuilding");
                    slot.discontinuity(Discontinuity::Stalled, clock::host_now_ns());
                    event_at.get_or_insert(now);
                    planner.on_event(now, RebuildReason::Stalled);
                }
            }
            // An idle tap is neither reported nor retried. One cheap read,
            // and only while the answer decides something.
            let idle =
                planner.is_waiting_for_callbacks() && built.as_ref().is_some_and(output_is_idle);

            match planner.poll(
                now,
                Hold {
                    settle: false,
                    retry: idle,
                },
            ) {
                Action::Wait => {}
                Action::Degraded => {
                    output_gate().settle(gate_generation);
                    let initial = match (ready.take(), built.as_ref()) {
                        (Some(ready), Some(built)) => ready.send(Ok(built.format)).is_ok(),
                        _ => false,
                    };
                    if !idle {
                        // F5: audio is playing and the tap delivers nothing
                        // (permission undetermined, or a wedged device). The
                        // meeting goes on with the microphone; the planner
                        // has scheduled a retry.
                        log("WARN", "Meetings: system audio is not delivering; continuing without it and retrying");
                        if !initial {
                            slot.discontinuity(Discontinuity::Stalled, clock::host_now_ns());
                        }
                    }
                }
                Action::Rebuild(reason) => {
                    gate_generation = output_gate().generation();
                    let delivering = last_progress
                        .is_some_and(|at| now.duration_since(at) < Duration::from_millis(600));
                    let device_event = matches!(
                        reason,
                        RebuildReason::DefaultDeviceChanged | RebuildReason::SampleRateChanged
                    );
                    if device_event && built.as_ref().is_some_and(|b| still_valid(b, delivering)) {
                        // Flapped back to the device the aggregate is on.
                        planner.on_built(now, true);
                        planner.on_first_callback();
                        output_gate().settle(gate_generation);
                        event_at = None;
                        continue;
                    }

                    let t = Instant::now();
                    free_retired(&mut retired);
                    if let Some(old) = built.take() {
                        teardown(old, &mut retired);
                    }
                    // A tap that never delivered may predate the user's
                    // answer to the permission prompt: start over with a new
                    // one. A device change keeps the tap; it is global.
                    if reason == RebuildReason::Retry {
                        if let Some(old) = tap.take() {
                            destroy_tap(api, old);
                        }
                    }
                    if tap.is_none() {
                        tap = create_tap(api)
                            .map_err(|e| log("WARN", &format!("Meetings: {e}")))
                            .ok();
                    }
                    let result = match tap.as_ref() {
                        Some(tap) => build(tap, &slot, &shared),
                        None => Err("there is no system audio tap".to_string()),
                    };
                    seen_callbacks = shared.callbacks.load(Ordering::Relaxed);
                    last_progress = None;
                    planner.on_built(Instant::now(), result.is_ok());
                    shared.rebuilds.fetch_add(1, Ordering::Relaxed);
                    match result {
                        Ok(new) => {
                            log(
                                "INFO",
                                &format!(
                                    "Meetings: system audio rebuilt in {} ms ({}; {} Hz, {} ch)",
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
                            built = Some(new);
                            // With nothing playing there is no first callback
                            // to wait for: the output side is as ready as it
                            // gets, and the microphone need not wait.
                            if built.as_ref().is_some_and(output_is_idle) {
                                output_gate().settle(gate_generation);
                                event_at = None;
                            }
                        }
                        Err(e) => {
                            log(
                                "WARN",
                                &format!(
                                    "Meetings: system audio rebuild failed ({}): {e}",
                                    reason.as_str()
                                ),
                            );
                            output_gate().settle(gate_generation);
                        }
                    }
                }
            }

            if now.duration_since(observed_at) >= OBSERVE_EVERY {
                observed_at = now;
                if observations % PREFLIGHT_EVERY == 0 {
                    permission_state =
                        permission::map_preflight(&permission::preflight_audio_capture()).state;
                }
                observations = observations.wrapping_add(1);
                let nonzero = shared.nonzero_callbacks.load(Ordering::Relaxed);
                let delivering = callbacks != observed_callbacks;
                let heard_audio = nonzero != observed_nonzero;
                // Audible audio needs no second opinion. Zeros and silence on
                // the line do, but not every second.
                if !heard_audio {
                    let max_age = if delivering {
                        PLAYING_CHECK_WHILE_SILENT
                    } else {
                        OBSERVE_EVERY
                    };
                    probe.check_if_older(now, max_age, built.as_ref().map(|b| b.output_device));
                }
                let observation = Observation {
                    delivering,
                    heard_audio,
                    output_running: probe.playing,
                    permission: permission_state,
                };
                observed_callbacks = callbacks;
                observed_nonzero = nonzero;
                let before = detector.notice();
                let notice = detector.observe(now, observation);
                if notice != before {
                    log(
                        "INFO",
                        &format!("Meetings: system audio notice: {before:?} -> {notice:?}"),
                    );
                }
                shared.notice.store(notice.as_u8(), Ordering::Relaxed);
            }
        }

        if let Some(built) = built.take() {
            teardown(built, &mut retired);
        }
        if let Some(tap) = tap.take() {
            destroy_tap(api, tap);
        }
        drop(subscription);
        output_gate().reset();
        // Grace period before freeing what an IOProc pointed at.
        std::thread::sleep(Duration::from_millis(50));
        free_retired(&mut retired);
    }

    #[cfg(test)]
    mod tests {
        use super::super::super::test_support::{hardware_lock, CapturingHandler};
        use super::*;

        fn our_aggregates() -> usize {
            hal::all_devices()
                .into_iter()
                .filter_map(|d| hal::device_uid(d).ok())
                .filter(|uid| uid.to_string().starts_with(AGGREGATE_UID_PREFIX))
                .count()
        }

        #[test]
        fn this_mac_passes_or_fails_the_gate_without_touching_the_hal() {
            match check_support() {
                Ok(()) => assert!(version_supports_taps(os_version().unwrap())),
                Err(reason) => assert!(!reason.is_empty()),
            }
        }

        #[test]
        #[ignore = "needs audio hardware, the System Audio Recording permission, and plays a sound"]
        fn five_seconds_from_the_tap_while_afplay_runs_are_not_all_zeros() {
            let _hardware = hardware_lock();
            let handler = CapturingHandler::default();
            let mut tap = SystemTapSource::new().expect("process taps are supported");
            let monitor = tap.monitor();
            let before = clock::host_now_ns();
            let format = tap
                .start(Box::new(handler.clone()))
                .expect("the tap starts");
            let mut player = std::process::Command::new("afplay")
                .arg("/System/Library/Sounds/Submarine.aiff")
                .spawn()
                .expect("afplay");
            std::thread::sleep(Duration::from_secs(5));
            let _ = player.kill();
            let _ = player.wait();
            let notice = monitor.notice();
            tap.stop().unwrap();
            let after = clock::host_now_ns();

            let captured = handler.0.lock().unwrap();
            let seconds = captured.frames as f64 / format.sample_rate as f64;
            println!(
                "tap: {:?} {format:?}: {} buffers, {seconds:.2} s, {} non-zero samples, notice {notice:?}, preflight {:?}",
                tap.device_name(),
                captured.buffers,
                captured.nonzero_samples,
                permission::preflight_audio_capture(),
            );
            assert!(
                captured.buffers > 0,
                "the IOProc never fired (permission undetermined?)"
            );
            // On the built-in speakers the IOProc only starts with the sound.
            // That must not have counted as a fault.
            assert_eq!((monitor.rebuilds(), notice), (0, SystemAudioNotice::None));
            assert!(
                (4.0..=5.5).contains(&seconds),
                "{seconds} s of audio in 5 s"
            );
            assert!(
                captured.nonzero_samples > 0,
                "only zeros: System Audio Recording is probably denied"
            );
            assert_eq!(
                captured.formats,
                vec![format],
                "frames carry the format start() returned"
            );
            let first = captured.first_host_ns.unwrap();
            assert!(first + 1_000_000_000 > before && captured.last_host_ns < after);
            assert_eq!(our_aggregates(), 0, "stop destroyed the aggregate");
        }

        #[test]
        #[ignore = "needs audio hardware, the System Audio Recording permission, and plays a sound"]
        fn a_device_event_rebuilds_an_idle_tap_but_not_one_that_still_delivers() {
            let _hardware = hardware_lock();
            let handler = CapturingHandler::default();
            let mut tap = SystemTapSource::new().expect("process taps are supported");
            let monitor = tap.monitor();
            tap.start(Box::new(handler.clone()))
                .expect("the tap starts");
            let inject = |tap: &SystemTapSource| {
                output_gate().note_event();
                let _ = tap
                    .worker
                    .as_ref()
                    .unwrap()
                    .tx
                    .send(Msg::Device(DeviceEvent::DefaultOutputChanged));
            };

            // Nothing delivers yet, so nothing proves the aggregate is still
            // good: the event rebuilds it, within the quiet period plus a
            // build, and the microphone is not kept waiting.
            let t = Instant::now();
            inject(&tap);
            while monitor.rebuilds() == 0 && t.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
            println!(
                "tap: rebuilt {} ms after the event",
                t.elapsed().as_millis()
            );
            assert_eq!(monitor.rebuilds(), 1);
            assert!(t.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!output_gate().is_busy());

            // The rebuilt tap works.
            let mut player = std::process::Command::new("afplay")
                .arg("/System/Library/Sounds/Submarine.aiff")
                .spawn()
                .expect("afplay");
            std::thread::sleep(Duration::from_millis(800));
            assert!(monitor.heard_audio(), "the rebuilt tap hears the sound");
            // Still the same device and delivering: a flap back is ignored.
            inject(&tap);
            std::thread::sleep(Duration::from_millis(1_000));
            assert_eq!(
                monitor.rebuilds(),
                1,
                "a tap that still delivers is left alone"
            );
            assert!(!output_gate().is_busy());
            let _ = player.kill();
            let _ = player.wait();
            tap.stop().unwrap();

            let captured = handler.0.lock().unwrap();
            assert!(
                captured
                    .discontinuities
                    .iter()
                    .any(|d| matches!(d, Discontinuity::FormatChanged { .. })),
                "{:?}",
                captured.discontinuities
            );
            assert_eq!(our_aggregates(), 0);
        }

        #[test]
        #[ignore = "needs audio hardware"]
        fn start_and_stop_leave_no_aggregate_behind() {
            let _hardware = hardware_lock();
            for _ in 0..2 {
                let mut tap = SystemTapSource::new().expect("process taps are supported");
                let monitor = tap.monitor();
                let t = Instant::now();
                let started = tap.start(Box::new(CapturingHandler::default()));
                let start_ms = t.elapsed().as_millis();
                let t = Instant::now();
                tap.stop().unwrap();
                tap.stop().unwrap();
                println!(
                    "tap: start {started:?} in {start_ms} ms ({} callbacks), stop in {} ms",
                    monitor.callbacks(),
                    t.elapsed().as_millis()
                );
                assert_eq!(our_aggregates(), 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_aggregates_have_an_owner() {
        let uid = format!("{AGGREGATE_UID_PREFIX}4242.3");
        assert_eq!(aggregate_owner_pid(&uid), Some(4242));
        for other in [
            "BuiltInSpeakerDevice",
            "com.flowingthoughts.spike.tap-aggregate.4242.3",
            "com.flowingthoughts.meetings.tap-aggregate.",
            "com.flowingthoughts.meetings.tap-aggregate.4242",
            "com.flowingthoughts.meetings.tap-aggregate.abc.1",
            "com.flowingthoughts.meetings.tap-aggregate.4242.x",
        ] {
            assert_eq!(aggregate_owner_pid(other), None, "{other}");
        }
    }

    #[test]
    fn leftovers_are_ours_or_a_dead_process_never_a_live_one() {
        let uid = |pid: u32, n: u32| format!("{AGGREGATE_UID_PREFIX}{pid}.{n}");
        let alive = |pid: u32| pid == 200;
        let live = vec![uid(100, 1)];
        assert!(
            is_leaked_aggregate(&uid(100, 0), 100, &live, alive),
            "this process: left by an earlier start"
        );
        assert!(
            !is_leaked_aggregate(&uid(100, 1), 100, &live, alive),
            "this process: in use right now"
        );
        assert!(
            is_leaked_aggregate(&uid(300, 0), 100, &live, alive),
            "a process that is gone"
        );
        assert!(
            !is_leaked_aggregate(&uid(200, 0), 100, &live, alive),
            "a second copy of the app that is running"
        );
        assert!(!is_leaked_aggregate(
            "AppleUSBAudioEngine:1",
            100,
            &live,
            alive
        ));
    }

    #[test]
    fn the_monitor_reads_what_the_tap_thread_wrote() {
        let shared = Arc::new(Shared::default());
        let monitor = SystemAudioMonitor(shared.clone());
        assert_eq!(monitor.notice(), SystemAudioNotice::None);
        assert!(!monitor.heard_audio() && !monitor.echo_risk());
        shared.notice.store(
            SystemAudioNotice::NoAudioDetected.as_u8(),
            Ordering::Relaxed,
        );
        shared.nonzero_callbacks.store(3, Ordering::Relaxed);
        shared.echo_risk.store(true, Ordering::Relaxed);
        assert!(monitor.notice().is_problem());
        assert!(monitor.heard_audio() && monitor.echo_risk());
    }
}
