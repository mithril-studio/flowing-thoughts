//! OWNER: WP9 (echo). Flags mic segments that are the speakers bleeding into
//! the microphone. Real echo cancellation is deferred.
//!
//! The function below is called by the worker (WP6) once both tracks of a run
//! are transcribed; its signature is fixed so WP6 and WP9 can be built in
//! parallel. It is pure: the worker reads the segments and writes
//! `suppressed_reason = 'echo'` for the ids it gets back.
//!
//! Rule: a mic segment is an echo when a system segment within ±1.5 s says
//! closely the same thing. Text similarity and thresholds come from the
//! spike's measured bleed level and echo lag. Bias towards keeping: losing
//! something the user really said is worse than showing a duplicate.
//!
//! Tests: thresholds on synthetic pairs — identical text inside the window,
//! identical text outside it, near-miss text, both people saying "yes".

// Scaffold: remove once WP9 implements this file.
#![allow(dead_code)]

/// The part of a segment echo detection looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoCandidate {
    pub segment_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Ids of the `mic` segments to flag as `SuppressedReason::Echo`. Both
/// slices are in timeline order.
pub fn find_echo_segments(_mic: &[EchoCandidate], _system: &[EchoCandidate]) -> Vec<String> {
    Vec::new()
}
