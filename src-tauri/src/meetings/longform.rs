//! OWNER: WP3 (long-form decoding). Nothing in the scaffold calls into this
//! file yet; WP6's worker is its only caller.
//!
//! A track is transcribed in three steps, each resumable on its own:
//!
//! 1. `plan_windows`: VAD over the track in 5-minute blocks, speech ranges
//!    packed into windows of at most 28 s. Plain data for `transcript_windows`.
//! 2. `track_language`: `nl`, `en`, or one detection on the first 30 s of
//!    speech. The answer goes on every window row, so a resumed job passes it
//!    back in as `known` and never detects twice.
//! 3. `decode_window`: one window in, flagged segments on the meeting
//!    timeline out, or `Preempted` when dictation wanted the model. A
//!    preempted window stays `pending`.
//!
//! The model and the VAD sit behind `WindowDecoder` and `SpeechDetector`, so
//! the worker can test resume with fakes; `WhisperDecoder` and
//! `SileroDetector` are the real ones. Everything else here is pure.
//!
//! Suspect segments are flagged (`types::SuppressedReason`), never deleted;
//! `echo` belongs to `echo.rs`. Meetings do not inherit dictation's filters:
//! no five-word floor, no injection guard, no `sanitize_transcript`.
//!
//! Whisper only: Parakeet has no timestamps, and `WhisperDecoder::load`
//! refuses it.

// Scaffold: remove once WP6 calls into this file.
#![allow(dead_code)]

use crate::local_transcribe::{
    self, DecodeParams, TemperatureFallback, TranscriptSegment, VadTuning,
};
use crate::meetings::types::{MeetingLanguage, SuppressedReason, TrackAudio, TARGET_SAMPLE_RATE};
use crate::model_manager::{self, Engine, ModelId};
use std::sync::Arc;
use std::time::Instant;
use whisper_rs::WhisperContext;

/// Longest window handed to Whisper, padding included. Whisper's own limit is
/// 30 s; staying under it means one window is exactly one encoder pass.
pub const MAX_WINDOW_MS: u64 = 28_000;
/// Silence longer than this ends a window instead of being decoded.
pub const GAP_BREAK_MS: u64 = 3_000;
/// Context added on each side of a window, as far as the neighbours allow.
pub const WINDOW_PAD_MS: u64 = 200;
/// How much of a track goes through VAD at once: bounds memory to ~19 MB.
pub const BLOCK_MS: u64 = 300_000;
/// A speech range ending this close to a block's end was probably cut off by
/// the block border, not by the speaker.
const BLOCK_EDGE_MS: u64 = 200;
/// Lead-in given to VAD before a range carried into the next block.
const CARRY_LEAD_MS: u64 = 300;
/// Speech that language detection listens to.
const DETECT_MS: u64 = 30_000;

/// VAD tuning for meetings. Shorter minimum speech than dictation (a "ja" or
/// "yes" is a whole turn in a conversation), and speech capped at one window.
pub const MEETING_VAD: VadTuning = VadTuning {
    min_speech_ms: 250,
    min_silence_ms: Some(400),
    speech_pad_ms: Some(150),
    max_speech_s: Some(28.0),
};

/// OpenAI's reference fallback schedule, set explicitly so a change of
/// defaults in whisper.cpp cannot silently change transcripts.
pub const MEETING_TEMPERATURE: TemperatureFallback = TemperatureFallback {
    start: 0.0,
    increment: 0.2,
    entropy_threshold: 2.4,
    logprob_threshold: -1.0,
};

const NO_SPEECH_PROB_OVER: f32 = 0.6;
const NO_SPEECH_LOGPROB_UNDER: f32 = -1.0;
/// A segment with less than this share of its duration inside VAD speech is
/// flagged `outside_vad`.
const MIN_VAD_OVERLAP: f64 = 0.2;
/// Identical segments in a row before the run counts as a loop.
const REPEAT_RUN: usize = 3;
const PROMPT_MAX_CHARS: usize = 200;
const PROMPT_TITLE_MAX_CHARS: usize = 120;

// ---------------------------------------------------------------------------
// Window planning
// ---------------------------------------------------------------------------

/// A `transcript_windows` row before it has an id: the unit of decoding and
/// of resume. Times are on the meeting timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedWindow {
    pub seq: u32,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl PlannedWindow {
    pub fn len_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// Finds speech. `SileroDetector` is the real one.
pub trait SpeechDetector {
    /// Speech in `samples` (16 kHz mono) as `(start_ms, end_ms)` relative to
    /// the first sample, in order.
    fn speech_ranges(&mut self, samples: &[f32]) -> Result<Vec<(u64, u64)>, String>;
}

/// Places one block's VAD ranges on the meeting timeline. Returns the ranges
/// that are complete and where the next block starts.
///
/// A range that touches the end of the block is not complete: the block
/// border cut it. It is dropped here and the next block starts just before
/// it, so VAD sees that stretch of speech whole.
pub fn place_block_ranges(
    block_start_ms: u64,
    block_len_ms: u64,
    ranges: &[(u64, u64)],
    is_last_block: bool,
) -> (Vec<(u64, u64)>, u64) {
    let block_end_ms = block_start_ms + block_len_ms;
    let mut placed: Vec<(u64, u64)> = ranges
        .iter()
        .map(|&(start, end)| (block_start_ms + start.min(block_len_ms), block_start_ms + end.min(block_len_ms)))
        .filter(|(start, end)| end > start)
        .collect();
    if is_last_block {
        return (placed, block_end_ms);
    }
    let Some(&(last_start, last_end)) = placed.last() else {
        return (placed, block_end_ms);
    };
    if last_end + BLOCK_EDGE_MS < block_end_ms {
        return (placed, block_end_ms);
    }
    let previous_end = placed.len().checked_sub(2).map_or(block_start_ms, |i| placed[i].1);
    let next_start = last_start.saturating_sub(CARRY_LEAD_MS).max(previous_end);
    // A block that is one long range from its first sample cannot be carried:
    // the next block would be this one again.
    if next_start <= block_start_ms {
        return (placed, block_end_ms);
    }
    placed.pop();
    (placed, next_start)
}

/// Packs speech ranges (meeting-timeline ms) into windows.
///
/// - Ranges closer than `GAP_BREAK_MS` share a window until it would pass
///   `MAX_WINDOW_MS`; a longer gap always starts a new one.
/// - Each window is padded by up to `WINDOW_PAD_MS` per side: never past half
///   the gap to its neighbour (so windows cannot overlap), never outside
///   `[0, track_end_ms]`, never past the 28 s cap.
/// - A single range over the cap is cut into equal parts.
///
/// The windows come out in order, do not overlap, and together cover every
/// input range.
pub fn pack_windows(ranges: &[(u64, u64)], track_end_ms: u64) -> Vec<PlannedWindow> {
    let mut sorted: Vec<(u64, u64)> = ranges.iter().copied().filter(|(s, e)| e > s).collect();
    sorted.sort_unstable();

    // Make the ranges disjoint, then cut the ones that cannot fit a window.
    // Ranges that merely touch stay apart: that is where VAD chose to split.
    let mut pieces: Vec<(u64, u64)> = Vec::with_capacity(sorted.len());
    for (start, end) in sorted {
        let start = match pieces.last_mut() {
            Some(last) if start < last.1 => {
                if end <= last.1 {
                    continue;
                }
                last.1
            }
            _ => start,
        };
        let len = end - start;
        let parts = len.div_ceil(MAX_WINDOW_MS);
        for i in 0..parts {
            pieces.push((start + len * i / parts, start + len * (i + 1) / parts));
        }
    }

    let mut cores: Vec<(u64, u64)> = Vec::new();
    for (start, end) in pieces {
        match cores.last_mut() {
            Some(core) if start - core.1 <= GAP_BREAK_MS && end - core.0 <= MAX_WINDOW_MS => {
                core.1 = end;
            }
            _ => cores.push((start, end)),
        }
    }

    (0..cores.len())
        .map(|i| {
            let (start, end) = cores[i];
            let room_before = match i.checked_sub(1) {
                Some(prev) => (start - cores[prev].1) / 2,
                None => start,
            };
            let room_after = match cores.get(i + 1) {
                Some(next) => (next.0 - end) / 2,
                None => track_end_ms.saturating_sub(end),
            };
            let budget = MAX_WINDOW_MS - (end - start);
            let pad_before = WINDOW_PAD_MS.min(room_before).min(budget / 2);
            let pad_after = WINDOW_PAD_MS.min(room_after).min(budget - pad_before);
            PlannedWindow {
                seq: i as u32,
                start_ms: start - pad_before,
                end_ms: end + pad_after,
            }
        })
        .collect()
}

/// Plans a whole track: VAD in `BLOCK_MS` blocks, then `pack_windows`. Reads
/// each stretch of audio once (plus the carried tail of a block) and holds one
/// block in memory at a time. A track without speech gets no windows.
pub fn plan_windows(
    audio: &mut dyn TrackAudio,
    detector: &mut dyn SpeechDetector,
) -> Result<Vec<PlannedWindow>, String> {
    let track_end_ms = audio.duration_ms();
    let mut ranges = Vec::new();
    let mut block_start_ms = 0;
    while block_start_ms < track_end_ms {
        let block_len_ms = BLOCK_MS.min(track_end_ms - block_start_ms);
        let samples = audio.read(block_start_ms, block_len_ms)?;
        let found = detector.speech_ranges(&samples)?;
        let is_last_block = block_start_ms + block_len_ms >= track_end_ms;
        let (complete, next_start_ms) =
            place_block_ranges(block_start_ms, block_len_ms, &found, is_last_block);
        ranges.extend(complete);
        block_start_ms = next_start_ms;
    }
    Ok(pack_windows(&ranges, track_end_ms))
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// What a decoder made of one window's samples.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeResult {
    /// Every segment, times relative to the first sample.
    Segments(Vec<TranscriptSegment>),
    /// Dictation asked for the model mid-decode. Not an error.
    Preempted,
}

/// The model behind `decode_window`. `WhisperDecoder` is the real one; the
/// worker's tests use a fake.
pub trait WindowDecoder {
    /// `"nl"` or `"en"` for up to 30 s of speech. One encoder pass.
    fn detect_language(&mut self, samples: &[f32]) -> Result<&'static str, String>;
    /// Decodes 16 kHz mono `samples` in `language` (`"nl"` or `"en"`).
    fn decode(
        &mut self,
        samples: &[f32],
        language: &str,
        prompt: Option<&str>,
    ) -> Result<DecodeResult, String>;
}

/// A `transcript_segments` row before it has an id. Times are on the meeting
/// timeline and inside the window.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSegment {
    /// Order within the window.
    pub seq: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub lang: String,
    pub no_speech_prob: f32,
    pub avg_logprob: f32,
    pub suppressed_reason: Option<SuppressedReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WindowOutcome {
    /// Decoded; possibly empty. The window is `done`.
    Done(Vec<WindowSegment>),
    /// The window stays `pending`: decode it again once dictation is done.
    Preempted,
}

/// Maps a time relative to a window's first sample onto the meeting timeline.
/// Linear, one to one. Whisper sometimes reports times past the end of short
/// audio, so the result is held inside the window.
pub fn map_to_timeline(window: &PlannedWindow, relative_ms: u64) -> u64 {
    window.start_ms + relative_ms.min(window.len_ms())
}

/// Decodes one window of a track and flags what looks wrong.
///
/// The caller holds the inference gate (`inference_gate::acquire_background`)
/// for the duration of this call and releases it afterwards, also on
/// `Preempted`. `language` comes from `track_language`.
pub fn decode_window(
    audio: &mut dyn TrackAudio,
    window: &PlannedWindow,
    language: &str,
    prompt: Option<&str>,
    detector: &mut dyn SpeechDetector,
    decoder: &mut dyn WindowDecoder,
) -> Result<WindowOutcome, String> {
    let samples = audio.read(window.start_ms, window.len_ms())?;
    if samples.is_empty() {
        return Ok(WindowOutcome::Done(Vec::new()));
    }
    // Planning found speech here, but its ranges are not stored. Running VAD
    // on the window again is cheap and is what makes `outside_vad` resumable.
    // Whisper runs whatever it says: a disagreement hides text, never loses it.
    let speech: Vec<(u64, u64)> = detector
        .speech_ranges(&samples)?
        .into_iter()
        .map(|(start, end)| (map_to_timeline(window, start), map_to_timeline(window, end)))
        .collect();
    let decoded = match decoder.decode(&samples, language, prompt)? {
        DecodeResult::Preempted => return Ok(WindowOutcome::Preempted),
        DecodeResult::Segments(segments) => segments,
    };

    let mut segments: Vec<WindowSegment> = Vec::with_capacity(decoded.len());
    let mut floor_ms = window.start_ms;
    for seg in decoded {
        let text = seg.text.trim();
        // Nothing was said, so there is nothing to flag or keep.
        if text.is_empty() {
            continue;
        }
        // Keep segments in order even if the decoder's timestamps are not.
        let start_ms = map_to_timeline(window, seg.start_ms).max(floor_ms);
        let end_ms = map_to_timeline(window, seg.end_ms).max(start_ms);
        floor_ms = start_ms;
        segments.push(WindowSegment {
            seq: segments.len() as u32,
            start_ms,
            end_ms,
            text: text.to_string(),
            lang: if seg.lang.is_empty() { language.to_string() } else { seg.lang },
            no_speech_prob: seg.no_speech_prob,
            avg_logprob: seg.avg_logprob,
            suppressed_reason: None,
        });
    }
    flag_segments(&mut segments, &speech, prompt);
    Ok(WindowOutcome::Done(segments))
}

// ---------------------------------------------------------------------------
// Language
// ---------------------------------------------------------------------------

/// The language to decode a track in.
///
/// `known` is the language already stored on one of the track's windows, if
/// any: a resumed job reuses it instead of detecting again. Otherwise `Auto`
/// detects once, on the first 30 s of the planned windows. Detection is one
/// encoder pass, so call this under the inference gate like a window.
pub fn track_language(
    requested: MeetingLanguage,
    known: Option<&str>,
    audio: &mut dyn TrackAudio,
    windows: &[PlannedWindow],
    decoder: &mut dyn WindowDecoder,
) -> Result<&'static str, String> {
    match requested {
        MeetingLanguage::Nl => return Ok("nl"),
        MeetingLanguage::En => return Ok("en"),
        MeetingLanguage::Auto => {}
    }
    match known {
        Some("nl") => return Ok("nl"),
        Some("en") => return Ok("en"),
        _ => {}
    }
    let samples = detection_audio(audio, windows)?;
    if samples.is_empty() {
        // No windows, nothing to decode: the answer is never used.
        return Ok("en");
    }
    decoder.detect_language(&samples)
}

/// The first `DETECT_MS` of a track's windows, which is where the speech is.
fn detection_audio(
    audio: &mut dyn TrackAudio,
    windows: &[PlannedWindow],
) -> Result<Vec<f32>, String> {
    let mut samples = Vec::new();
    let mut wanted_ms = DETECT_MS;
    for window in windows {
        if wanted_ms == 0 {
            break;
        }
        let take_ms = window.len_ms().min(wanted_ms);
        samples.extend(audio.read(window.start_ms, take_ms)?);
        wanted_ms -= take_ms;
    }
    Ok(samples)
}

// ---------------------------------------------------------------------------
// Flagging
// ---------------------------------------------------------------------------

/// Lowercased words with punctuation stripped: what "the same text" means.
fn normalized_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// The decoder itself doubts there was speech *and* is unsure of the words.
/// Either one alone is normal for quiet or accented speech.
pub fn is_no_speech(no_speech_prob: f32, avg_logprob: f32) -> bool {
    no_speech_prob > NO_SPEECH_PROB_OVER && avg_logprob < NO_SPEECH_LOGPROB_UNDER
}

/// Under 20% of the segment lies inside VAD speech: text from silence. A
/// segment without duration counts as inside when VAD speech surrounds it.
pub fn is_outside_vad(start_ms: u64, end_ms: u64, speech: &[(u64, u64)]) -> bool {
    if end_ms <= start_ms {
        return !speech.iter().any(|&(s, e)| s <= start_ms && start_ms <= e);
    }
    let overlap: u64 = speech
        .iter()
        .map(|&(s, e)| e.min(end_ms).saturating_sub(s.max(start_ms)))
        .sum();
    (overlap as f64) < MIN_VAD_OVERLAP * (end_ms - start_ms) as f64
}

/// The same n-gram three or more times back to back, covering at least eight
/// words: "no no no" is a person, "thank you thank you thank you thank you"
/// is a decoder loop.
pub fn has_ngram_loop(text: &str) -> bool {
    const MIN_REPEATS: usize = 3;
    const MIN_WORDS: usize = 8;
    let words = normalized_words(text);
    for n in 1..=words.len() / MIN_REPEATS {
        for start in 0..words.len() - n {
            let gram = &words[start..start + n];
            let repeats = words[start..]
                .chunks_exact(n)
                .take_while(|chunk| *chunk == gram)
                .count();
            if repeats >= MIN_REPEATS && repeats * n >= MIN_WORDS {
                return true;
            }
        }
    }
    false
}

/// The text is made of the prompt's words and nothing else, and is at least
/// half as long as the prompt. One name from the prompt said out loud is a
/// person talking; the title and the attendee list read back is the decoder.
pub fn is_prompt_echo(text: &str, prompt: &str) -> bool {
    let prompt_words = normalized_words(prompt);
    let words = normalized_words(text);
    let needed = prompt_words.len().div_ceil(2).max(2);
    words.len() >= needed && words.iter().all(|word| prompt_words.contains(word))
}

/// Sets `suppressed_reason` on the segments of one window. `speech` is VAD
/// speech on the meeting timeline. Never removes or reorders anything, and
/// leaves segments that already carry a reason alone.
///
/// When several rules match, the first of `no_speech`, `outside_vad`,
/// `prompt_echo`, `repeat` wins. Of a run of identical segments the first
/// one stays visible: it may well have been said.
pub fn flag_segments(segments: &mut [WindowSegment], speech: &[(u64, u64)], prompt: Option<&str>) {
    let normalized: Vec<Vec<String>> = segments.iter().map(|s| normalized_words(&s.text)).collect();
    let mut in_repeat_run = vec![false; segments.len()];
    let mut run_start = 0;
    for i in 1..=segments.len() {
        if i < segments.len() && !normalized[i].is_empty() && normalized[i] == normalized[run_start] {
            continue;
        }
        if i - run_start >= REPEAT_RUN {
            in_repeat_run[run_start + 1..i].fill(true);
        }
        run_start = i;
    }

    for (i, seg) in segments.iter_mut().enumerate() {
        if seg.suppressed_reason.is_some() {
            continue;
        }
        seg.suppressed_reason = if is_no_speech(seg.no_speech_prob, seg.avg_logprob) {
            Some(SuppressedReason::NoSpeech)
        } else if is_outside_vad(seg.start_ms, seg.end_ms, speech) {
            Some(SuppressedReason::OutsideVad)
        } else if prompt.is_some_and(|p| is_prompt_echo(&seg.text, p)) {
            Some(SuppressedReason::PromptEcho)
        } else if in_repeat_run[i] || has_ngram_loop(&seg.text) {
            Some(SuppressedReason::Repeat)
        } else {
            None
        };
    }
}

// ---------------------------------------------------------------------------
// Initial prompt
// ---------------------------------------------------------------------------

/// The initial prompt for a meeting: its title and the participants' names,
/// so Whisper spells them the way the user does. About 200 characters at
/// most; names that no longer fit are left out whole. `None` when there is
/// nothing to say. Pass an empty title for a default, generated one.
pub fn build_initial_prompt(title: &str, participants: &[String]) -> Option<String> {
    let title: String = title.trim().chars().take(PROMPT_TITLE_MAX_CHARS).collect();
    let mut prompt = title.trim_end().trim_end_matches(['.', ',', ';', ':']).to_string();
    let mut used = prompt.chars().count();
    let mut names: Vec<&str> = Vec::new();
    for name in participants.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        if names.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
            continue;
        }
        // ". " or ", " before the name, "." after the last one.
        let cost = name.chars().count() + 2;
        if used + cost + 1 > PROMPT_MAX_CHARS {
            break;
        }
        used += cost;
        names.push(name);
    }
    if !names.is_empty() {
        if !prompt.is_empty() {
            prompt.push_str(". ");
        }
        prompt.push_str(&names.join(", "));
    }
    if prompt.is_empty() {
        return None;
    }
    prompt.push('.');
    Some(prompt)
}

// ---------------------------------------------------------------------------
// The real decoder and detector
// ---------------------------------------------------------------------------

/// Silero VAD with the meeting tuning.
pub struct SileroDetector {
    model_path: String,
}

impl SileroDetector {
    /// Fails when the VAD model is not installed. Dictation can do without
    /// it; meetings cannot, because windows are made of its speech ranges.
    pub fn installed() -> Result<Self, String> {
        let path = model_manager::vad_model_path()?;
        if !path.exists() {
            return Err("Meeting transcription needs the voice activity detection model, which is not installed".to_string());
        }
        let model_path = path
            .to_str()
            .ok_or_else(|| "VAD model path is not valid UTF-8".to_string())?
            .to_string();
        Ok(Self { model_path })
    }
}

impl SpeechDetector for SileroDetector {
    fn speech_ranges(&mut self, samples: &[f32]) -> Result<Vec<(u64, u64)>, String> {
        local_transcribe::speech_ranges(&self.model_path, samples, &MEETING_VAD)
    }
}

/// Whisper for meetings. Shares the loaded model with dictation.
pub struct WhisperDecoder {
    ctx: Arc<WhisperContext>,
    multilingual: bool,
    /// `inference_gate::should_preempt`, replaceable in tests.
    abort: fn() -> bool,
}

impl WhisperDecoder {
    /// The entry point of long-form decoding: loads `model_id`, or takes it
    /// from dictation's cache. Meetings v1 decode with Whisper only.
    pub fn load(model_id: &ModelId) -> Result<Self, String> {
        if model_id.engine() != Engine::Whisper {
            return Err(format!(
                "Meeting transcription needs a Whisper model: {} has no timestamps. Choose a Whisper model in Settings → Meetings.",
                model_id.id()
            ));
        }
        let ctx = local_transcribe::get_or_load_context(model_id)?;
        Ok(Self {
            multilingual: ctx.is_multilingual(),
            ctx,
            abort: crate::inference_gate::should_preempt,
        })
    }
}

impl WindowDecoder for WhisperDecoder {
    fn detect_language(&mut self, samples: &[f32]) -> Result<&'static str, String> {
        let (language, confidence) = local_transcribe::detect_language(&self.ctx, samples)?;
        let _ = crate::storage::append_log(
            "INFO",
            &format!("Meetings: detected language {language} ({confidence:.2})"),
        );
        Ok(language)
    }

    fn decode(
        &mut self,
        samples: &[f32],
        language: &str,
        prompt: Option<&str>,
    ) -> Result<DecodeResult, String> {
        // Dictation is already waiting: do not start an encoder pass first.
        if (self.abort)() {
            return Ok(DecodeResult::Preempted);
        }
        let started = Instant::now();
        let params = DecodeParams {
            language: if self.multilingual { language } else { "en" },
            prompt,
            no_context: Some(true),
            temperature: Some(MEETING_TEMPERATURE),
            abort: Some(self.abort),
        };
        let output = local_transcribe::decode_segments(&self.ctx, samples, &params)?;
        let audio_ms = samples.len() as u64 * 1000 / u64::from(TARGET_SAMPLE_RATE);
        let elapsed_ms = started.elapsed().as_millis();
        if output.aborted {
            let _ = crate::storage::append_log(
                "INFO",
                &format!("Meetings: window of {audio_ms}ms preempted by dictation after {elapsed_ms}ms"),
            );
            return Ok(DecodeResult::Preempted);
        }
        let _ = crate::storage::append_log(
            "INFO",
            &format!(
                "Meetings: window of {audio_ms}ms decoded into {} segment(s) in {elapsed_ms}ms",
                output.segments.len()
            ),
        );
        Ok(DecodeResult::Segments(output.segments))
    }
}

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// A whole track in memory. For tests here and in the worker (WP6); WP4
/// provides the real `TrackAudio` over chunk files.
#[cfg(test)]
pub(crate) struct MemoryTrackAudio {
    pub samples: Vec<f32>,
    /// `(start_ms, len_ms)` of every read, to assert what was touched.
    pub reads: Vec<(u64, u64)>,
}

#[cfg(test)]
impl MemoryTrackAudio {
    pub fn new(samples: Vec<f32>) -> Self {
        Self { samples, reads: Vec::new() }
    }

    /// Silence, with a constant 0.5 wherever `speech` (ms ranges) says so.
    pub fn with_speech(duration_ms: u64, speech: &[(u64, u64)]) -> Self {
        let per_ms = TARGET_SAMPLE_RATE as usize / 1000;
        let mut samples = vec![0.0; duration_ms as usize * per_ms];
        for &(start, end) in speech {
            samples[start as usize * per_ms..end as usize * per_ms].fill(0.5);
        }
        Self::new(samples)
    }
}

#[cfg(test)]
impl TrackAudio for MemoryTrackAudio {
    fn duration_ms(&self) -> u64 {
        self.samples.len() as u64 * 1000 / u64::from(TARGET_SAMPLE_RATE)
    }

    fn read(&mut self, start_ms: u64, len_ms: u64) -> Result<Vec<f32>, String> {
        self.reads.push((start_ms, len_ms));
        let per_ms = TARGET_SAMPLE_RATE as usize / 1000;
        let start = (start_ms as usize * per_ms).min(self.samples.len());
        let end = ((start_ms + len_ms) as usize * per_ms).min(self.samples.len());
        Ok(self.samples[start..end].to_vec())
    }
}

/// Calls every millisecond that is not silent speech. Pairs with
/// `MemoryTrackAudio::with_speech`.
#[cfg(test)]
pub(crate) struct LevelDetector;

#[cfg(test)]
impl SpeechDetector for LevelDetector {
    fn speech_ranges(&mut self, samples: &[f32]) -> Result<Vec<(u64, u64)>, String> {
        let per_ms = TARGET_SAMPLE_RATE as usize / 1000;
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for (ms, frame) in samples.chunks(per_ms).enumerate() {
            if frame.iter().all(|s| s.abs() < 0.1) {
                continue;
            }
            let ms = ms as u64;
            match ranges.last_mut() {
                Some(last) if last.1 == ms => last.1 = ms + 1,
                _ => ranges.push((ms, ms + 1)),
            }
        }
        Ok(ranges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_window_invariants(windows: &[PlannedWindow], ranges: &[(u64, u64)], track_end_ms: u64) {
        for (i, w) in windows.iter().enumerate() {
            assert_eq!(w.seq, i as u32);
            assert!(w.end_ms > w.start_ms, "empty window {w:?}");
            assert!(w.len_ms() <= MAX_WINDOW_MS, "window over the cap: {w:?}");
            assert!(w.end_ms <= track_end_ms, "window past the track: {w:?}");
        }
        for pair in windows.windows(2) {
            assert!(pair[0].end_ms <= pair[1].start_ms, "windows overlap: {pair:?}");
        }
        for &(start, end) in ranges {
            let covered: u64 = windows
                .iter()
                .map(|w| w.end_ms.min(end).saturating_sub(w.start_ms.max(start)))
                .sum();
            assert_eq!(covered, end - start, "range {start}..{end} not covered by {windows:?}");
        }
    }

    // -- planner ----------------------------------------------------------

    #[test]
    fn nearby_ranges_share_a_padded_window() {
        let ranges = [(1_000, 4_000), (5_000, 9_000), (11_500, 12_000)];
        let windows = pack_windows(&ranges, 60_000);
        assert_eq!(windows, vec![PlannedWindow { seq: 0, start_ms: 800, end_ms: 12_200 }]);
    }

    #[test]
    fn a_gap_over_three_seconds_breaks_the_window() {
        // 3 000 ms exactly still joins; one more millisecond does not.
        let joined = pack_windows(&[(0, 2_000), (5_000, 6_000)], 60_000);
        assert_eq!(joined.len(), 1);
        let split = pack_windows(&[(0, 2_000), (5_001, 6_000)], 60_000);
        assert_eq!(
            split,
            vec![
                PlannedWindow { seq: 0, start_ms: 0, end_ms: 2_200 },
                PlannedWindow { seq: 1, start_ms: 4_801, end_ms: 6_200 },
            ]
        );
    }

    #[test]
    fn windows_never_pass_28_seconds() {
        // Continuous chatter: 10 s turns, half a second apart, for 5 minutes.
        let ranges: Vec<(u64, u64)> = (0..28).map(|i| (i * 10_500, i * 10_500 + 10_000)).collect();
        let windows = pack_windows(&ranges, 300_000);
        assert_window_invariants(&windows, &ranges, 300_000);
        // Two turns fit (20.5 s), a third would not (31 s).
        assert_eq!(windows.len(), 14);
        assert_eq!(windows[0], PlannedWindow { seq: 0, start_ms: 0, end_ms: 20_700 });
    }

    #[test]
    fn a_full_window_gives_up_padding_to_stay_under_the_cap() {
        let windows = pack_windows(&[(1_000, 28_900)], 60_000);
        assert_eq!(windows, vec![PlannedWindow { seq: 0, start_ms: 950, end_ms: 28_950 }]);
    }

    #[test]
    fn a_range_over_the_cap_is_cut_into_equal_parts() {
        let ranges = [(10_000, 70_000)];
        let windows = pack_windows(&ranges, 100_000);
        assert_window_invariants(&windows, &ranges, 100_000);
        assert_eq!(windows.len(), 3);
        // The cuts are in the middle of speech: no padding there, and the
        // parts meet exactly.
        assert_eq!(windows[0], PlannedWindow { seq: 0, start_ms: 9_800, end_ms: 30_000 });
        assert_eq!(windows[1], PlannedWindow { seq: 1, start_ms: 30_000, end_ms: 50_000 });
        assert_eq!(windows[2], PlannedWindow { seq: 2, start_ms: 50_000, end_ms: 70_200 });
    }

    #[test]
    fn padding_never_overlaps_a_neighbour_or_leaves_the_track() {
        // Windows that cannot merge (cap) but sit 100 ms apart: each gets
        // half the gap. The first starts at 0 and the last ends at the track's
        // end, so there is nothing to pad into.
        let ranges = [(0, 20_000), (20_100, 40_000), (40_000, 59_950)];
        let windows = pack_windows(&ranges, 60_000);
        assert_window_invariants(&windows, &ranges, 60_000);
        assert_eq!(
            windows,
            vec![
                PlannedWindow { seq: 0, start_ms: 0, end_ms: 20_050 },
                PlannedWindow { seq: 1, start_ms: 20_050, end_ms: 40_000 },
                PlannedWindow { seq: 2, start_ms: 40_000, end_ms: 60_000 },
            ]
        );
    }

    #[test]
    fn unsorted_overlapping_and_empty_ranges_are_tolerated() {
        let ranges = [(9_000, 12_000), (1_000, 5_000), (4_000, 6_000), (2_000, 3_000), (7_000, 7_000)];
        let windows = pack_windows(&ranges, 20_000);
        assert_eq!(windows, vec![PlannedWindow { seq: 0, start_ms: 800, end_ms: 12_200 }]);
        assert!(pack_windows(&[], 20_000).is_empty());
    }

    #[test]
    fn packing_holds_its_invariants_on_generated_input() {
        // Deterministic LCG: range and gap lengths from 50 ms to 40 s.
        let mut seed: u64 = 0x5eed;
        let mut next = |max: u64| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            50 + (seed >> 33) % max
        };
        for _ in 0..200 {
            let mut ranges = Vec::new();
            let mut t = next(5_000);
            for _ in 0..next(40) {
                let len = if next(10) > 8 { next(40_000) } else { next(6_000) };
                ranges.push((t, t + len));
                t += len + if next(10) > 6 { next(8_000) } else { next(400) - 50 };
            }
            let track_end_ms = t + next(1_000);
            let windows = pack_windows(&ranges, track_end_ms);
            assert_window_invariants(&windows, &ranges, track_end_ms);
        }
    }

    #[test]
    fn a_range_touching_the_block_end_is_carried_into_the_next_block() {
        // Speech from 298 s to 302 s straddles the 5-minute border.
        let (complete, next) =
            place_block_ranges(0, BLOCK_MS, &[(10_000, 12_000), (298_000, 300_000)], false);
        assert_eq!(complete, vec![(10_000, 12_000)]);
        assert_eq!(next, 297_700);

        // A range that ends clear of the border is complete.
        let (complete, next) = place_block_ranges(0, BLOCK_MS, &[(298_000, 299_700)], false);
        assert_eq!(complete, vec![(298_000, 299_700)]);
        assert_eq!(next, BLOCK_MS);

        // The lead-in never reaches back into speech that is already placed.
        let (complete, next) =
            place_block_ranges(0, BLOCK_MS, &[(290_000, 297_900), (298_000, 300_000)], false);
        assert_eq!(complete, vec![(290_000, 297_900)]);
        assert_eq!(next, 297_900);

        // In the last block the end of the block is the end of the track.
        let (complete, next) = place_block_ranges(300_000, 60_000, &[(58_000, 60_000)], true);
        assert_eq!(complete, vec![(358_000, 360_000)]);
        assert_eq!(next, 360_000);

        // One range filling the whole block cannot be carried: no progress.
        let (complete, next) = place_block_ranges(300_000, BLOCK_MS, &[(0, BLOCK_MS)], false);
        assert_eq!(complete, vec![(300_000, 600_000)]);
        assert_eq!(next, 600_000);
    }

    #[test]
    fn planning_reads_in_blocks_and_keeps_border_speech_in_one_window() {
        let speech = [(5_000, 8_000), (298_000, 302_000), (450_000, 452_000), (598_500, 601_000)];
        let mut audio = MemoryTrackAudio::with_speech(640_000, &speech);
        let windows = plan_windows(&mut audio, &mut LevelDetector).unwrap();
        assert_window_invariants(&windows, &speech, 640_000);
        assert_eq!(
            windows,
            vec![
                PlannedWindow { seq: 0, start_ms: 4_800, end_ms: 8_200 },
                PlannedWindow { seq: 1, start_ms: 297_800, end_ms: 302_200 },
                PlannedWindow { seq: 2, start_ms: 449_800, end_ms: 452_200 },
                PlannedWindow { seq: 3, start_ms: 598_300, end_ms: 601_200 },
            ]
        );
        // Block two starts where the carried speech starts, less the lead-in.
        // It ends clear of any speech, so block three starts where it ended.
        assert_eq!(
            audio.reads,
            vec![(0, 300_000), (297_700, 300_000), (597_700, 42_300)]
        );
    }

    #[test]
    fn a_silent_or_empty_track_has_no_windows() {
        let mut audio = MemoryTrackAudio::with_speech(10_000, &[]);
        assert!(plan_windows(&mut audio, &mut LevelDetector).unwrap().is_empty());
        let mut audio = MemoryTrackAudio::new(Vec::new());
        assert!(plan_windows(&mut audio, &mut LevelDetector).unwrap().is_empty());
        assert!(audio.reads.is_empty());
    }

    // -- decoding ----------------------------------------------------------

    fn raw(start_ms: u64, end_ms: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start_ms,
            end_ms,
            text: text.to_string(),
            no_speech_prob: 0.01,
            avg_logprob: -0.2,
            lang: "nl".to_string(),
        }
    }

    /// Returns a script instead of decoding, and records what it was asked.
    struct ScriptedDecoder {
        result: DecodeResult,
        detected: &'static str,
        decodes: Vec<(usize, String, Option<String>)>,
        detections: Vec<usize>,
    }

    impl ScriptedDecoder {
        fn new(result: DecodeResult) -> Self {
            Self { result, detected: "nl", decodes: Vec::new(), detections: Vec::new() }
        }
    }

    impl WindowDecoder for ScriptedDecoder {
        fn detect_language(&mut self, samples: &[f32]) -> Result<&'static str, String> {
            self.detections.push(samples.len());
            Ok(self.detected)
        }

        fn decode(
            &mut self,
            samples: &[f32],
            language: &str,
            prompt: Option<&str>,
        ) -> Result<DecodeResult, String> {
            self.decodes.push((samples.len(), language.to_string(), prompt.map(str::to_owned)));
            Ok(self.result.clone())
        }
    }

    #[test]
    fn segment_times_map_linearly_onto_the_meeting_timeline() {
        let window = PlannedWindow { seq: 7, start_ms: 600_000, end_ms: 610_000 };
        assert_eq!(map_to_timeline(&window, 0), 600_000);
        assert_eq!(map_to_timeline(&window, 1), 600_001);
        assert_eq!(map_to_timeline(&window, 4_320), 604_320);
        assert_eq!(map_to_timeline(&window, 10_000), 610_000);
        // Whisper pads short audio to 30 s and can report times out there.
        assert_eq!(map_to_timeline(&window, 29_980), 610_000);
    }

    #[test]
    fn a_window_decodes_into_ordered_in_window_segments() {
        let window = PlannedWindow { seq: 0, start_ms: 600_000, end_ms: 610_000 };
        let mut audio = MemoryTrackAudio::with_speech(700_000, &[(600_200, 609_800)]);
        let mut decoder = ScriptedDecoder::new(DecodeResult::Segments(vec![
            raw(0, 2_500, " Goedemorgen allemaal."),
            raw(2_500, 2_500, "   "),
            raw(2_500, 6_000, " Zullen we beginnen?"),
            // Starts before its predecessor and ends past the audio.
            raw(2_000, 29_000, " Ja."),
        ]));
        let outcome =
            decode_window(&mut audio, &window, "nl", Some("Standup."), &mut LevelDetector, &mut decoder)
                .unwrap();
        let WindowOutcome::Done(segments) = outcome else { panic!("expected segments") };

        assert_eq!(audio.reads, vec![(600_000, 10_000)]);
        assert_eq!(decoder.decodes, vec![(160_000, "nl".to_string(), Some("Standup.".to_string()))]);
        let got: Vec<(u32, u64, u64, &str)> =
            segments.iter().map(|s| (s.seq, s.start_ms, s.end_ms, s.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (0, 600_000, 602_500, "Goedemorgen allemaal."),
                (1, 602_500, 606_000, "Zullen we beginnen?"),
                (2, 602_500, 610_000, "Ja."),
            ]
        );
        assert!(segments.iter().all(|s| s.suppressed_reason.is_none() && s.lang == "nl"));
    }

    #[test]
    fn a_preempted_decode_is_an_outcome_not_an_error() {
        let window = PlannedWindow { seq: 0, start_ms: 0, end_ms: 5_000 };
        let mut audio = MemoryTrackAudio::with_speech(5_000, &[(500, 4_500)]);
        let mut decoder = ScriptedDecoder::new(DecodeResult::Preempted);
        let outcome =
            decode_window(&mut audio, &window, "en", None, &mut LevelDetector, &mut decoder).unwrap();
        assert_eq!(outcome, WindowOutcome::Preempted);
    }

    #[test]
    fn text_decoded_from_the_silent_part_of_a_window_is_flagged() {
        let window = PlannedWindow { seq: 0, start_ms: 10_000, end_ms: 20_000 };
        let mut audio = MemoryTrackAudio::with_speech(30_000, &[(10_200, 13_000), (15_500, 19_800)]);
        let mut decoder = ScriptedDecoder::new(DecodeResult::Segments(vec![
            raw(200, 3_000, " We ship on Friday."),
            raw(3_100, 5_400, " Thanks for watching!"),
            raw(5_500, 9_800, " Any objections?"),
        ]));
        let outcome =
            decode_window(&mut audio, &window, "en", None, &mut LevelDetector, &mut decoder).unwrap();
        let WindowOutcome::Done(segments) = outcome else { panic!("expected segments") };
        let reasons: Vec<_> = segments.iter().map(|s| s.suppressed_reason).collect();
        assert_eq!(reasons, vec![None, Some(SuppressedReason::OutsideVad), None]);
    }

    #[test]
    fn a_window_past_the_end_of_the_audio_is_done_and_empty() {
        let window = PlannedWindow { seq: 0, start_ms: 50_000, end_ms: 60_000 };
        let mut audio = MemoryTrackAudio::with_speech(5_000, &[]);
        let mut decoder = ScriptedDecoder::new(DecodeResult::Preempted);
        let outcome =
            decode_window(&mut audio, &window, "en", None, &mut LevelDetector, &mut decoder).unwrap();
        assert_eq!(outcome, WindowOutcome::Done(Vec::new()));
        assert!(decoder.decodes.is_empty());
    }

    // -- language -----------------------------------------------------------

    #[test]
    fn a_set_or_known_language_never_touches_audio_or_model() {
        let windows = [PlannedWindow { seq: 0, start_ms: 0, end_ms: 5_000 }];
        let mut audio = MemoryTrackAudio::with_speech(5_000, &[(0, 5_000)]);
        let mut decoder = ScriptedDecoder::new(DecodeResult::Preempted);
        for (requested, known, expected) in [
            (MeetingLanguage::Nl, None, "nl"),
            (MeetingLanguage::En, Some("nl"), "en"),
            (MeetingLanguage::Auto, Some("nl"), "nl"),
            (MeetingLanguage::Auto, Some("en"), "en"),
        ] {
            let got = track_language(requested, known, &mut audio, &windows, &mut decoder).unwrap();
            assert_eq!(got, expected);
        }
        assert!(audio.reads.is_empty());
        assert!(decoder.detections.is_empty());
    }

    #[test]
    fn auto_detects_once_on_the_first_thirty_seconds_of_windows() {
        let windows = [
            PlannedWindow { seq: 0, start_ms: 10_000, end_ms: 22_000 },
            PlannedWindow { seq: 1, start_ms: 40_000, end_ms: 60_000 },
            PlannedWindow { seq: 2, start_ms: 90_000, end_ms: 100_000 },
        ];
        let mut audio = MemoryTrackAudio::with_speech(120_000, &[]);
        let mut decoder = ScriptedDecoder::new(DecodeResult::Preempted);
        decoder.detected = "en";
        // A language stored by some other version is not trusted.
        let got =
            track_language(MeetingLanguage::Auto, Some("af"), &mut audio, &windows, &mut decoder).unwrap();
        assert_eq!(got, "en");
        assert_eq!(audio.reads, vec![(10_000, 12_000), (40_000, 18_000)]);
        assert_eq!(decoder.detections, vec![30 * 16_000]);
        assert!(decoder.decodes.is_empty(), "detection must not decode");
    }

    #[test]
    fn auto_on_a_track_without_windows_does_not_run_the_model() {
        let mut audio = MemoryTrackAudio::with_speech(5_000, &[]);
        let mut decoder = ScriptedDecoder::new(DecodeResult::Preempted);
        track_language(MeetingLanguage::Auto, None, &mut audio, &[], &mut decoder).unwrap();
        assert!(decoder.detections.is_empty());
    }

    // -- flagging -------------------------------------------------------------

    fn seg(start_ms: u64, end_ms: u64, text: &str) -> WindowSegment {
        WindowSegment {
            seq: 0,
            start_ms,
            end_ms,
            text: text.to_string(),
            lang: "en".to_string(),
            no_speech_prob: 0.01,
            avg_logprob: -0.2,
            suppressed_reason: None,
        }
    }

    #[test]
    fn no_speech_needs_both_a_high_probability_and_a_low_logprob() {
        assert!(is_no_speech(0.61, -1.01));
        assert!(is_no_speech(0.95, -2.5));
        // Near-misses: only one of the two, or exactly on a threshold.
        assert!(!is_no_speech(0.95, -0.4));
        assert!(!is_no_speech(0.2, -1.8));
        assert!(!is_no_speech(0.6, -1.5));
        assert!(!is_no_speech(0.9, -1.0));
    }

    #[test]
    fn outside_vad_means_under_a_fifth_inside_speech() {
        let speech = [(1_000, 3_000), (8_000, 9_000)];
        assert!(is_outside_vad(4_000, 6_000, &speech));
        assert!(is_outside_vad(4_000, 6_000, &[]));
        // 190 ms of 1 000 ms inside: flagged. 200 ms: not.
        assert!(is_outside_vad(2_810, 3_810, &speech));
        assert!(!is_outside_vad(2_800, 3_800, &speech));
        // Overlap adds up across ranges: 500 + 1 000 of 6 500 ms.
        assert!(!is_outside_vad(2_500, 9_000, &speech));
        assert!(!is_outside_vad(1_200, 2_800, &speech));
        // No duration: inside speech or not.
        assert!(!is_outside_vad(2_000, 2_000, &speech));
        assert!(is_outside_vad(5_000, 5_000, &speech));
    }

    #[test]
    fn ngram_loops_are_found_and_emphasis_is_not() {
        assert!(has_ngram_loop("Thank you. Thank you. Thank you. Thank you."));
        assert!(has_ngram_loop("so we need to we need to we need to we need to go"));
        assert!(has_ngram_loop("and then I said I'll be right back I'll be right back I'll be right back"));
        assert!(has_ngram_loop("ja ja ja ja ja ja ja ja"));
        // Near-misses: too few repeats, too few words, or not back to back.
        assert!(!has_ngram_loop("No, no, no."));
        assert!(!has_ngram_loop("Thank you. Thank you. Thank you."));
        assert!(!has_ngram_loop("I'll be right back, I'll be right back."));
        assert!(!has_ngram_loop("we need to plan, we need to build, we need to ship, we need to rest"));
        assert!(!has_ngram_loop("ja ja ja ja ja ja ja"));
        assert!(!has_ngram_loop(""));
    }

    #[test]
    fn three_identical_segments_in_a_row_are_a_repeat() {
        let speech = [(0, 60_000)];
        let mut segments = vec![
            seg(0, 1_000, "Okay."),
            seg(1_000, 2_000, "Thanks for watching!"),
            seg(2_000, 3_000, "thanks for watching"),
            seg(3_000, 4_000, " Thanks, for watching. "),
            seg(4_000, 5_000, "Thanks for watching!"),
            seg(5_000, 6_000, "Right."),
        ];
        flag_segments(&mut segments, &speech, None);
        let reasons: Vec<_> = segments.iter().map(|s| s.suppressed_reason).collect();
        let repeat = Some(SuppressedReason::Repeat);
        // The first of the run stays: someone may really have said it.
        assert_eq!(reasons, vec![None, None, repeat, repeat, repeat, None]);

        // Near-misses: two in a row, and three that are not in a row.
        let mut segments = vec![
            seg(0, 1_000, "Yes."),
            seg(1_000, 2_000, "Yes."),
            seg(2_000, 3_000, "No."),
            seg(3_000, 4_000, "Yes."),
            // Punctuation only: nothing to compare.
            seg(4_000, 5_000, "..."),
            seg(5_000, 6_000, "..."),
            seg(6_000, 7_000, "..."),
        ];
        flag_segments(&mut segments, &speech, None);
        assert!(segments.iter().all(|s| s.suppressed_reason.is_none()));
    }

    #[test]
    fn prompt_echo_is_only_the_prompt_read_back() {
        let prompt = "Roadmap review. Anna de Vries, Piet Jansen, Joost.";
        assert!(is_prompt_echo("Roadmap review. Anna de Vries, Piet Jansen, Joost.", prompt));
        assert!(is_prompt_echo(" anna de vries piet jansen joost", prompt));
        assert!(is_prompt_echo("Roadmap review, Roadmap review, Anna.", prompt));
        // Near-misses: a name said out loud, a short mention, a real sentence.
        assert!(!is_prompt_echo("Anna?", prompt));
        assert!(!is_prompt_echo("Piet Jansen.", prompt));
        assert!(!is_prompt_echo("Anna de Vries and Piet Jansen will review the roadmap.", prompt));
        assert!(!is_prompt_echo("", prompt));
        assert!(!is_prompt_echo("Hello there.", ""));
    }

    #[test]
    fn flagging_picks_one_reason_and_never_removes_a_segment() {
        let prompt = "Roadmap review. Anna, Piet.";
        let speech = [(0, 10_000)];
        let mut doubted = seg(1_000, 2_000, "Roadmap review. Anna, Piet.");
        doubted.no_speech_prob = 0.9;
        doubted.avg_logprob = -1.4;
        let mut already = seg(3_000, 4_000, "Fine.");
        already.suppressed_reason = Some(SuppressedReason::Echo);
        let mut segments = vec![
            doubted,
            seg(2_000, 3_000, "Roadmap review. Anna, Piet."),
            already,
            seg(20_000, 21_000, "Roadmap review. Anna, Piet."),
            seg(5_000, 9_000, "Let's start with the roadmap."),
        ];
        flag_segments(&mut segments, &speech, Some(prompt));
        let reasons: Vec<_> = segments.iter().map(|s| s.suppressed_reason).collect();
        assert_eq!(
            reasons,
            vec![
                Some(SuppressedReason::NoSpeech),
                Some(SuppressedReason::PromptEcho),
                Some(SuppressedReason::Echo),
                Some(SuppressedReason::OutsideVad),
                None,
            ]
        );
        assert_eq!(segments[4].text, "Let's start with the roadmap.");
    }

    // -- prompt ---------------------------------------------------------------

    #[test]
    fn the_prompt_is_title_plus_names_within_the_cap() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            build_initial_prompt(" Roadmap review. ", &names(&["Anna de Vries", " ", "Piet", "anna de vries"])),
            Some("Roadmap review. Anna de Vries, Piet.".to_string())
        );
        assert_eq!(build_initial_prompt("Standup", &[]), Some("Standup.".to_string()));
        assert_eq!(build_initial_prompt("", &names(&["Anna", "Piet"])), Some("Anna, Piet.".to_string()));
        assert_eq!(build_initial_prompt("  ", &names(&[" "])), None);

        let many: Vec<String> = (0..60).map(|i| format!("Deelnemer Nummer{i}")).collect();
        let prompt = build_initial_prompt(&"Kwartaalplanning ".repeat(20), &many).unwrap();
        assert!(prompt.chars().count() <= PROMPT_MAX_CHARS, "{} chars", prompt.chars().count());
        assert!(prompt.chars().count() > 150);
        // Names are dropped whole, never cut.
        assert!(prompt.ends_with("Deelnemer Nummer3."), "{prompt}");
        // Multi-byte titles are cut on a character, not a byte.
        let prompt = build_initial_prompt(&"é".repeat(500), &[]).unwrap();
        assert_eq!(prompt.chars().count(), PROMPT_TITLE_MAX_CHARS + 1);
    }

    // -- model ----------------------------------------------------------------

    #[test]
    fn parakeet_is_refused_with_a_clear_error() {
        let err = WhisperDecoder::load(&ModelId::ParakeetV3).err().expect("must refuse Parakeet");
        assert!(err.contains("Whisper model"), "{err}");
    }

    /// Speech with pauses, from macOS `say` unless FT_SAMPLE_WAV names a
    /// 16 kHz mono WAV to use instead.
    fn speech_fixture() -> Vec<f32> {
        let sample = std::env::var("FT_SAMPLE_WAV").unwrap_or_default();
        if !sample.is_empty() {
            return local_transcribe::load_wav_as_mono_16k(std::path::Path::new(&sample)).unwrap();
        }
        let dir = std::env::temp_dir().join("flowing_thoughts_longform_check");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("say.wav");
        let status = std::process::Command::new("say")
            .args(["-o", wav.to_str().unwrap(), "--data-format=LEI16@16000"])
            .arg(
                "Good morning everyone, let's get started with the weekly planning. \
                 [[slnc 5000]] First on the agenda is the release schedule for next month. \
                 [[slnc 1200]] Does anyone have concerns about the current timeline?",
            )
            .status()
            .expect("macOS `say` is needed to synthesize the fixture");
        assert!(status.success());
        local_transcribe::load_wav_as_mono_16k(&wav).unwrap()
    }

    #[test]
    #[ignore = "needs the 574 MB large-v3-turbo model + VAD model installed; run with --ignored"]
    fn a_wav_decodes_into_ordered_in_window_timestamped_segments() {
        // Thirty seconds of silence first, so timeline and window times differ.
        let mut samples = vec![0.0; 30 * TARGET_SAMPLE_RATE as usize];
        samples.extend(speech_fixture());
        let mut audio = MemoryTrackAudio::new(samples);
        let mut detector = SileroDetector::installed().expect("VAD model installed");
        let mut decoder =
            WhisperDecoder::load(&ModelId::LargeV3TurboQ5).expect("large-v3-turbo model installed");

        let windows = plan_windows(&mut audio, &mut detector).unwrap();
        println!("  windows: {windows:?}");
        assert!(!windows.is_empty(), "VAD found no speech in the fixture");
        assert!(windows[0].start_ms >= 29_000, "the leading silence must not be planned");
        assert_window_invariants(&windows, &[], audio.duration_ms());

        let language =
            track_language(MeetingLanguage::Auto, None, &mut audio, &windows, &mut decoder).unwrap();
        println!("  language: {language}");
        if std::env::var("FT_SAMPLE_WAV").unwrap_or_default().is_empty() {
            assert_eq!(language, "en");
        }

        let prompt = build_initial_prompt("Weekly planning", &["Anna".to_string()]);
        let mut total = 0;
        for window in &windows {
            let outcome = decode_window(
                &mut audio,
                window,
                language,
                prompt.as_deref(),
                &mut detector,
                &mut decoder,
            )
            .unwrap();
            let WindowOutcome::Done(segments) = outcome else { panic!("nothing preempts a test") };
            let mut previous_start = window.start_ms;
            for s in &segments {
                println!(
                    "  [{:>6}..{:>6}] {:?} {:?} (no_speech {:.2}, logprob {:.2})",
                    s.start_ms, s.end_ms, s.suppressed_reason, s.text, s.no_speech_prob, s.avg_logprob
                );
                assert!(s.start_ms >= previous_start, "segments out of order");
                assert!(s.start_ms >= window.start_ms && s.end_ms <= window.end_ms, "segment outside its window");
                assert!(s.end_ms >= s.start_ms);
                assert!(!s.text.is_empty() && s.text == s.text.trim());
                assert_eq!(s.lang, language);
                assert!(s.avg_logprob < 0.0, "avg_logprob was not filled in");
                previous_start = s.start_ms;
            }
            total += segments.iter().filter(|s| s.suppressed_reason.is_none()).count();
        }
        assert!(total > 0, "nothing was transcribed");
    }

    #[test]
    #[ignore = "needs the 574 MB large-v3-turbo model + VAD model installed; run with --ignored"]
    fn a_raised_preempt_flag_aborts_the_decode() {
        use std::sync::atomic::{AtomicU32, Ordering};
        // Lets the early check pass, then aborts from inside whisper.cpp.
        static POLLS: AtomicU32 = AtomicU32::new(0);
        fn abort_on_second_poll() -> bool {
            POLLS.fetch_add(1, Ordering::Relaxed) >= 1
        }

        let samples = speech_fixture();
        let mut decoder =
            WhisperDecoder::load(&ModelId::LargeV3TurboQ5).expect("large-v3-turbo model installed");
        decoder.abort = abort_on_second_poll;
        let result = decoder.decode(&samples[..samples.len().min(28 * 16_000)], "en", None).unwrap();
        assert_eq!(result, DecodeResult::Preempted);
        assert!(POLLS.load(Ordering::Relaxed) >= 2, "whisper.cpp never polled the abort callback");

        decoder.abort = || true;
        let result = decoder.decode(&samples[..16_000], "en", None).unwrap();
        assert_eq!(result, DecodeResult::Preempted);
    }
}
