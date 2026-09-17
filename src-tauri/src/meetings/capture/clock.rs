//! OWNER: WP5 (capture). The mach host clock, in nanoseconds.
//!
//! Both tracks are stamped on this clock: it is what Core Audio's `mHostTime`
//! counts in, and what `AudioFrames::host_time_ns` promises. Everything here
//! is integer math and safe on the real-time audio thread.

// libc marks the mach time bindings deprecated in favour of `mach2`, which is
// not a dependency of this crate.
#![allow(deprecated)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timebase {
    pub numer: u32,
    pub denom: u32,
}

#[cfg(target_os = "macos")]
pub fn timebase() -> Timebase {
    use std::sync::OnceLock;
    static TIMEBASE: OnceLock<Timebase> = OnceLock::new();
    *TIMEBASE.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `info` is a valid out-pointer.
        unsafe { libc::mach_timebase_info(&mut info) };
        if info.denom == 0 {
            Timebase { numer: 1, denom: 1 }
        } else {
            Timebase { numer: info.numer, denom: info.denom }
        }
    })
}

#[cfg(not(target_os = "macos"))]
pub fn timebase() -> Timebase {
    Timebase { numer: 1, denom: 1 }
}

/// Host ticks (`mach_absolute_time`, `AudioTimeStamp::mHostTime`) to
/// nanoseconds.
#[inline]
pub fn ticks_to_ns(timebase: Timebase, ticks: u64) -> u64 {
    ((ticks as u128 * timebase.numer as u128) / timebase.denom.max(1) as u128) as u64
}

/// Now, on the host clock. Read the timebase once, outside the callback, and
/// pass it in.
#[cfg(target_os = "macos")]
#[inline]
pub fn now_ns(timebase: Timebase) -> u64 {
    // SAFETY: no preconditions.
    ticks_to_ns(timebase, unsafe { libc::mach_absolute_time() })
}

#[cfg(not(target_os = "macos"))]
#[inline]
pub fn now_ns(_timebase: Timebase) -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// The meeting origin for `recording::start_track`: both tracks take the same
/// value.
pub fn host_now_ns() -> u64 {
    now_ns(timebase())
}

/// Duration of `frames` at `sample_rate`. Real-time safe.
#[inline]
pub fn frames_to_ns(frames: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    (frames as u128 * 1_000_000_000 / sample_rate as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_convert_with_the_apple_silicon_timebase() {
        let timebase = Timebase { numer: 125, denom: 3 };
        assert_eq!(ticks_to_ns(timebase, 24), 1_000);
        // A week of uptime must not overflow.
        let week_ticks = 7 * 24 * 3_600 * 24_000_000_u64;
        assert_eq!(ticks_to_ns(timebase, week_ticks), 7 * 24 * 3_600 * 1_000_000_000);
        assert_eq!(ticks_to_ns(Timebase { numer: 1, denom: 0 }, 5), 5);
    }

    #[test]
    fn the_host_clock_moves_forward() {
        let a = host_now_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(host_now_ns() > a);
    }

    #[test]
    fn buffer_durations() {
        assert_eq!(frames_to_ns(480, 24_000), 20_000_000);
        assert_eq!(frames_to_ns(512, 48_000), 10_666_666);
        assert_eq!(frames_to_ns(512, 0), 0);
    }
}
