//! OWNER: WP4 (recording). Host-time bookkeeping for one track. Pure logic:
//! no I/O, no clock of its own. Host time only ever comes in with the buffers,
//! which is also what makes it injectable in tests.
//!
//! A *run* is a stretch of contiguous audio anchored to the host time of its
//! first buffer. Every buffer is compared with where the previous one ended:
//!
//! - A hole over 100 ms (a stall, a device rebuild, ring overflow, a pause)
//!   ends the run. The next buffer anchors a new one, which the recorder
//!   turns into `SampleSink::begin`: a new chunk with a `gap_before_ms`.
//!   Long gaps are never zero-filled.
//! - A smaller hole right after a *known* loss (an xrun, a dropped buffer) is
//!   padded with silence so the run stays aligned without a new chunk.
//! - Anything else is jitter or drift and is tolerated. Drift stays bounded to
//!   a few ms per chunk because `rotate` re-anchors every 60 s chunk to the
//!   observed host clock instead of to the sample count.

const NS_PER_MS: u64 = 1_000_000;
const NS_PER_SEC: u64 = 1_000_000_000;

/// A hole longer than this closes the chunk and starts a new anchor.
pub const GAP_THRESHOLD_MS: u64 = 100;

/// Smoothing of the observed clock drift (1/N of each new observation), so a
/// single jittery timestamp does not move the next chunk's anchor.
const DRIFT_SMOOTHING: i64 = 8;

/// Host time to meeting-timeline milliseconds. Before the origin is 0.
pub fn host_to_timeline_ms(origin_host_ns: u64, host_ns: u64) -> u64 {
    host_ns.saturating_sub(origin_host_ns) / NS_PER_MS
}

/// Duration of `frames` at `sample_rate`, rounded to the nearest nanosecond.
pub fn frames_to_ns(frames: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let rate = sample_rate as u128;
    ((frames as u128 * NS_PER_SEC as u128 + rate / 2) / rate) as u64
}

/// Frames that fit in `ns` at `sample_rate`, rounded to the nearest frame.
pub fn ns_to_frames(ns: u64, sample_rate: u32) -> u64 {
    let second = NS_PER_SEC as u128;
    ((ns as u128 * sample_rate as u128 + second / 2) / second) as u64
}

/// Silence between the end of one chunk and the start of the next, as the
/// sidecar records it. Overlap (drift) is 0.
pub fn gap_between_ms(prev_anchor_ns: u64, prev_frames: u64, sample_rate: u32, next_anchor_ns: u64) -> u64 {
    let prev_end_ns = prev_anchor_ns + frames_to_ns(prev_frames, sample_rate);
    next_anchor_ns.saturating_sub(prev_end_ns) / NS_PER_MS
}

/// What to do with a buffer, given its host time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Append to the current run.
    Contiguous,
    /// Frames were lost just before this buffer but the hole is short: pad
    /// with `pad_ns` of silence, then append.
    Pad { pad_ns: u64 },
    /// No run yet, or a hole over the threshold: start a new run here.
    NewRun,
}

/// The start of a run, as handed to `SampleSink::begin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunStart {
    pub anchor_host_ns: u64,
    /// Where the run starts on the meeting timeline.
    pub start_ms: u64,
    /// Silence since the end of the previous run. 0 for the first run.
    pub gap_before_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct Run {
    anchor_host_ns: u64,
    /// Audio placed since the anchor, by sample count.
    nominal_ns: u64,
    /// Smoothed (observed host time - nominal position): how far the device
    /// clock has wandered from the host clock since the anchor.
    drift_ns: i64,
}

impl Run {
    fn expected_next_ns(&self) -> u64 {
        self.anchor_host_ns + self.nominal_ns
    }
}

#[derive(Debug, Clone)]
pub struct Timeline {
    origin_host_ns: u64,
    gap_threshold_ns: u64,
    run: Option<Run>,
    /// Nominal end of the last audio placed, across runs.
    last_end_host_ns: Option<u64>,
    /// Frames were lost since the last buffer.
    loss_pending: bool,
}

impl Timeline {
    #[cfg(test)]
    pub fn new(origin_host_ns: u64) -> Self {
        Self::with_gap_threshold_ms(origin_host_ns, GAP_THRESHOLD_MS)
    }

    pub fn with_gap_threshold_ms(origin_host_ns: u64, gap_threshold_ms: u64) -> Self {
        Self {
            origin_host_ns,
            gap_threshold_ns: gap_threshold_ms * NS_PER_MS,
            run: None,
            last_end_host_ns: None,
            loss_pending: false,
        }
    }

    pub fn to_timeline_ms(&self, host_ns: u64) -> u64 {
        host_to_timeline_ms(self.origin_host_ns, host_ns)
    }

    pub fn in_run(&self) -> bool {
        self.run.is_some()
    }

    /// Decides where a buffer captured at `host_ns` goes. Does not change
    /// anything: follow up with `start_run` (for `NewRun`) and `advance`.
    pub fn classify(&self, host_ns: u64) -> Placement {
        let Some(run) = &self.run else {
            return Placement::NewRun;
        };
        let expected = run.expected_next_ns();
        if host_ns <= expected {
            // A timestamp slightly in the past is jitter, not a hole.
            return Placement::Contiguous;
        }
        let hole_ns = host_ns - expected;
        if hole_ns > self.gap_threshold_ns {
            Placement::NewRun
        } else if self.loss_pending {
            Placement::Pad { pad_ns: hole_ns }
        } else {
            Placement::Contiguous
        }
    }

    /// Ends the current run (if any) and anchors a new one at `host_ns`. The
    /// anchor never lands before the end of the previous run, so chunks cannot
    /// overlap because of a late timestamp.
    pub fn start_run(&mut self, host_ns: u64) -> RunStart {
        self.end_run();
        let anchor_host_ns = host_ns.max(self.last_end_host_ns.unwrap_or(0));
        let gap_before_ms = match self.last_end_host_ns {
            Some(last_end) => (anchor_host_ns - last_end) / NS_PER_MS,
            None => 0,
        };
        self.run = Some(Run { anchor_host_ns, nominal_ns: 0, drift_ns: 0 });
        self.loss_pending = false;
        RunStart {
            anchor_host_ns,
            start_ms: self.to_timeline_ms(anchor_host_ns),
            gap_before_ms,
        }
    }

    /// Accounts for a buffer of `frames` at `sample_rate` captured at
    /// `host_ns`, appended to the current run.
    pub fn advance(&mut self, host_ns: u64, frames: u64, sample_rate: u32) {
        let Some(run) = &mut self.run else { return };
        let observed = host_ns as i128 - run.expected_next_ns() as i128;
        // A short hole after a known loss is padded by the caller and is not
        // drift; every other small offset is.
        if !self.loss_pending {
            let observed = observed.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            run.drift_ns += (observed - run.drift_ns) / DRIFT_SMOOTHING;
        }
        run.nominal_ns += frames_to_ns(frames, sample_rate);
        self.last_end_host_ns = Some(run.expected_next_ns());
        self.loss_pending = false;
    }

    /// Accounts for silence the caller inserted into the current run.
    pub fn pad(&mut self, pad_ns: u64) {
        if let Some(run) = &mut self.run {
            run.nominal_ns += pad_ns;
            self.last_end_host_ns = Some(run.expected_next_ns());
        }
    }

    /// Frames were lost before the next buffer (xrun, ring overflow). The
    /// hole is measured from the next buffer's host time.
    pub fn note_loss(&mut self) {
        self.loss_pending = true;
    }

    /// Ends the run without starting another (pause, stall, stop). The next
    /// buffer starts a new run whatever its host time is.
    pub fn end_run(&mut self) {
        self.run = None;
    }

    /// A chunk filled up `at_nominal_ns` into the current run. Re-anchors the
    /// run there, corrected by the observed drift, and returns the new anchor
    /// for `SampleSink::begin`. `None` outside a run.
    pub fn rotate(&mut self, at_nominal_ns: u64) -> Option<RunStart> {
        let run = self.run.as_mut()?;
        let at = at_nominal_ns.min(run.nominal_ns);
        let anchor = run.anchor_host_ns as i128 + at as i128 + run.drift_ns as i128;
        let anchor_host_ns = anchor.clamp(run.anchor_host_ns as i128, u64::MAX as i128) as u64;
        run.anchor_host_ns = anchor_host_ns;
        run.nominal_ns -= at;
        run.drift_ns = 0;
        self.last_end_host_ns = Some(run.expected_next_ns());
        Some(RunStart {
            anchor_host_ns,
            start_ms: host_to_timeline_ms(self.origin_host_ns, anchor_host_ns),
            gap_before_ms: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const BUF: u64 = 480; // 10 ms
    const BUF_NS: u64 = 10 * NS_PER_MS;
    const ORIGIN: u64 = 5 * NS_PER_SEC;

    /// Feeds one buffer the way the recorder does and returns the run start
    /// when it anchored a new run.
    fn feed(timeline: &mut Timeline, host_ns: u64) -> (Placement, Option<RunStart>) {
        let placement = timeline.classify(host_ns);
        let mut start = None;
        match placement {
            Placement::NewRun => start = Some(timeline.start_run(host_ns)),
            Placement::Pad { pad_ns } => timeline.pad(pad_ns),
            Placement::Contiguous => {}
        }
        timeline.advance(host_ns, BUF, RATE);
        (placement, start)
    }

    #[test]
    fn conversions_round_to_nearest() {
        assert_eq!(frames_to_ns(16_000, 16_000), NS_PER_SEC);
        assert_eq!(frames_to_ns(441, 44_100), 10 * NS_PER_MS);
        assert_eq!(frames_to_ns(1, 0), 0);
        assert_eq!(ns_to_frames(60 * NS_PER_SEC, 16_000), 960_000);
        assert_eq!(host_to_timeline_ms(ORIGIN, ORIGIN + 1_500 * NS_PER_MS), 1_500);
        assert_eq!(host_to_timeline_ms(ORIGIN, ORIGIN - 1), 0);
        assert_eq!(gap_between_ms(ORIGIN, 16_000, 16_000, ORIGIN + 1_250 * NS_PER_MS), 250);
        assert_eq!(gap_between_ms(ORIGIN, 16_000, 16_000, ORIGIN + 998 * NS_PER_MS), 0);
    }

    #[test]
    fn contiguous_buffers_never_re_anchor() {
        let mut timeline = Timeline::new(ORIGIN);
        let (first, start) = feed(&mut timeline, ORIGIN + 20 * NS_PER_MS);
        assert_eq!(first, Placement::NewRun);
        let start = start.unwrap();
        assert_eq!(start.anchor_host_ns, ORIGIN + 20 * NS_PER_MS);
        assert_eq!(start.start_ms, 20);
        assert_eq!(start.gap_before_ms, 0);
        for i in 1..6_000 {
            let (placement, _) = feed(&mut timeline, ORIGIN + 20 * NS_PER_MS + i * BUF_NS);
            assert_eq!(placement, Placement::Contiguous, "buffer {i}");
        }
    }

    #[test]
    fn a_250_ms_hole_starts_a_new_run_with_the_gap() {
        let mut timeline = Timeline::new(ORIGIN);
        for i in 0..100 {
            feed(&mut timeline, ORIGIN + i * BUF_NS);
        }
        // The run ended at 1000 ms; the next buffer arrives at 1250 ms.
        let (placement, start) = feed(&mut timeline, ORIGIN + 1_250 * NS_PER_MS);
        assert_eq!(placement, Placement::NewRun);
        let start = start.unwrap();
        assert_eq!(start.anchor_host_ns, ORIGIN + 1_250 * NS_PER_MS);
        assert_eq!(start.start_ms, 1_250);
        assert_eq!(start.gap_before_ms, 250);
        let (placement, _) = feed(&mut timeline, ORIGIN + 1_260 * NS_PER_MS);
        assert_eq!(placement, Placement::Contiguous);
    }

    #[test]
    fn jitter_and_overlap_do_not_re_anchor() {
        let mut timeline = Timeline::new(ORIGIN);
        feed(&mut timeline, ORIGIN);
        let offsets_ms: [i64; 6] = [3, -4, 5, -2, 90, -60];
        for (i, offset) in offsets_ms.iter().enumerate() {
            let nominal = ORIGIN + (i as u64 + 1) * BUF_NS;
            let host = (nominal as i64 + offset * NS_PER_MS as i64) as u64;
            let (placement, _) = feed(&mut timeline, host);
            assert_eq!(placement, Placement::Contiguous, "offset {offset} ms");
        }
    }

    #[test]
    fn a_short_hole_after_a_known_loss_is_padded() {
        let mut timeline = Timeline::new(ORIGIN);
        feed(&mut timeline, ORIGIN);
        timeline.note_loss();
        // One 10 ms buffer went missing.
        let (placement, _) = feed(&mut timeline, ORIGIN + 2 * BUF_NS);
        assert_eq!(placement, Placement::Pad { pad_ns: BUF_NS });
        // Padded and advanced: the next buffer is contiguous again.
        let (placement, _) = feed(&mut timeline, ORIGIN + 3 * BUF_NS);
        assert_eq!(placement, Placement::Contiguous);
        // A long hole after a loss is still a new run, never padding.
        timeline.note_loss();
        let (placement, start) = feed(&mut timeline, ORIGIN + 4 * BUF_NS + 6 * NS_PER_SEC);
        assert_eq!(placement, Placement::NewRun);
        assert_eq!(start.unwrap().gap_before_ms, 6_000);
    }

    #[test]
    fn pause_and_resume_produce_a_gap_not_silence() {
        let mut timeline = Timeline::new(ORIGIN);
        for i in 0..50 {
            feed(&mut timeline, ORIGIN + i * BUF_NS);
        }
        timeline.end_run();
        assert!(!timeline.in_run());
        // Resumed 30 s later.
        let resume = ORIGIN + 500 * NS_PER_MS + 30 * NS_PER_SEC;
        let (placement, start) = feed(&mut timeline, resume);
        assert_eq!(placement, Placement::NewRun);
        let start = start.unwrap();
        assert_eq!(start.gap_before_ms, 30_000);
        assert_eq!(start.start_ms, 30_500);
    }

    #[test]
    fn a_new_run_never_starts_before_the_previous_one_ended() {
        let mut timeline = Timeline::new(ORIGIN);
        for i in 0..10 {
            feed(&mut timeline, ORIGIN + i * BUF_NS);
        }
        timeline.end_run();
        // A timestamp 30 ms before the end of the last run.
        let start = timeline.start_run(ORIGIN + 70 * NS_PER_MS);
        assert_eq!(start.anchor_host_ns, ORIGIN + 100 * NS_PER_MS);
        assert_eq!(start.gap_before_ms, 0);
    }

    #[test]
    fn rotation_re_anchors_to_the_observed_clock() {
        // The device runs 50 ppm slow against the host clock: every 10 ms
        // buffer arrives 500 ns later than the sample count says.
        let mut timeline = Timeline::new(ORIGIN);
        let skew_ns = 500;
        let buffers = 6_100; // 61 s
        for i in 0..buffers {
            let (placement, _) = feed(&mut timeline, ORIGIN + i * (BUF_NS + skew_ns));
            assert_eq!(placement == Placement::NewRun, i == 0, "drift never re-anchors");
        }
        let start = timeline.rotate(60 * NS_PER_SEC).unwrap();
        let nominal = ORIGIN + 60 * NS_PER_SEC;
        let real = ORIGIN + 6_000 * (BUF_NS + skew_ns);
        assert!(start.anchor_host_ns > nominal, "anchor follows the host clock");
        let error = real.abs_diff(start.anchor_host_ns);
        assert!(error < NS_PER_MS / 2, "anchor error {error} ns");
        assert_eq!(start.gap_before_ms, 0);
        // The rest of the run carries over: the next buffer is contiguous.
        let (placement, _) = feed(&mut timeline, ORIGIN + buffers * (BUF_NS + skew_ns));
        assert_eq!(placement, Placement::Contiguous);
        assert!(timeline.rotate(60 * NS_PER_SEC).is_some());
        timeline.end_run();
        assert!(timeline.rotate(0).is_none());
    }
}
