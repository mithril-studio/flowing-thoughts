//! OWNER: WP5 (capture). Device changes: the HAL listeners, the rules for
//! when a source rebuilds, and "is the output the built-in speakers?".
//!
//! Three parts:
//!
//! - `RebuildPlanner`: pure logic with an injected clock. It turns device
//!   events into "rebuild now", and owns every timeout, so no source can hang.
//! - `OutputGate`: lets the microphone wait until the output side is running
//!   again before it rebuilds.
//! - The hub (macOS): one process-wide set of HAL property listeners that
//!   fans events out to the sources. Events leave the HAL notification thread
//!   right away; all rebuilding happens on the sources' own threads.
//!
//! What the spike measured and this is built for:
//!
//! - Removing AirPods made the default device flap three times in 3.7 s. A
//!   rebuild onto the dying device gets no callbacks. So: wait until events
//!   have been quiet for 300 ms, and abandon a wait for the first callback as
//!   soon as a newer event arrives.
//! - cpal tearing down a Bluetooth input stream serializes the HAL for
//!   seconds, and the tap's IOProc only started firing once that was over. So
//!   the microphone does not rebuild until the output side runs again.
//! - While the permission is undetermined the IOProc may never fire. A missing
//!   first callback degrades the source; it retries with a backoff.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::meetings::echo::output_has_echo_risk;

/// Why a source rebuilds. Only used for logs and status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildReason {
    DefaultDeviceChanged,
    SampleRateChanged,
    /// cpal reported a stream error (device unplugged).
    StreamError,
    /// Callbacks stopped without a device event (sleep, a wedged device).
    Stalled,
    /// An earlier build never delivered; trying again.
    Retry,
}

impl RebuildReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RebuildReason::DefaultDeviceChanged => "default device changed",
            RebuildReason::SampleRateChanged => "sample rate changed",
            RebuildReason::StreamError => "stream error",
            RebuildReason::Stalled => "stalled",
            RebuildReason::Retry => "retry",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlannerConfig {
    /// Events must have been quiet this long before a rebuild starts.
    pub quiet: Duration,
    /// A build that has not delivered by then is degraded.
    pub first_callback_timeout: Duration,
    /// How long a rebuild may be held back by `poll`'s `blocked` flag.
    pub max_blocked: Duration,
    /// Delays between retries of a degraded source. The last one repeats.
    pub retry_backoff: Vec<Duration>,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            quiet: Duration::from_millis(300),
            first_callback_timeout: Duration::from_secs(3),
            max_blocked: Duration::from_secs(5),
            retry_backoff: [5, 10, 20, 30].map(Duration::from_secs).to_vec(),
        }
    }
}

/// Reasons to hold a rebuild back, as the source's thread sees them right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hold {
    /// Something else must recover first: the microphone waits for the output
    /// side. Delays the rebuild after a device event by at most `max_blocked`.
    pub settle: bool,
    /// Retrying is pointless right now: the tap is silent because nothing is
    /// playing. Holds the retry of a degraded source for as long as it lasts.
    pub retry: bool,
}

/// What the source's thread should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Wait,
    /// Tear down and build again, then call `on_built`.
    Rebuild(RebuildReason),
    /// The last build never delivered a callback. Report it (the meeting
    /// carries on with the other track); a retry is already scheduled.
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Delivering audio.
    Running,
    Settling {
        first_event: Instant,
        last_event: Instant,
        reason: RebuildReason,
    },
    /// `Rebuild` was handed out; waiting for `on_built`.
    Building,
    AwaitingFirstCallback {
        since: Instant,
    },
    Degraded {
        retry_at: Instant,
    },
}

/// Decides when a source rebuilds. It has no clock and does no I/O: the
/// source's thread feeds it events and the time, and acts on what `poll`
/// returns.
#[derive(Debug, Clone)]
pub struct RebuildPlanner {
    config: PlannerConfig,
    state: State,
    /// Builds in a row that never delivered.
    failures: usize,
}

impl RebuildPlanner {
    /// A planner for a source that was just built: `on_built` has happened.
    pub fn new(config: PlannerConfig, built_at: Instant) -> Self {
        Self {
            config,
            state: State::AwaitingFirstCallback { since: built_at },
            failures: 0,
        }
    }

    /// A device event, a stream error or a stall. Whatever was going on, the
    /// quiet period starts again; a wait for the first callback is abandoned.
    pub fn on_event(&mut self, now: Instant, reason: RebuildReason) {
        let first_event = match self.state {
            State::Settling { first_event, .. } => first_event,
            _ => now,
        };
        self.failures = 0;
        self.state = State::Settling {
            first_event,
            last_event: now,
            reason,
        };
    }

    /// The rebuild `poll` asked for is done. Feed the events that arrived
    /// during the build *after* this call: they abandon the wait.
    pub fn on_built(&mut self, now: Instant, ok: bool) {
        if self.state != State::Building {
            return;
        }
        if ok {
            self.state = State::AwaitingFirstCallback { since: now };
        } else {
            self.degrade(now);
        }
    }

    /// Callbacks arrived. Ends the wait after a build, and also a degraded
    /// state: a tap that was silent for want of anything playing needs no
    /// retry once it delivers.
    pub fn on_first_callback(&mut self) {
        if matches!(
            self.state,
            State::AwaitingFirstCallback { .. } | State::Degraded { .. }
        ) {
            self.state = State::Running;
            self.failures = 0;
        }
    }

    pub fn is_running(&self) -> bool {
        self.state == State::Running
    }

    pub fn is_awaiting_first_callback(&self) -> bool {
        matches!(self.state, State::AwaitingFirstCallback { .. })
    }

    /// Built, but not delivering: waiting for the first callback or degraded.
    pub fn is_waiting_for_callbacks(&self) -> bool {
        matches!(
            self.state,
            State::AwaitingFirstCallback { .. } | State::Degraded { .. }
        )
    }

    pub fn poll(&mut self, now: Instant, hold: Hold) -> Action {
        match self.state {
            State::Running | State::Building => Action::Wait,
            State::Settling {
                first_event,
                last_event,
                reason,
            } => {
                if now.saturating_duration_since(last_event) < self.config.quiet {
                    return Action::Wait;
                }
                let held = now.saturating_duration_since(first_event);
                if hold.settle && held < self.config.quiet + self.config.max_blocked {
                    return Action::Wait;
                }
                self.state = State::Building;
                Action::Rebuild(reason)
            }
            State::AwaitingFirstCallback { since } => {
                if now.saturating_duration_since(since) < self.config.first_callback_timeout {
                    return Action::Wait;
                }
                self.degrade(now);
                Action::Degraded
            }
            State::Degraded { retry_at } => {
                if now < retry_at || hold.retry {
                    return Action::Wait;
                }
                self.state = State::Building;
                Action::Rebuild(RebuildReason::Retry)
            }
        }
    }

    /// How long the thread may sleep before `poll` could return something
    /// else. `None`: nothing is due; look again at the next event or tick
    /// (a deadline that has passed is being held back).
    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        let at = match self.state {
            State::Running | State::Building => return None,
            State::Settling { last_event, .. } => last_event + self.config.quiet,
            State::AwaitingFirstCallback { since } => since + self.config.first_callback_timeout,
            State::Degraded { retry_at } => retry_at,
        };
        (at > now).then(|| at - now)
    }

    fn degrade(&mut self, now: Instant) {
        let backoff = &self.config.retry_backoff;
        let delay = backoff
            .get(self.failures.min(backoff.len().saturating_sub(1)))
            .copied()
            .unwrap_or(Duration::from_secs(30));
        self.failures += 1;
        self.state = State::Degraded {
            retry_at: now + delay,
        };
    }
}

/// "Is the output side still recovering?" The system tap notes every output
/// event the moment the HAL reports it and settles once it delivers audio
/// again (or has given up). The microphone holds its rebuild while `is_busy`.
///
/// Generations instead of a flag: settling an old rebuild must not hide an
/// event that arrived in the meantime.
#[derive(Debug, Default)]
pub struct OutputGate {
    seen: AtomicU64,
    handled: AtomicU64,
}

impl OutputGate {
    pub const fn new() -> Self {
        Self {
            seen: AtomicU64::new(0),
            handled: AtomicU64::new(0),
        }
    }

    /// An output event arrived. Returns its generation.
    pub fn note_event(&self) -> u64 {
        self.seen.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn generation(&self) -> u64 {
        self.seen.load(Ordering::SeqCst)
    }

    /// Everything up to `generation` has been dealt with.
    pub fn settle(&self, generation: u64) {
        self.handled.fetch_max(generation, Ordering::SeqCst);
    }

    /// Nothing is recovering any more (the tap stopped).
    pub fn reset(&self) {
        self.settle(self.generation());
    }

    pub fn is_busy(&self) -> bool {
        self.handled.load(Ordering::SeqCst) < self.seen.load(Ordering::SeqCst)
    }
}

/// The process-wide gate between the system tap and the microphone.
pub fn output_gate() -> &'static OutputGate {
    static GATE: OutputGate = OutputGate::new();
    &GATE
}

/// Built-in output that is not the headphone jack. On Macs where the jack is
/// a data source of the one built-in device it reads `'hdpn'`, which is
/// `echo::output_has_echo_risk`'s decision; where it is a device of its own
/// it is named "External Headphones".
pub fn is_builtin_speakers(
    transport: Option<u32>,
    data_source: Option<u32>,
    name: Option<&str>,
) -> bool {
    transport.is_some_and(|transport| output_has_echo_risk(transport, data_source))
        && !name.is_some_and(|n| n.to_ascii_lowercase().contains("headphone"))
}

/// Whether the current default output is the built-in speakers: the session
/// (WP7) stores it as `meetings.echo_risk` and shows "Use headphones for best
/// results". `false` when it cannot be told.
pub fn output_is_builtin_speakers() -> bool {
    #[cfg(target_os = "macos")]
    {
        use super::hal;
        let Ok(device) = hal::default_output_device() else {
            return false;
        };
        is_builtin_speakers(
            hal::transport_type(device),
            hal::output_data_source(device),
            hal::device_name(device).as_deref(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// What the HAL reported. Device ids are `AudioObjectID`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEvent {
    DefaultOutputChanged,
    DefaultInputChanged,
    /// The nominal sample rate of a device watched with `watch_sample_rate`.
    SampleRateChanged {
        device: u32,
    },
}

#[cfg(target_os = "macos")]
pub use hub::{subscribe, unwatch_sample_rate, watch_sample_rate};

#[cfg(target_os = "macos")]
mod hub {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use objc2_core_audio::{
        kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultInputDevice,
        kAudioHardwarePropertyDefaultOutputDevice, AudioObjectAddPropertyListener, AudioObjectID,
        AudioObjectPropertyAddress, AudioObjectRemovePropertyListener,
    };

    use super::super::hal::{self, OSStatus};
    use super::DeviceEvent;

    type Sink = Box<dyn Fn(DeviceEvent) + Send>;

    #[derive(Default)]
    struct Hub {
        next_id: u64,
        sinks: Vec<(u64, Sink)>,
        /// Sample-rate listeners per device, counted: the microphone and the
        /// tap can watch the same device (a headset).
        rate_watches: HashMap<AudioObjectID, usize>,
        system_listeners: bool,
    }

    fn hub() -> MutexGuard<'static, Hub> {
        static HUB: OnceLock<Mutex<Hub>> = OnceLock::new();
        HUB.get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Runs on a HAL notification thread (not the real-time one). It only
    /// forwards: the sinks push into channels. No HAL call may be made from
    /// here or while holding the hub's lock.
    unsafe extern "C-unwind" fn listener_proc(
        object: AudioObjectID,
        n_addresses: u32,
        addresses: NonNull<AudioObjectPropertyAddress>,
        _client: *mut c_void,
    ) -> OSStatus {
        let addresses = std::slice::from_raw_parts(addresses.as_ptr(), n_addresses as usize);
        let hub = hub();
        for address in addresses {
            #[allow(non_upper_case_globals)]
            let event = match address.mSelector {
                kAudioHardwarePropertyDefaultOutputDevice => DeviceEvent::DefaultOutputChanged,
                kAudioHardwarePropertyDefaultInputDevice => DeviceEvent::DefaultInputChanged,
                kAudioDevicePropertyNominalSampleRate => {
                    DeviceEvent::SampleRateChanged { device: object }
                }
                _ => continue,
            };
            for (_, sink) in &hub.sinks {
                sink(event);
            }
        }
        0
    }

    fn add_listener(object: AudioObjectID, selector: u32) -> OSStatus {
        let address = hal::address(selector);
        // SAFETY: a static function and a null client: nothing to outlive.
        unsafe {
            AudioObjectAddPropertyListener(
                object,
                NonNull::from(&address),
                Some(listener_proc),
                std::ptr::null_mut(),
            )
        }
    }

    fn remove_listener(object: AudioObjectID, selector: u32) -> OSStatus {
        let address = hal::address(selector);
        // SAFETY: as in `add_listener`.
        unsafe {
            AudioObjectRemovePropertyListener(
                object,
                NonNull::from(&address),
                Some(listener_proc),
                std::ptr::null_mut(),
            )
        }
    }

    /// Dropping it ends the subscription.
    pub struct Subscription {
        id: u64,
    }

    impl Drop for Subscription {
        fn drop(&mut self) {
            hub().sinks.retain(|(id, _)| *id != self.id);
        }
    }

    /// `sink` gets every device event, on a HAL notification thread: filter,
    /// push into a channel, return. The default-device listeners are added on
    /// first use and stay for the life of the process.
    pub fn subscribe(sink: impl Fn(DeviceEvent) + Send + 'static) -> Result<Subscription, String> {
        let (id, install) = {
            let mut hub = hub();
            hub.next_id += 1;
            let id = hub.next_id;
            hub.sinks.push((id, Box::new(sink)));
            let install = !hub.system_listeners;
            hub.system_listeners = true;
            (id, install)
        };
        let subscription = Subscription { id };
        if install {
            for selector in [
                kAudioHardwarePropertyDefaultOutputDevice,
                kAudioHardwarePropertyDefaultInputDevice,
            ] {
                let status = add_listener(hal::SYSTEM_OBJECT, selector);
                if status != 0 {
                    hub().system_listeners = false;
                    return Err(format!(
                        "Failed to listen for audio device changes: {}",
                        hal::status_str(status)
                    ));
                }
            }
        }
        Ok(subscription)
    }

    /// Delivers `SampleRateChanged { device }` until `unwatch_sample_rate`.
    pub fn watch_sample_rate(device: AudioObjectID) {
        let first = {
            let mut hub = hub();
            let count = hub.rate_watches.entry(device).or_insert(0);
            *count += 1;
            *count == 1
        };
        if first {
            let _ = add_listener(device, kAudioDevicePropertyNominalSampleRate);
        }
    }

    pub fn unwatch_sample_rate(device: AudioObjectID) {
        let last = {
            let mut hub = hub();
            match hub.rate_watches.get_mut(&device) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => {
                    hub.rate_watches.remove(&device);
                    true
                }
                None => false,
            }
        };
        if last {
            // Fails when the device is already gone; nothing to clean up then.
            let _ = remove_listener(device, kAudioDevicePropertyNominalSampleRate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A scripted world around the planner: device events at fixed times, a
    /// build that takes `build_ms`, and a first callback `first_callback_ms`
    /// after a build, but only when the build started after `device_stable_at`
    /// (a build onto a device that is going away never delivers).
    struct Script {
        events_at: Vec<u64>,
        build_ms: u64,
        first_callback_ms: u64,
        device_stable_at: u64,
        blocked_until: u64,
    }

    #[derive(Debug, Default)]
    struct Outcome {
        rebuilds_at: Vec<u64>,
        degraded_at: Vec<u64>,
        running_at: Option<u64>,
    }

    fn run(script: &Script, config: PlannerConfig, until_ms: u64) -> Outcome {
        let t0 = Instant::now();
        // The source was built before the script starts and is delivering.
        let mut planner = RebuildPlanner::new(config, t0);
        planner.on_first_callback();
        let mut outcome = Outcome::default();
        let mut events = script.events_at.clone();
        events.sort_unstable();
        let mut first_callback_at: Option<u64> = None;
        let mut now = 0u64;
        while now <= until_ms {
            while events.first().is_some_and(|at| *at <= now) {
                let at = events.remove(0);
                planner.on_event(t0 + ms(at), RebuildReason::DefaultDeviceChanged);
                first_callback_at = None;
                outcome.running_at = None;
            }
            if first_callback_at.is_some_and(|at| at <= now) && planner.is_awaiting_first_callback()
            {
                planner.on_first_callback();
                outcome.running_at = Some(now);
                first_callback_at = None;
            }
            match planner.poll(
                t0 + ms(now),
                Hold {
                    settle: now < script.blocked_until,
                    retry: false,
                },
            ) {
                Action::Wait => now += 1,
                Action::Degraded => {
                    outcome.degraded_at.push(now);
                    now += 1;
                }
                Action::Rebuild(_) => {
                    outcome.rebuilds_at.push(now);
                    let started = now;
                    // The thread is inside HAL calls; events queue up.
                    now += script.build_ms;
                    planner.on_built(t0 + ms(now), true);
                    first_callback_at = (started >= script.device_stable_at)
                        .then_some(now + script.first_callback_ms);
                }
            }
        }
        outcome
    }

    #[test]
    fn a_single_device_change_rebuilds_once_after_the_quiet_period() {
        let script = Script {
            events_at: vec![1_000],
            build_ms: 285,
            first_callback_ms: 40,
            device_stable_at: 0,
            blocked_until: 0,
        };
        let outcome = run(&script, PlannerConfig::default(), 10_000);
        assert_eq!(outcome.rebuilds_at, vec![1_300]);
        assert!(outcome.degraded_at.is_empty());
        assert_eq!(outcome.running_at, Some(1_625));
    }

    #[test]
    fn a_burst_of_events_is_one_rebuild() {
        let script = Script {
            events_at: vec![0, 50, 120, 380, 600],
            build_ms: 285,
            first_callback_ms: 40,
            device_stable_at: 0,
            blocked_until: 0,
        };
        let outcome = run(&script, PlannerConfig::default(), 10_000);
        assert_eq!(
            outcome.rebuilds_at,
            vec![900],
            "300 ms after the last event"
        );
    }

    /// Finding F4: removing AirPods flapped the default output three times in
    /// 3.7 s (speakers, AirPods, speakers). The first two rebuilds land on a
    /// device that is going away and never deliver. Each newer event must
    /// abandon that wait at once, and the last rebuild must be running within
    /// 2 s of the last event, without ever reporting the track as degraded.
    #[test]
    fn three_flaps_in_under_four_seconds_recover_within_two_seconds() {
        let script = Script {
            events_at: vec![0, 1_900, 3_700],
            build_ms: 285,
            first_callback_ms: 60,
            device_stable_at: 3_700,
            blocked_until: 0,
        };
        let outcome = run(&script, PlannerConfig::default(), 20_000);
        assert_eq!(outcome.rebuilds_at, vec![300, 2_200, 4_000]);
        assert!(
            outcome.degraded_at.is_empty(),
            "the waits were abandoned, not timed out"
        );
        let running_at = outcome.running_at.expect("recovered");
        assert!(
            running_at - 3_700 < 2_000,
            "recovered {} ms after the last flap",
            running_at - 3_700
        );
    }

    #[test]
    fn an_event_during_a_build_abandons_the_wait_for_its_first_callback() {
        let t0 = Instant::now();
        let mut planner = RebuildPlanner::new(PlannerConfig::default(), t0);
        planner.on_first_callback();
        planner.on_event(t0, RebuildReason::DefaultDeviceChanged);
        assert_eq!(
            planner.poll(t0 + ms(300), Hold::default()),
            Action::Rebuild(RebuildReason::DefaultDeviceChanged)
        );
        // The build took 2.5 s (a stalled HAL) and an event arrived meanwhile.
        planner.on_built(t0 + ms(2_800), true);
        planner.on_event(t0 + ms(2_800), RebuildReason::SampleRateChanged);
        assert!(!planner.is_awaiting_first_callback());
        assert_eq!(planner.poll(t0 + ms(2_900), Hold::default()), Action::Wait);
        assert_eq!(
            planner.poll(t0 + ms(3_100), Hold::default()),
            Action::Rebuild(RebuildReason::SampleRateChanged)
        );
    }

    /// Finding F5: while the permission is undetermined the IOProc may never
    /// fire. That must end in "degraded" and scheduled retries, never a hang.
    #[test]
    fn a_missing_first_callback_degrades_and_retries_with_backoff() {
        let t0 = Instant::now();
        let mut planner = RebuildPlanner::new(PlannerConfig::default(), t0);
        assert_eq!(planner.poll(t0 + ms(2_999), Hold::default()), Action::Wait);
        assert_eq!(
            planner.poll(t0 + ms(3_000), Hold::default()),
            Action::Degraded
        );
        assert_eq!(planner.poll(t0 + ms(7_999), Hold::default()), Action::Wait);
        assert_eq!(planner.next_deadline(t0 + ms(7_000)), Some(ms(1_000)));
        assert_eq!(
            planner.poll(t0 + ms(8_000), Hold::default()),
            Action::Rebuild(RebuildReason::Retry)
        );
        planner.on_built(t0 + ms(8_300), true);
        assert_eq!(
            planner.poll(t0 + ms(11_300), Hold::default()),
            Action::Degraded
        );
        // Second retry after 10 s, not 5.
        assert_eq!(planner.poll(t0 + ms(21_299), Hold::default()), Action::Wait);
        assert_eq!(
            planner.poll(t0 + ms(21_300), Hold::default()),
            Action::Rebuild(RebuildReason::Retry)
        );
        // This one delivers: the backoff starts over.
        planner.on_built(t0 + ms(21_600), true);
        planner.on_first_callback();
        assert!(planner.is_running());
        assert_eq!(planner.next_deadline(t0 + ms(30_000)), None);
    }

    /// Found on real hardware: with the built-in speakers and nothing playing
    /// the IOProc does not fire at all, and starts as soon as anything plays.
    /// That is not a fault: no retries while it lasts, and no retry once the
    /// callbacks arrive by themselves.
    #[test]
    fn a_silent_tap_is_not_retried_while_nothing_plays_and_recovers_by_itself() {
        let t0 = Instant::now();
        let idle = Hold {
            settle: false,
            retry: true,
        };
        let mut planner = RebuildPlanner::new(PlannerConfig::default(), t0);
        assert_eq!(planner.poll(t0 + ms(3_000), idle), Action::Degraded);
        assert_eq!(planner.poll(t0 + ms(8_000), idle), Action::Wait);
        assert_eq!(planner.poll(t0 + ms(600_000), idle), Action::Wait);
        assert!(planner.is_waiting_for_callbacks());
        // Somebody starts talking: callbacks arrive, nothing is rebuilt.
        planner.on_first_callback();
        assert!(planner.is_running());
        assert_eq!(
            planner.poll(t0 + ms(600_001), Hold::default()),
            Action::Wait
        );

        // Whereas with audio playing and still no callbacks, it retries.
        let mut planner = RebuildPlanner::new(PlannerConfig::default(), t0);
        assert_eq!(
            planner.poll(t0 + ms(3_000), Hold::default()),
            Action::Degraded
        );
        assert_eq!(
            planner.poll(t0 + ms(8_000), Hold::default()),
            Action::Rebuild(RebuildReason::Retry)
        );
    }

    /// A device change is never held back by "nothing is playing".
    #[test]
    fn a_device_change_rebuilds_a_silent_tap_too() {
        let t0 = Instant::now();
        let idle = Hold {
            settle: false,
            retry: true,
        };
        let mut planner = RebuildPlanner::new(PlannerConfig::default(), t0);
        assert_eq!(planner.poll(t0 + ms(3_000), idle), Action::Degraded);
        planner.on_event(t0 + ms(4_000), RebuildReason::DefaultDeviceChanged);
        assert_eq!(
            planner.poll(t0 + ms(4_300), idle),
            Action::Rebuild(RebuildReason::DefaultDeviceChanged)
        );
    }

    #[test]
    fn a_failed_build_is_retried_too() {
        let t0 = Instant::now();
        let mut planner = RebuildPlanner::new(PlannerConfig::default(), t0);
        planner.on_first_callback();
        planner.on_event(t0, RebuildReason::StreamError);
        assert_eq!(
            planner.poll(t0 + ms(300), Hold::default()),
            Action::Rebuild(RebuildReason::StreamError)
        );
        planner.on_built(t0 + ms(400), false);
        assert_eq!(planner.poll(t0 + ms(5_399), Hold::default()), Action::Wait);
        assert_eq!(
            planner.poll(t0 + ms(5_400), Hold::default()),
            Action::Rebuild(RebuildReason::Retry)
        );
    }

    #[test]
    fn the_microphone_waits_for_the_output_side_but_not_for_ever() {
        let base = Script {
            events_at: vec![0],
            build_ms: 200,
            first_callback_ms: 40,
            device_stable_at: 0,
            blocked_until: 900,
        };
        let outcome = run(&base, PlannerConfig::default(), 20_000);
        assert_eq!(
            outcome.rebuilds_at,
            vec![900],
            "as soon as the output side runs again"
        );

        let stuck = Script {
            blocked_until: u64::MAX,
            ..base
        };
        let outcome = run(&stuck, PlannerConfig::default(), 20_000);
        assert_eq!(
            outcome.rebuilds_at,
            vec![5_300],
            "quiet period plus max_blocked"
        );
    }

    #[test]
    fn the_output_gate_is_not_settled_by_an_older_rebuild() {
        let gate = OutputGate::new();
        assert!(!gate.is_busy());
        let first = gate.note_event();
        assert!(gate.is_busy());
        // A second event arrives while the rebuild for the first one runs.
        gate.note_event();
        gate.settle(first);
        assert!(gate.is_busy(), "the newer event is still unhandled");
        gate.settle(gate.generation());
        assert!(!gate.is_busy());
        // Settling never goes backwards.
        gate.settle(first);
        assert!(!gate.is_busy());
        gate.note_event();
        gate.reset();
        assert!(!gate.is_busy());
    }

    #[test]
    fn echo_risk_means_built_in_speakers_only() {
        use crate::meetings::echo::{DATA_SOURCE_HEADPHONES, TRANSPORT_BUILT_IN};
        let builtin = Some(TRANSPORT_BUILT_IN);
        let speakers = Some(u32::from_be_bytes(*b"ispk"));
        let bluetooth = Some(u32::from_be_bytes(*b"blue"));
        assert!(is_builtin_speakers(
            builtin,
            speakers,
            Some("MacBook Pro Speakers")
        ));
        assert!(is_builtin_speakers(
            builtin,
            None,
            Some("Mac mini Speakers")
        ));
        assert!(!is_builtin_speakers(
            builtin,
            Some(DATA_SOURCE_HEADPHONES),
            Some("Built-in Output")
        ));
        assert!(!is_builtin_speakers(
            builtin,
            None,
            Some("External Headphones")
        ));
        assert!(!is_builtin_speakers(
            bluetooth,
            None,
            Some("Joost’s AirPods Pro")
        ));
        assert!(!is_builtin_speakers(None, None, None));
    }
}
