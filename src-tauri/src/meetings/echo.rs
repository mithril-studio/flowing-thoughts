//! OWNER: WP9 (echo). Flags mic segments that are the speakers bleeding into
//! the microphone. Real echo cancellation is deferred; v1 works on text.
//!
//! On speakers the remote voices are transcribed twice: on the system track
//! ("Them") and again, quieter and noisier, on the mic track ("Me"). Once both
//! tracks of a run are transcribed the worker (WP6) calls `flag_run_echoes`,
//! which hides the mic copies with `suppressed_reason = 'echo'`.
//!
//! - `find_echo_segments` is the pure rule: a mic segment is an echo when the
//!   system speech within ±`ECHO_TOLERANCE_MS` says closely the same thing.
//!   Both texts are normalized (lowercase, diacritics folded, punctuation
//!   stripped) and compared token by token: the longest common subsequence
//!   with the best-matching stretch of system speech, as a share of the mic
//!   segment's tokens.
//! - Bias towards keeping: losing something the user really said is worse
//!   than showing a duplicate. One- and two-word segments are never flagged
//!   ("ja" from both sides is a reply, not an echo), double-talk only when the
//!   bleed dominates the segment, and common words scattered through a long
//!   stretch of system speech do not add up to a match.
//! - Flags only: nothing is deleted, the system track is never touched and a
//!   segment that already carries a reason keeps it.
//! - `output_has_echo_risk` is the pure decision behind `meetings.echo_risk`;
//!   the Core Audio reads belong to `capture::device_watch` (WP5).

use rusqlite::Connection;

use super::store;
use super::types::{Segment, SuppressedReason, TrackKind};

/// How far outside a mic segment system speech still counts as simultaneous.
/// Covers the echo lag plus Whisper's loose segment timestamps on both tracks.
/// Awaits calibration from the capture spike's bleed test (measured echo lag).
pub const ECHO_TOLERANCE_MS: u64 = 1_500;

/// Share of a mic segment's tokens that must reappear, in order, in the
/// system speech for the segment to be flagged. Awaits calibration from the
/// capture spike's bleed test (how garbled bleed transcribes at real levels).
pub const ECHO_SIMILARITY_THRESHOLD: f32 = 0.6;

/// Mic segments with fewer tokens are never flagged: a back-channel reply
/// ("ja", "ok", "yes", "thank you") looks exactly like its echo.
pub const ECHO_MIN_TOKENS: usize = 3;

/// The stretch of system speech a mic segment is compared with is at most
/// this many times the mic segment's length in tokens. Bleed is one
/// contiguous stretch of what the speakers played, with room for words the
/// mic missed; without the limit the function words of a short reply would
/// all be found somewhere in half a minute of system speech. Awaits
/// calibration along with the threshold.
const ECHO_SPAN_FACTOR: f32 = 1.5;

/// The part of a segment echo detection looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoCandidate {
    pub segment_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Ids of the `mic` segments to flag as `SuppressedReason::Echo`, in input
/// order. Both slices are in timeline order, though nothing here relies on it.
pub fn find_echo_segments(mic: &[EchoCandidate], system: &[EchoCandidate]) -> Vec<String> {
    let system_texts: Vec<String> = system.iter().map(|s| normalize_text(&s.text)).collect();

    mic.iter()
        .filter(|candidate| {
            let mic_text = normalize_text(&candidate.text);
            let mic_tokens: Vec<&str> = mic_text.split_whitespace().collect();
            if mic_tokens.len() < ECHO_MIN_TOKENS {
                return false;
            }
            let mut nearby: Vec<(&EchoCandidate, &String)> = system
                .iter()
                .zip(&system_texts)
                .filter(|(s, _)| within_tolerance(candidate, s))
                .collect();
            nearby.sort_by_key(|(s, _)| (s.start_ms, s.end_ms));
            let system_tokens: Vec<&str> = nearby
                .iter()
                .flat_map(|(_, text)| text.split_whitespace())
                .collect();
            echo_similarity(&mic_tokens, &system_tokens) >= ECHO_SIMILARITY_THRESHOLD
        })
        .map(|candidate| candidate.segment_id.clone())
        .collect()
}

/// The system segment overlaps the mic segment once the mic segment is
/// widened by the tolerance on both sides. The edges are inclusive.
fn within_tolerance(mic: &EchoCandidate, system: &EchoCandidate) -> bool {
    system.end_ms.saturating_add(ECHO_TOLERANCE_MS) >= mic.start_ms
        && system.start_ms <= mic.end_ms.saturating_add(ECHO_TOLERANCE_MS)
}

/// Lowercase, diacritics folded, apostrophes dropped, every other
/// non-alphanumeric character a word break, whitespace collapsed. Whisper is
/// not consistent between two decodes of the same words ("één" / "een",
/// "zo'n" / "zo’n", "Oké." / "oke"), and bleed makes it worse.
pub fn normalize_text(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            // Combining marks: the decomposed form of a diacritic.
            '\u{0300}'..='\u{036F}' => {}
            // Inside a word in both languages ("don't", "zo'n", "'s"), and
            // Whisper alternates between the straight and the curly one.
            '\'' | '’' | '‘' | 'ʼ' | '`' | '´' => {}
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => folded.push('a'),
            'ç' => folded.push('c'),
            'è' | 'é' | 'ê' | 'ë' => folded.push('e'),
            'ì' | 'í' | 'î' | 'ï' => folded.push('i'),
            'ñ' => folded.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => folded.push('o'),
            'ù' | 'ú' | 'û' | 'ü' => folded.push('u'),
            'ý' | 'ÿ' => folded.push('y'),
            'æ' => folded.push_str("ae"),
            'œ' => folded.push_str("oe"),
            'ß' => folded.push_str("ss"),
            'ĳ' => folded.push_str("ij"),
            c if c.is_alphanumeric() => folded.push(c),
            _ => folded.push(' '),
        }
    }
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Share of the mic tokens found, in order, in the best-matching stretch of
/// the system tokens: 1.0 is a verbatim copy, 0.0 nothing in common. The
/// ratio is over the mic tokens only, so a mic segment that is a fragment of
/// a longer system sentence still scores 1.0, and double-talk scores the
/// share of the segment that is bleed.
fn echo_similarity(mic: &[&str], system: &[&str]) -> f32 {
    if mic.is_empty() || system.is_empty() {
        return 0.0;
    }
    let span = ((mic.len() as f32 * ECHO_SPAN_FACTOR).ceil() as usize).min(system.len());
    let best = system
        .windows(span)
        .map(|stretch| lcs_len(mic, stretch))
        .max()
        .unwrap_or(0);
    best as f32 / mic.len() as f32
}

/// Length of the longest common subsequence, two rolling rows.
fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    let mut row = vec![0usize; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            row[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(row[j])
            };
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[b.len()]
}

// --- Applying the flags ------------------------------------------------------

/// What one pass over a run found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EchoOutcome {
    /// Mic segments that were looked at: the ones without a reason yet.
    pub examined: usize,
    /// Of those, how many are now `echo`. Zero on a repeated pass.
    pub flagged: usize,
}

/// The worker's entry point, once both tracks of `run_id` are transcribed:
/// flags the run's echoes and logs the counts (never text). The worker calls
/// it inside the transaction that settles the run; emitting
/// `meeting-updated` stays with the worker.
pub fn flag_run_echoes(
    conn: &Connection,
    meeting_id: &str,
    run_id: &str,
) -> Result<EchoOutcome, String> {
    let outcome = apply_echo_flags(conn, meeting_id, run_id)?;
    super::recording::log(
        "INFO",
        &format!(
            "Meetings: echo check on run {run_id}: {} of {} mic segments flagged",
            outcome.flagged, outcome.examined
        ),
    );
    Ok(outcome)
}

/// Loads the run's segments, runs `find_echo_segments` and writes
/// `suppressed_reason = 'echo'` on the mic segments it returns.
///
/// - Only segments of `run_id` are read or written, so a re-transcribed run
///   is judged on its own segments and the old run keeps its flags.
/// - The decoded text is compared, not the user's edit of it.
/// - A segment that already carries a reason is left alone: it cannot be
///   flagged, and on the system side it is not evidence of real speech
///   either (`no_speech`, `repeat` and friends are probably hallucinated).
/// - Idempotent: segments are immutable, so a second pass finds the same
///   echoes already flagged and changes nothing.
fn apply_echo_flags(
    conn: &Connection,
    meeting_id: &str,
    run_id: &str,
) -> Result<EchoOutcome, String> {
    let segments = store::list_segments(conn, meeting_id, Some(run_id))?;
    let candidates = |kind: TrackKind| -> Vec<EchoCandidate> {
        segments
            .iter()
            .filter(|seg| seg.track_kind == kind && seg.suppressed_reason.is_none())
            .map(to_candidate)
            .collect()
    };
    let mic = candidates(TrackKind::Mic);
    let system = candidates(TrackKind::System);

    let echoes = find_echo_segments(&mic, &system);
    let flagged = if echoes.is_empty() {
        0
    } else {
        store::flag_segments(conn, &echoes, SuppressedReason::Echo)?
    };
    Ok(EchoOutcome {
        examined: mic.len(),
        flagged,
    })
}

fn to_candidate(segment: &Segment) -> EchoCandidate {
    EchoCandidate {
        segment_id: segment.id.clone(),
        start_ms: segment.start_ms,
        end_ms: segment.end_ms,
        // `text` is the displayed text; `original_text` is the decoded text
        // when the user edited it.
        text: segment
            .original_text
            .clone()
            .unwrap_or_else(|| segment.text.clone()),
    }
}

// --- Echo risk ---------------------------------------------------------------

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// `kAudioDeviceTransportTypeBuiltIn`.
pub const TRANSPORT_BUILT_IN: u32 = four_cc(b"bltn");
/// `kAudioDevicePropertyDataSource` of the built-in output with headphones in
/// the jack. The internal speakers are `'ispk'`.
pub const DATA_SOURCE_HEADPHONES: u32 = four_cc(b"hdpn");

/// Whether a meeting played through this output device will bleed into the
/// mic: true for the built-in speakers. The session (WP7) stores it as
/// `meetings.echo_risk` and shows "Use headphones for best results".
///
/// `transport_type` is the default output device's
/// `kAudioDevicePropertyTransportType`; `data_source` is its
/// `kAudioDevicePropertyDataSource` on the output scope, `None` when the
/// device has none. The built-in device covers both the speakers and the
/// headphone jack, and only the data source tells them apart; a built-in
/// output that does not say is taken for speakers, because a needless hint
/// costs less than a missing one.
pub fn output_has_echo_risk(transport_type: u32, data_source: Option<u32>) -> bool {
    transport_type == TRANSPORT_BUILT_IN && data_source != Some(DATA_SOURCE_HEADPHONES)
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::super::store::{NewMeeting, NewRun, NewSegment, NewTrack, NewWindow};
    use super::super::types::MeetingLanguage;
    use super::*;

    fn seg(id: &str, start_ms: u64, end_ms: u64, text: &str) -> EchoCandidate {
        EchoCandidate {
            segment_id: id.to_string(),
            start_ms,
            end_ms,
            text: text.to_string(),
        }
    }

    fn flagged(mic: &[EchoCandidate], system: &[EchoCandidate]) -> Vec<String> {
        find_echo_segments(mic, system)
    }

    // --- The pure rule ---------------------------------------------------------

    #[test]
    fn exact_duplicate_is_flagged() {
        let system = [seg("s1", 10_000, 14_000, "We should move the release to next Friday.")];
        let mic = [seg("m1", 10_200, 14_300, "We should move the release to next Friday.")];
        assert_eq!(flagged(&mic, &system), vec!["m1"]);
    }

    #[test]
    fn whisper_variants_of_the_same_sentence_are_flagged() {
        // Bleed is quiet: words drop out, casing and punctuation differ, a
        // word is misheard.
        let system = [seg(
            "s1",
            10_000,
            16_000,
            "So, I think we're going to need two more weeks for the migration, honestly.",
        )];
        let mic = [seg(
            "m1",
            10_400,
            15_800,
            "so i think we are going to need two more weeks for the immigration",
        )];
        assert_eq!(flagged(&mic, &system), vec!["m1"]);

        let system = [seg(
            "s2",
            20_000,
            25_000,
            "Volgende week hebben we de oplevering, dus ik wil vandaag de planning rondmaken.",
        )];
        let mic = [seg(
            "m2",
            20_300,
            24_900,
            "volgende week hebben we oplevering dus ik wil de planning rond maken",
        )];
        assert_eq!(flagged(&mic, &system), vec!["m2"]);
    }

    #[test]
    fn echo_split_differently_than_the_system_track_is_flagged() {
        // One long system segment, bleed cut into two mic segments; and the
        // other way round.
        let system = [seg(
            "s1",
            0,
            9_000,
            "The numbers for the third quarter look better than expected, mostly because of the new pricing.",
        )];
        let mic = [
            seg("m1", 200, 4_500, "The numbers for the third quarter look better"),
            seg("m2", 4_600, 9_100, "than expected mostly because of the new pricing"),
        ];
        assert_eq!(flagged(&mic, &system), vec!["m1", "m2"]);

        let system = [
            seg("s1", 0, 4_000, "The numbers for the third quarter"),
            seg("s2", 4_000, 9_000, "look better than expected."),
        ];
        let mic = [seg(
            "m1",
            300,
            9_200,
            "the numbers for the third quarter look better than expected",
        )];
        assert_eq!(flagged(&mic, &system), vec!["m1"]);
    }

    #[test]
    fn unrelated_simultaneous_speech_is_not_flagged() {
        let system = [seg("s1", 10_000, 15_000, "Then we can look at the budget for the next quarter.")];
        let mic = [seg("m1", 10_500, 14_000, "Sorry, can you hear me? My connection is bad.")];
        assert!(flagged(&mic, &system).is_empty());
    }

    #[test]
    fn short_back_channel_replies_are_never_flagged() {
        let system = [
            seg("s1", 1_000, 1_500, "Ja."),
            seg("s2", 5_000, 5_600, "Ok."),
            seg("s3", 9_000, 9_800, "Yes, exactly."),
            seg("s4", 12_000, 12_900, "Thank you."),
        ];
        let mic = [
            seg("m1", 1_100, 1_600, "Ja."),
            seg("m2", 5_100, 5_700, "Ok!"),
            seg("m3", 9_100, 9_900, "Yes, exactly."),
            seg("m4", 12_100, 13_000, "Thank you."),
            seg("m5", 15_000, 15_500, "..."),
        ];
        assert!(flagged(&mic, &system).is_empty());
    }

    #[test]
    fn common_words_scattered_through_long_system_speech_do_not_add_up() {
        // Every word of the reply occurs, in order, somewhere in the system
        // segment; no stretch of it says the same thing.
        let system = [seg(
            "s1",
            0,
            20_000,
            "Ik heb gisteren met de klant gebeld en ja die vonden de demo erg sterk, maar dat \
             neemt niet weg dat er nog veel werk ligt voordat het echt af is en iedereen het \
             goed genoeg vindt.",
        )];
        let mic = [seg("m1", 18_000, 19_500, "Ja, dat is goed.")];
        assert!(flagged(&mic, &system).is_empty());
    }

    #[test]
    fn double_talk_is_not_flagged() {
        // The user talks over the speakers: own words plus some bleed.
        let system = [seg("s1", 10_000, 15_000, "and then we roll it out to all the customers in March")];
        let mic = [seg(
            "m1",
            10_500,
            15_500,
            "Wait, before you go on, I have a question about that, to all the customers",
        )];
        assert!(flagged(&mic, &system).is_empty());
    }

    #[test]
    fn double_talk_is_flagged_only_when_the_bleed_clears_the_threshold() {
        let system = [seg("s1", 10_000, 15_000, "and then we roll it out to all the customers in March")];
        // 12 of 14 tokens are bleed.
        let mic = [seg(
            "m1",
            10_200,
            15_300,
            "and then we roll it out to all the customers in March, yeah okay",
        )];
        assert_eq!(flagged(&mic, &system), vec!["m1"]);
    }

    #[test]
    fn similarity_threshold_is_inclusive() {
        let system = ["alpha", "bravo", "charlie", "delta", "echo"];
        // 3 of 5 mic tokens: exactly the threshold.
        let at = ["alpha", "bravo", "charlie", "xray", "yankee"];
        assert!(echo_similarity(&at, &system) >= ECHO_SIMILARITY_THRESHOLD);
        // 2 of 5: below.
        let below = ["alpha", "bravo", "xray", "yankee", "zulu"];
        assert!(echo_similarity(&below, &system) < ECHO_SIMILARITY_THRESHOLD);
        // Order matters: the same words backwards are not the same sentence.
        let reversed = ["echo", "delta", "charlie", "bravo", "alpha"];
        assert!(echo_similarity(&reversed, &system) < ECHO_SIMILARITY_THRESHOLD);

        assert_eq!(echo_similarity(&system, &system), 1.0);
        assert_eq!(echo_similarity(&[], &system), 0.0);
        assert_eq!(echo_similarity(&system, &[]), 0.0);
    }

    #[test]
    fn tolerance_window_edges_are_inclusive() {
        let text = "please send me the updated contract today";
        let mic = [seg("m1", 10_000, 12_000, text)];

        // System speech ending exactly the tolerance before the mic segment.
        let before = [seg("s1", 6_000, 10_000 - ECHO_TOLERANCE_MS, text)];
        assert_eq!(flagged(&mic, &before), vec!["m1"]);
        let too_early = [seg("s1", 6_000, 10_000 - ECHO_TOLERANCE_MS - 1, text)];
        assert!(flagged(&mic, &too_early).is_empty());

        // System speech starting exactly the tolerance after it.
        let after = [seg("s1", 12_000 + ECHO_TOLERANCE_MS, 16_000, text)];
        assert_eq!(flagged(&mic, &after), vec!["m1"]);
        let too_late = [seg("s1", 12_000 + ECHO_TOLERANCE_MS + 1, 16_000, text)];
        assert!(flagged(&mic, &too_late).is_empty());

        // A mic segment at the very start of the meeting does not underflow.
        let first = [seg("m0", 0, 900, text)];
        assert_eq!(flagged(&first, &[seg("s0", 100, 1_000, text)]), vec!["m0"]);
    }

    #[test]
    fn segments_outside_the_window_are_ignored() {
        // The same sentence a minute apart is a quote, not an echo; and the
        // matching half that lies outside the window does not count.
        let system = [
            seg("s1", 0, 4_000, "the launch date is the first of June"),
            seg("s2", 60_000, 64_000, "we also need a new logo"),
        ];
        let mic = [seg("m1", 61_000, 65_000, "the launch date is the first of June")];
        assert!(flagged(&mic, &system).is_empty());
        assert!(flagged(&mic, &[]).is_empty());
        assert!(flagged(&[], &system).is_empty());
    }

    #[test]
    fn only_mic_ids_are_returned_in_mic_order() {
        let system = [
            seg("s1", 0, 3_000, "first we look at the roadmap"),
            seg("s2", 10_000, 13_000, "then we talk about hiring plans"),
        ];
        let mic = [
            seg("m1", 100, 3_100, "first we look at the roadmap"),
            seg("m2", 5_000, 8_000, "I have a dentist appointment at four"),
            seg("m3", 10_100, 13_100, "then we talk about hiring plans"),
        ];
        assert_eq!(flagged(&mic, &system), vec!["m1", "m3"]);
    }

    // --- Normalization -----------------------------------------------------------

    #[test]
    fn dutch_text_with_diacritics_and_punctuation_normalizes() {
        assert_eq!(
            normalize_text("  Eén ding: coördinatie van de ideeën, vóór 's ochtends!  "),
            "een ding coordinatie van de ideeen voor s ochtends"
        );
        assert_eq!(normalize_text("Zo’n café… oké?"), normalize_text("zo'n cafe oke"));
        assert_eq!(normalize_text("Het is 10.30 uur — privé-afspraak."), "het is 10 30 uur prive afspraak");
        // Decomposed diacritics (e + combining acute) fold the same way.
        assert_eq!(normalize_text("cafe\u{0301}"), "cafe");
        assert_eq!(normalize_text("ĲSSEL"), "ijssel");
    }

    #[test]
    fn english_text_normalizes() {
        assert_eq!(
            normalize_text("Well — it's \"done\", isn't it?\n(I think.)"),
            "well its done isnt it i think"
        );
        assert_eq!(normalize_text("It’s"), normalize_text("its"));
        assert_eq!(normalize_text(" ... "), "");
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn diacritics_and_punctuation_do_not_hide_an_echo() {
        let system = [seg("s1", 0, 5_000, "Eén ding nog: de coördinatie van het café-overleg, oké?")];
        let mic = [seg("m1", 300, 5_200, "een ding nog de coordinatie van het cafe overleg oke")];
        assert_eq!(flagged(&mic, &system), vec!["m1"]);
    }

    #[test]
    fn lcs_is_a_subsequence_length() {
        assert_eq!(lcs_len(&["a", "b", "c", "d"], &["a", "x", "c", "d", "y"]), 3);
        assert_eq!(lcs_len(&["a", "b"], &["b", "a"]), 1);
        assert_eq!(lcs_len(&["a"], &[]), 0);
    }

    // --- Applying the flags --------------------------------------------------------

    /// Same approach as the store's tests: the real schema, built from the
    /// migration batches in `db.rs`.
    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let source = include_str!("../db.rs");
        let mut rest = &source[..source.find("#[cfg(test)]").expect("db.rs test module")];
        while let Some(start) = rest.find("\"BEGIN;") {
            let batch = &rest[start + 1..];
            let end = batch.find("COMMIT;\"").expect("end of migration batch") + "COMMIT;".len();
            conn.execute_batch(&batch[..end]).expect("run migration batch");
            rest = &batch[end..];
        }
        conn
    }

    struct Fixture {
        meeting_id: String,
        mic: String,
        system: String,
    }

    fn fixture(conn: &Connection) -> Fixture {
        let meeting_id = store::insert_meeting(
            conn,
            &NewMeeting {
                title: "Standup".to_string(),
                language: MeetingLanguage::Auto,
                model: Some("whisper-small-q5".to_string()),
                origin_host_ns: Some(1_000),
                calendar_event_id: None,
            },
        )
        .unwrap();
        let track = |kind: TrackKind| {
            store::insert_track(
                conn,
                &NewTrack {
                    meeting_id: meeting_id.clone(),
                    kind,
                    device_name: None,
                    format: None,
                },
            )
            .unwrap()
        };
        let mic = track(TrackKind::Mic);
        let system = track(TrackKind::System);
        store::seed_track_speakers(conn, &meeting_id).unwrap();
        Fixture { meeting_id, mic, system }
    }

    fn new_run(conn: &Connection, meeting_id: &str) -> String {
        store::insert_run(
            conn,
            &NewRun {
                meeting_id: meeting_id.to_string(),
                model: "whisper-small-q5".to_string(),
                language: MeetingLanguage::En,
                params_json: None,
            },
        )
        .unwrap()
    }

    fn new_segment(start_ms: u64, text: &str, reason: Option<SuppressedReason>) -> NewSegment {
        NewSegment {
            start_ms,
            end_ms: start_ms + 4_000,
            text: text.to_string(),
            lang: Some("en".to_string()),
            no_speech_prob: Some(0.01),
            avg_logprob: Some(-0.2),
            suppressed_reason: reason,
        }
    }

    /// One window on `track_id` holding `segments`. Returns the segment ids.
    fn decode(conn: &Connection, run_id: &str, track_id: &str, segments: &[NewSegment]) -> Vec<String> {
        let window = NewWindow {
            track_id: track_id.to_string(),
            seq: 0,
            start_ms: 0,
            end_ms: 28_000,
        };
        let window_ids = store::insert_windows(conn, run_id, &[window]).unwrap();
        store::complete_window(conn, &window_ids[0], Some("en"), segments).unwrap()
    }

    /// `(id, suppressed_reason)` of every segment of the run.
    fn reasons(conn: &Connection, f: &Fixture, run_id: &str) -> Vec<(String, Option<SuppressedReason>)> {
        store::list_segments(conn, &f.meeting_id, Some(run_id))
            .unwrap()
            .into_iter()
            .map(|s| (s.id, s.suppressed_reason))
            .collect()
    }

    fn reason_of(conn: &Connection, segment_id: &str) -> Option<SuppressedReason> {
        store::get_segment(conn, segment_id).unwrap().unwrap().suppressed_reason
    }

    const ROADMAP: &str = "First we look at the roadmap for the next quarter.";
    const HIRING: &str = "Then we talk about the hiring plans for the design team.";

    #[test]
    fn flags_the_mic_echoes_and_nothing_else() {
        let conn = memory_db();
        let f = fixture(&conn);
        let run_id = new_run(&conn, &f.meeting_id);
        let system = decode(
            &conn,
            &run_id,
            &f.system,
            &[new_segment(0, ROADMAP, None), new_segment(10_000, HIRING, None)],
        );
        let mic = decode(
            &conn,
            &run_id,
            &f.mic,
            &[
                new_segment(200, "first we look at the roadmap for the next quarter", None),
                new_segment(5_000, "Sounds good, I have two questions about that.", None),
            ],
        );

        let outcome = apply_echo_flags(&conn, &f.meeting_id, &run_id).unwrap();
        assert_eq!(outcome, EchoOutcome { examined: 2, flagged: 1 });

        assert_eq!(reason_of(&conn, &mic[0]), Some(SuppressedReason::Echo));
        assert_eq!(reason_of(&conn, &mic[1]), None);
        // The system track is never touched, and nothing is deleted.
        assert_eq!(reason_of(&conn, &system[0]), None);
        assert_eq!(reason_of(&conn, &system[1]), None);
        assert_eq!(reasons(&conn, &f, &run_id).len(), 4);
        // Flagged means hidden by default, behind the "show N hidden" toggle.
        assert!(store::get_segment(&conn, &mic[0]).unwrap().unwrap().hidden);
    }

    #[test]
    fn a_second_pass_changes_nothing() {
        let conn = memory_db();
        let f = fixture(&conn);
        let run_id = new_run(&conn, &f.meeting_id);
        decode(&conn, &run_id, &f.system, &[new_segment(0, ROADMAP, None)]);
        decode(
            &conn,
            &run_id,
            &f.mic,
            &[new_segment(100, ROADMAP, None), new_segment(6_000, "I can take that one.", None)],
        );

        let first = apply_echo_flags(&conn, &f.meeting_id, &run_id).unwrap();
        assert_eq!(first.flagged, 1);
        let after_first = reasons(&conn, &f, &run_id);

        let second = apply_echo_flags(&conn, &f.meeting_id, &run_id).unwrap();
        assert_eq!(second.flagged, 0);
        assert_eq!(reasons(&conn, &f, &run_id), after_first);
    }

    #[test]
    fn an_existing_reason_is_preserved() {
        let conn = memory_db();
        let f = fixture(&conn);
        let run_id = new_run(&conn, &f.meeting_id);
        decode(&conn, &run_id, &f.system, &[new_segment(0, ROADMAP, None)]);
        // A textbook echo, but long-form decoding already called it a repeat.
        let mic = decode(
            &conn,
            &run_id,
            &f.mic,
            &[new_segment(100, ROADMAP, Some(SuppressedReason::Repeat))],
        );

        let outcome = apply_echo_flags(&conn, &f.meeting_id, &run_id).unwrap();
        assert_eq!(outcome, EchoOutcome { examined: 0, flagged: 0 });
        assert_eq!(reason_of(&conn, &mic[0]), Some(SuppressedReason::Repeat));
    }

    #[test]
    fn a_suppressed_system_segment_is_not_evidence() {
        let conn = memory_db();
        let f = fixture(&conn);
        let run_id = new_run(&conn, &f.meeting_id);
        // Hallucinated over silence on the system track; the user really said it.
        decode(
            &conn,
            &run_id,
            &f.system,
            &[new_segment(0, ROADMAP, Some(SuppressedReason::NoSpeech))],
        );
        let mic = decode(&conn, &run_id, &f.mic, &[new_segment(100, ROADMAP, None)]);

        let outcome = apply_echo_flags(&conn, &f.meeting_id, &run_id).unwrap();
        assert_eq!(outcome.flagged, 0);
        assert_eq!(reason_of(&conn, &mic[0]), None);
    }

    #[test]
    fn a_retranscribed_run_is_judged_on_its_own_segments() {
        let conn = memory_db();
        let f = fixture(&conn);

        let first_run = new_run(&conn, &f.meeting_id);
        decode(&conn, &first_run, &f.system, &[new_segment(0, ROADMAP, None)]);
        let first_mic = decode(&conn, &first_run, &f.mic, &[new_segment(100, ROADMAP, None)]);
        apply_echo_flags(&conn, &f.meeting_id, &first_run).unwrap();
        assert_eq!(reason_of(&conn, &first_mic[0]), Some(SuppressedReason::Echo));

        // The second run hears the mic differently: no echo this time. The
        // first run's system segment must not be held against it.
        let second_run = new_run(&conn, &f.meeting_id);
        decode(&conn, &second_run, &f.system, &[new_segment(10_000, HIRING, None)]);
        let second_mic = decode(&conn, &second_run, &f.mic, &[new_segment(100, ROADMAP, None)]);
        let outcome = apply_echo_flags(&conn, &f.meeting_id, &second_run).unwrap();
        assert_eq!(outcome, EchoOutcome { examined: 1, flagged: 0 });
        assert_eq!(reason_of(&conn, &second_mic[0]), None);
        // And the first run keeps its flags.
        assert_eq!(reason_of(&conn, &first_mic[0]), Some(SuppressedReason::Echo));
    }

    #[test]
    fn the_decoded_text_is_compared_not_the_users_edit() {
        let conn = memory_db();
        let f = fixture(&conn);
        let run_id = new_run(&conn, &f.meeting_id);
        decode(&conn, &run_id, &f.system, &[new_segment(0, ROADMAP, None)]);
        let mic = decode(&conn, &run_id, &f.mic, &[new_segment(100, ROADMAP, None)]);
        store::set_segment_text(&conn, &mic[0], Some("Something else entirely, typed by hand.")).unwrap();

        apply_echo_flags(&conn, &f.meeting_id, &run_id).unwrap();
        assert_eq!(reason_of(&conn, &mic[0]), Some(SuppressedReason::Echo));
        // The edit itself is untouched.
        let edits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM segment_edits WHERE segment_id = ?1 AND text IS NOT NULL",
                params![mic[0]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(edits, 1);
    }

    #[test]
    fn a_mic_only_meeting_has_no_echoes() {
        let conn = memory_db();
        let f = fixture(&conn);
        let run_id = new_run(&conn, &f.meeting_id);
        decode(&conn, &run_id, &f.mic, &[new_segment(100, ROADMAP, None)]);
        let outcome = apply_echo_flags(&conn, &f.meeting_id, &run_id).unwrap();
        assert_eq!(outcome, EchoOutcome { examined: 1, flagged: 0 });
    }

    // --- Echo risk -------------------------------------------------------------------

    #[test]
    fn only_the_built_in_speakers_are_an_echo_risk() {
        let internal_speakers = Some(four_cc(b"ispk"));
        assert!(output_has_echo_risk(TRANSPORT_BUILT_IN, internal_speakers));
        // Headphones in the jack of the same built-in device.
        assert!(!output_has_echo_risk(TRANSPORT_BUILT_IN, Some(DATA_SOURCE_HEADPHONES)));
        // A built-in output that does not name its data source: assume speakers.
        assert!(output_has_echo_risk(TRANSPORT_BUILT_IN, None));
        // Bluetooth, USB: headsets as far as we can tell.
        assert!(!output_has_echo_risk(four_cc(b"blue"), None));
        assert!(!output_has_echo_risk(four_cc(b"usb "), internal_speakers));

        assert_eq!(TRANSPORT_BUILT_IN, 0x626C_746E);
        assert_eq!(DATA_SOURCE_HEADPHONES, 0x6864_706E);
    }
}
