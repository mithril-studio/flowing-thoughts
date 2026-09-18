//! OWNER: WP5 (capture). System Audio Recording permission.
//!
//! `check()` and `open_settings()` are called by `commands.rs` and their
//! signatures are fixed. macOS reports a denial as silence, not as an error,
//! so there are two layers:
//!
//! 1. `check()`: best-effort `TCCAccessPreflight("kTCCServiceAudioCapture")`
//!    loaded with `dlopen`, behind the `private-tcc` cargo feature. A missing
//!    framework or symbol, or a build without the feature, is `unknown`.
//! 2. `SilenceDetector`, run by the system tap once a second. The session
//!    (WP7) reads the result from `SystemAudioMonitor::notice()` and owns
//!    `RecordingStatus::system_audio_silent` and the event.
//!
//! Exact digital zeros are what the tap delivers whenever nothing is playing
//! (spike finding F3), and on some outputs it does not call back at all until
//! something plays (seen on the built-in speakers). So neither zeros nor
//! silence on the line say anything by themselves. The detector only speaks
//! up when the tap delivers zeros, or nothing, *while another process is
//! playing audio*, or when preflight says denied.
//!
//! A notice is never an error: the meeting continues with the microphone.

use std::time::{Duration, Instant};

use super::super::types::{PermissionState, PermissionStatus};

/// Zeros while something is playing, for this long, raise the notice.
pub const SILENCE_THRESHOLD: Duration = Duration::from_secs(20);
/// No callbacks at all while something is playing, for this long, raise it.
pub const NOT_DELIVERING_THRESHOLD: Duration = Duration::from_secs(3);

/// What `TCCAccessPreflight` returned, or why it could not be asked.
pub type PreflightResult = Result<i32, String>;

pub fn check() -> PermissionStatus {
    if let Err(reason) = super::support() {
        return PermissionStatus {
            state: PermissionState::Unsupported,
            detail: Some(format!(
                "Recording system audio needs macOS 14.4 or later ({reason})."
            )),
        };
    }
    map_preflight(&preflight_audio_capture())
}

/// Spike finding F5: 0 is authorized, 1 is denied, anything else (2 was seen)
/// is "not determined yet".
pub fn map_preflight(result: &PreflightResult) -> PermissionStatus {
    let (state, detail) = match result {
        Ok(0) => (PermissionState::Granted, None),
        Ok(1) => (
            PermissionState::Denied,
            Some(
                "System audio recording is turned off for FlowingThoughts in System Settings. \
                 Meetings record the microphone only."
                    .to_string(),
            ),
        ),
        Ok(_) => (
            PermissionState::Unknown,
            Some(
                "macOS asks for permission the first time a meeting records system audio."
                    .to_string(),
            ),
        ),
        Err(reason) => (
            PermissionState::Unknown,
            Some(format!(
                "The permission cannot be checked ahead of a recording ({reason})."
            )),
        ),
    };
    PermissionStatus { state, detail }
}

/// Private API, so nothing links against it: the framework is opened with
/// `dlopen` and every failure is an `Err` the caller shows as `unknown`.
#[cfg(all(target_os = "macos", feature = "private-tcc"))]
pub fn preflight_audio_capture() -> PreflightResult {
    use std::ffi::{c_int, c_void, CStr};
    use std::sync::OnceLock;

    use objc2_core_foundation::CFString;

    const TCC_PATH: &CStr = c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC";
    type PreflightFn = unsafe extern "C" fn(*const c_void, *const c_void) -> c_int;

    static PREFLIGHT: OnceLock<Result<PreflightFn, String>> = OnceLock::new();
    let preflight = PREFLIGHT
        .get_or_init(|| {
            // SAFETY: both strings are NUL-terminated; the handle is kept for
            // the life of the process, like the function pointer.
            unsafe {
                let handle = libc::dlopen(TCC_PATH.as_ptr(), libc::RTLD_LAZY);
                if handle.is_null() {
                    return Err("the TCC framework is not available".to_string());
                }
                let symbol = libc::dlsym(handle, c"TCCAccessPreflight".as_ptr());
                if symbol.is_null() {
                    return Err("TCCAccessPreflight is not available".to_string());
                }
                Ok(std::mem::transmute::<*mut c_void, PreflightFn>(symbol))
            }
        })
        .clone()?;
    let service = CFString::from_str("kTCCServiceAudioCapture");
    // SAFETY: `Boolean TCCAccessPreflight(CFStringRef service, CFDictionaryRef options)`.
    Ok(unsafe {
        preflight(
            &*service as *const CFString as *const c_void,
            std::ptr::null(),
        )
    })
}

#[cfg(not(all(target_os = "macos", feature = "private-tcc")))]
pub fn preflight_audio_capture() -> PreflightResult {
    Err("this build has no permission preflight".to_string())
}

/// Opens System Settings → Privacy & Security → Screen & System Audio
/// Recording.
pub fn open_settings() -> Result<(), String> {
    // `Privacy_AudioCapture` is the "System Audio Recording Only" list; the
    // pane it lives in is the fallback.
    let targets = [
        "x-apple.systempreferences:com.apple.preference.security?Privacy_AudioCapture",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        "x-apple.systempreferences:com.apple.preference.security?Privacy",
    ];
    for target in targets {
        if let Ok(exit) = std::process::Command::new("open").arg(target).status() {
            if exit.success() {
                return Ok(());
            }
        }
    }
    Err("Unable to open System Settings automatically. Open Privacy & Security > Screen & System Audio Recording manually.".to_string())
}

/// What the session shows about the system track. Anything but `None` maps to
/// `RecordingStatus::system_audio_silent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAudioNotice {
    None,
    /// Preflight says the permission was denied.
    PermissionDenied,
    /// Only zeros for 20 s while another app was playing audio: most likely
    /// denied.
    NoAudioDetected,
    /// Another app is playing audio and the tap is not calling back at all
    /// (permission still undetermined, or the device is wedged). It keeps
    /// retrying.
    NotDelivering,
}

impl SystemAudioNotice {
    pub fn is_problem(self) -> bool {
        self != SystemAudioNotice::None
    }

    pub(crate) fn as_u8(self) -> u8 {
        match self {
            SystemAudioNotice::None => 0,
            SystemAudioNotice::PermissionDenied => 1,
            SystemAudioNotice::NoAudioDetected => 2,
            SystemAudioNotice::NotDelivering => 3,
        }
    }

    pub(crate) fn from_u8(value: u8) -> Self {
        match value {
            1 => SystemAudioNotice::PermissionDenied,
            2 => SystemAudioNotice::NoAudioDetected,
            3 => SystemAudioNotice::NotDelivering,
            _ => SystemAudioNotice::None,
        }
    }
}

/// One look at the system track, about once a second.
#[derive(Debug, Clone, Copy)]
pub struct Observation {
    /// Callbacks arrived since the last observation.
    pub delivering: bool,
    /// At least one of them held a non-zero sample.
    pub heard_audio: bool,
    /// Another process is playing audio. `None`: the HAL cannot say.
    pub output_running: Option<bool>,
    pub permission: PermissionState,
}

/// The decision table behind the "No system audio detected" notice. Pure: the
/// tap feeds it observations and the time.
///
/// | delivering | heard audio | other app playing | preflight | result |
/// |---|---|---|---|---|
/// | yes | yes | any | any | none, and clears an earlier notice |
/// | any | no | any | denied | `PermissionDenied`, at once |
/// | no | - | yes, for 3 s on end | not denied | `NotDelivering` |
/// | yes | no | yes, for 20 s on end | not denied | `NoAudioDetected` |
/// | any | no | no or unknown | not denied | none: silence is normal |
///
/// A raised notice stays until audio is heard, so it does not flicker when
/// the other app pauses.
#[derive(Debug, Clone)]
pub struct SilenceDetector {
    silence_threshold: Duration,
    not_delivering_threshold: Duration,
    zeros_while_playing_since: Option<Instant>,
    not_delivering_since: Option<Instant>,
    notice: SystemAudioNotice,
}

impl Default for SilenceDetector {
    fn default() -> Self {
        Self::new(SILENCE_THRESHOLD, NOT_DELIVERING_THRESHOLD)
    }
}

impl SilenceDetector {
    pub fn new(silence_threshold: Duration, not_delivering_threshold: Duration) -> Self {
        Self {
            silence_threshold,
            not_delivering_threshold,
            zeros_while_playing_since: None,
            not_delivering_since: None,
            notice: SystemAudioNotice::None,
        }
    }

    pub fn notice(&self) -> SystemAudioNotice {
        self.notice
    }

    pub fn observe(&mut self, now: Instant, observation: Observation) -> SystemAudioNotice {
        let Observation {
            delivering,
            heard_audio,
            output_running,
            permission,
        } = observation;
        if delivering && heard_audio {
            self.zeros_while_playing_since = None;
            self.not_delivering_since = None;
            self.notice = SystemAudioNotice::None;
            return self.notice;
        }

        let playing = output_running == Some(true);
        if playing && delivering {
            self.zeros_while_playing_since.get_or_insert(now);
        } else {
            self.zeros_while_playing_since = None;
        }
        if playing && !delivering {
            self.not_delivering_since.get_or_insert(now);
        } else {
            self.not_delivering_since = None;
        }
        let lasted = |since: Option<Instant>, threshold: Duration| {
            since.is_some_and(|since| now.saturating_duration_since(since) >= threshold)
        };

        if permission == PermissionState::Denied {
            self.notice = SystemAudioNotice::PermissionDenied;
        } else if lasted(self.not_delivering_since, self.not_delivering_threshold) {
            self.notice = SystemAudioNotice::NotDelivering;
        } else if lasted(self.zeros_while_playing_since, self.silence_threshold) {
            self.notice = SystemAudioNotice::NoAudioDetected;
        } else if (self.notice == SystemAudioNotice::NotDelivering && (delivering || !playing))
            || self.notice == SystemAudioNotice::PermissionDenied
        {
            // Callbacks are back or nothing is playing any more, or preflight
            // no longer says denied. Zeros from here on are judged on their
            // own.
            self.notice = SystemAudioNotice::None;
        }
        self.notice
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_results_map_to_permission_states() {
        assert_eq!(map_preflight(&Ok(0)).state, PermissionState::Granted);
        assert!(map_preflight(&Ok(0)).detail.is_none());
        assert_eq!(map_preflight(&Ok(1)).state, PermissionState::Denied);
        // 2 is what the spike saw while undetermined; anything else is
        // treated the same.
        for other in [2, 3, -1, 255] {
            let status = map_preflight(&Ok(other));
            assert_eq!(status.state, PermissionState::Unknown, "{other}");
            assert!(status.detail.is_some());
        }
        let status = map_preflight(&Err("the TCC framework is not available".into()));
        assert_eq!(status.state, PermissionState::Unknown);
        assert!(status.detail.unwrap().contains("TCC framework"));
    }

    #[test]
    fn preflight_never_panics_and_check_never_blocks_a_meeting() {
        // Whatever this machine answers, it is one of the four states.
        let status = check();
        assert!(PermissionState::ALL.contains(&status.state));
        let _ = preflight_audio_capture();
    }

    fn obs(delivering: bool, heard_audio: bool, output_running: Option<bool>) -> Observation {
        Observation {
            delivering,
            heard_audio,
            output_running,
            permission: PermissionState::Unknown,
        }
    }

    /// Feeds the same observation once a second for `secs` seconds.
    fn feed(
        detector: &mut SilenceDetector,
        t0: Instant,
        from: u64,
        secs: u64,
        o: Observation,
    ) -> SystemAudioNotice {
        let mut last = detector.notice();
        for s in from..=from + secs {
            last = detector.observe(t0 + Duration::from_secs(s), o);
        }
        last
    }

    /// Finding F3: with nothing playing the tap delivers exact zeros. An hour
    /// of that is not a problem.
    #[test]
    fn zeros_with_nothing_playing_are_normal() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::default();
        assert_eq!(
            feed(&mut detector, t0, 0, 3_600, obs(true, false, Some(false))),
            SystemAudioNotice::None
        );
        // Nor when the HAL cannot say whether anything is playing.
        assert_eq!(
            feed(&mut detector, t0, 3_601, 3_600, obs(true, false, None)),
            SystemAudioNotice::None
        );
    }

    #[test]
    fn zeros_while_another_app_plays_raise_the_notice_after_twenty_seconds() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::default();
        assert_eq!(
            feed(&mut detector, t0, 0, 19, obs(true, false, Some(true))),
            SystemAudioNotice::None
        );
        assert_eq!(
            detector.observe(t0 + Duration::from_secs(20), obs(true, false, Some(true))),
            SystemAudioNotice::NoAudioDetected
        );
        assert!(detector.notice().is_problem());
    }

    #[test]
    fn the_twenty_seconds_must_be_continuous() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::default();
        feed(&mut detector, t0, 0, 15, obs(true, false, Some(true)));
        // The other app stops playing: the count starts over.
        detector.observe(t0 + Duration::from_secs(16), obs(true, false, Some(false)));
        assert_eq!(
            feed(&mut detector, t0, 17, 15, obs(true, false, Some(true))),
            SystemAudioNotice::None
        );
        // So does hearing something.
        detector.observe(t0 + Duration::from_secs(33), obs(true, true, Some(true)));
        assert_eq!(
            feed(&mut detector, t0, 34, 19, obs(true, false, Some(true))),
            SystemAudioNotice::None
        );
    }

    #[test]
    fn a_notice_stays_until_audio_is_heard() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::default();
        feed(&mut detector, t0, 0, 20, obs(true, false, Some(true)));
        // The other app pauses: no flicker.
        assert_eq!(
            detector.observe(t0 + Duration::from_secs(21), obs(true, false, Some(false))),
            SystemAudioNotice::NoAudioDetected
        );
        assert_eq!(
            detector.observe(t0 + Duration::from_secs(22), obs(true, true, Some(true))),
            SystemAudioNotice::None
        );
    }

    #[test]
    fn a_denied_preflight_raises_the_notice_at_once_but_audio_wins() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::default();
        let denied = Observation {
            permission: PermissionState::Denied,
            ..obs(true, false, Some(false))
        };
        assert_eq!(
            detector.observe(t0, denied),
            SystemAudioNotice::PermissionDenied
        );
        // Preflight is best effort (the spike once heard audio while it said
        // "undetermined"): real audio always clears the notice.
        let heard = Observation {
            heard_audio: true,
            ..denied
        };
        assert_eq!(
            detector.observe(t0 + Duration::from_secs(1), heard),
            SystemAudioNotice::None
        );
    }

    /// Found on hardware: on the built-in speakers the IOProc does not fire
    /// at all until something plays. That is an idle tap, not a fault.
    #[test]
    fn no_callbacks_with_nothing_playing_are_normal() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::default();
        assert_eq!(
            feed(&mut detector, t0, 0, 3_600, obs(false, false, Some(false))),
            SystemAudioNotice::None
        );
        assert_eq!(
            feed(&mut detector, t0, 3_601, 600, obs(false, false, None)),
            SystemAudioNotice::None
        );
        // Something plays for two seconds and the callbacks have not started
        // yet, then it stops: still nothing to report.
        feed(&mut detector, t0, 4_300, 2, obs(false, false, Some(true)));
        assert_eq!(
            feed(&mut detector, t0, 4_303, 60, obs(false, false, Some(false))),
            SystemAudioNotice::None
        );
    }

    /// Finding F5: while undetermined the IOProc may not fire at all.
    #[test]
    fn no_callbacks_raise_not_delivering_after_three_seconds_and_clear_when_they_return() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::default();
        assert_eq!(
            feed(&mut detector, t0, 0, 2, obs(false, false, Some(true))),
            SystemAudioNotice::None
        );
        assert_eq!(
            detector.observe(t0 + Duration::from_secs(3), obs(false, false, Some(true))),
            SystemAudioNotice::NotDelivering
        );
        // Callbacks come back with zeros and nothing is playing: fine again.
        assert_eq!(
            detector.observe(t0 + Duration::from_secs(4), obs(true, false, Some(false))),
            SystemAudioNotice::None
        );
    }

    #[test]
    fn notices_round_trip_through_their_atomic_form() {
        for notice in [
            SystemAudioNotice::None,
            SystemAudioNotice::PermissionDenied,
            SystemAudioNotice::NoAudioDetected,
            SystemAudioNotice::NotDelivering,
        ] {
            assert_eq!(SystemAudioNotice::from_u8(notice.as_u8()), notice);
        }
    }
}
