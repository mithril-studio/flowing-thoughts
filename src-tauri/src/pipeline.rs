//! The pure, UI-free stages of a dictation: the capture gate, the
//! local-or-cloud routing decision, the vocabulary prompt, and the
//! post-transcription text filters.
//!
//! The live session in `lib.rs` and the eval harness (`eval/`) both call these,
//! so a measured result describes what dictation really does. Change a rule
//! here and the next eval run shows what it cost or bought.

use crate::{corrections, dev_vocab};

/// Recordings shorter than this are treated as accidental hotkey taps and
/// discarded silently — no error toast, no pipeline run.
pub const MIN_DICTATION_MS: u64 = 300;

/// Peak amplitude below which a capture is considered silence and skipped.
/// Whisper reliably hallucinates on silent audio — subtitle credits from its
/// training data ("(C) TV GELDERLAND 2021") and markers like [BLANK_AUDIO].
pub const SILENCE_PEAK_THRESHOLD: f32 = 0.015;

/// True when a capture never reaches transcription: an accidental tap or a
/// silent recording.
pub fn is_capture_discarded(duration_ms: u64, peak_amplitude: f32) -> bool {
    duration_ms < MIN_DICTATION_MS || peak_amplitude < SILENCE_PEAK_THRESHOLD
}

/// Where a capture is sent for transcription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// On-device, with the installed local model (Whisper or Parakeet).
    Local,
    /// The cloud API. Only ever reached by explicit user choice.
    Cloud,
    /// Local mode, but the selected model is not on disk. An error, never a
    /// reason to upload.
    LocalModelMissing,
}

/// Decide the transcription route from the `transcription.provider` setting
/// (`"local"` or `"api"`) and whether the selected local model is installed.
///
/// Audio leaves the machine only when the user explicitly selected the API
/// provider. API keys are deliberately not an input: a configured key can
/// never turn a missing local model into an upload. Any mode other than
/// `"api"` is treated as local, so an unexpected value fails closed.
pub fn choose_route(mode: &str, local_model_installed: bool) -> Route {
    if mode == "api" {
        Route::Cloud
    } else if local_model_installed {
        Route::Local
    } else {
        Route::LocalModelMissing
    }
}

pub const CORRECTION_PROMPT_CHAR_CAP: usize = 800;
pub const CORRECTION_PROMPT_LIMIT: i64 = 40;

/// Whisper `prompt` built from the intended terms the user has taught us, so
/// the decoder biases toward them on ambiguous audio. With the developer
/// dictionary on, the built-in vocabulary fills whatever budget the user's
/// terms leave over.
pub fn build_vocabulary_prompt(user_terms: &[String], developer_dictionary: bool) -> Option<String> {
    if developer_dictionary {
        dev_vocab::build_biased_prompt(user_terms, CORRECTION_PROMPT_CHAR_CAP)
    } else {
        corrections::build_prompt_from_corrections(user_terms, CORRECTION_PROMPT_CHAR_CAP)
    }
}

/// Why a transcript was thrown away instead of injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Nothing real left after stripping markers and known credit lines.
    Filtered,
    /// Whisper continued the biased vocabulary prompt instead of transcribing.
    PromptEcho,
    /// Fewer than `MIN_INJECT_WORDS` real words.
    UnderWordFloor,
}

impl DropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::Filtered => "filtered",
            DropReason::PromptEcho => "prompt echo",
            DropReason::UnderWordFloor => "under word floor",
        }
    }
}

/// Run the raw model output through every "is this speech at all?" filter.
/// `Ok` carries the sanitized transcript; `Err` says which filter dropped it.
pub fn filter_transcript(
    raw: &str,
    user_terms: &[String],
    developer_dictionary: bool,
) -> Result<String, DropReason> {
    // Filter Whisper's silence hallucinations (subtitle credits,
    // [BLANK_AUDIO], *Muziek*).
    let raw = sanitize_transcript(raw);
    if raw.is_empty() {
        return Err(DropReason::Filtered);
    }
    // Whisper also continues the biased vocabulary prompt when there is
    // nothing to transcribe, pasting fragments like "And Linux." into the
    // focused app. That is not speech either.
    if dev_vocab::is_prompt_echo(&raw, user_terms, developer_dictionary) {
        return Err(DropReason::PromptEcho);
    }
    // Blunt length floor. The hallucinations that survive every content-based
    // filter are all short fragments.
    if is_below_word_floor(&raw) {
        return Err(DropReason::UnderWordFloor);
    }
    Ok(raw)
}

/// Turn a transcript that survived `filter_transcript` into the text that is
/// injected. Developer jargon is repaired first, then the user's learned
/// corrections go on top (so a personal correction always wins over the
/// built-in dictionary), all before smart formatting so capitalisation rules
/// run on the final word shape.
pub fn finalize_transcript(
    filtered: String,
    developer_dictionary: bool,
    correction_pairs: &[(String, String)],
    smart_formatting: bool,
) -> String {
    let text = if developer_dictionary {
        dev_vocab::normalize(&filtered)
    } else {
        filtered
    };
    let replaced = corrections::apply_replacements(&text, correction_pairs);
    if smart_formatting {
        apply_smart_formatting(&replaced)
    } else {
        replaced
    }
}

/// Transcripts with fewer real words than this are never injected.
///
/// Whisper's silence hallucinations are overwhelmingly one- to four-word
/// fragments ("And Linux.", "Thank you.", "Bye."), and no decoder-side filter
/// catches them all — the model reports *high* confidence in its own
/// invention, so asking it to grade its own work does not work. A blunt length
/// floor does.
///
/// Measured against the full logged history: 147 of 263 transcripts fall under
/// this floor, and reviewing that entire bucket, not one is a genuine content
/// dictation — it is hallucinations, ambient audio, and setup-day tests.
///
/// The cost is that a deliberate short reply ("yes", "sounds good") is dropped
/// too. It stays recoverable in logs.txt, and the proper fix is VAD upstream so
/// the decoder never sees non-speech in the first place.
pub const MIN_INJECT_WORDS: usize = 5;

pub fn real_word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count()
}

pub fn is_below_word_floor(text: &str) -> bool {
    real_word_count(text) < MIN_INJECT_WORDS
}

/// Whisper hallucinates non-speech markers and subtitle credits from its
/// training data on silent or noisy audio: "[BLANK_AUDIO]", "*Muziek*",
/// "(C) TV GELDERLAND 2021", "Ondertiteld door ...". Strip the bracketed and
/// starred markers and reject short transcripts that are known credit lines.
/// Returns an empty string when nothing real remains — callers treat that as
/// "no speech detected" and skip injection.
pub fn sanitize_transcript(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '[' => {
                for n in chars.by_ref() {
                    if n == ']' {
                        break;
                    }
                }
            }
            '*' => {
                for n in chars.by_ref() {
                    if n == '*' {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    let cleaned = out.trim();
    if !cleaned.chars().any(|c| c.is_alphanumeric()) {
        return String::new();
    }
    // Known hallucinations (subtitle credits, YouTube outros) only ever
    // appear as short standalone outputs; the length cap keeps real
    // dictations that mention these words (e.g. "zet de ondertiteling
    // aan…") from being dropped.
    if cleaned.chars().count() < 80 {
        let lower = cleaned.to_lowercase();
        const HALLUCINATED_PHRASES: [&str; 12] = [
            "tv gelderland",
            "ondertiteld door",
            "ondertiteling",
            "subtitles by the amara",
            "thanks for watching",
            "thank you for watching",
            "subscribe to my channel",
            "like and subscribe",
            "see you in the next video",
            "in the comments below",
            "bedankt voor het kijken",
            "abonneer je op",
        ];
        if HALLUCINATED_PHRASES.iter().any(|h| lower.contains(h)) {
            return String::new();
        }
    }
    cleaned.to_string()
}

pub fn apply_smart_formatting(text: &str) -> String {
    // Preserve all whitespace (spaces, tabs, newlines) — only capitalise the
    // first visible character. Whisper already returns proper punctuation, so
    // we don't force a trailing period.
    let trimmed_start = text.trim_start_matches(|c: char| c.is_whitespace());
    if trimmed_start.is_empty() {
        return text.to_string();
    }
    let leading_ws_len = text.len() - trimmed_start.len();
    let leading_ws = &text[..leading_ws_len];
    let mut chars = trimmed_start.chars();
    let first = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    let rest: String = chars.collect();
    format!("{leading_ws}{first}{rest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_floor_drops_the_short_hallucination_fragments() {
        // Every distinct sub-floor output the app actually pasted.
        for junk in [
            "And Linux.",
            "Thank you.",
            "Bye.",
            "And so on.",
            "And so forth.",
            "You",
            "Framework.",
            "And Reboot.",
            "And Vivo.org.",
            "And Java.org.",
            "Czy cụ Sasha",
            "Basically a single job",
        ] {
            assert!(is_below_word_floor(junk), "{junk:?} should be dropped");
        }
        // Punctuation and emoji are not words.
        assert!(is_below_word_floor("😍😍😍😍"));
        assert!(is_below_word_floor("... ... ..."));
    }

    #[test]
    fn word_floor_keeps_real_dictation() {
        assert!(!is_below_word_floor("let's fix the following things one"));
        assert!(!is_below_word_floor("deploy the API to Vercel now"));
        // Exactly at the floor is kept.
        assert!(!is_below_word_floor("one two three four five"));
        assert!(is_below_word_floor("one two three four"));
    }

    #[test]
    fn sanitize_drops_silence_hallucinations() {
        assert_eq!(sanitize_transcript("[BLANK_AUDIO]"), "");
        assert_eq!(sanitize_transcript("*Muziek*"), "");
        assert_eq!(sanitize_transcript("***"), "");
        assert_eq!(sanitize_transcript("(C) TV GELDERLAND 2021"), "");
        assert_eq!(sanitize_transcript("Ondertiteld door de NOS"), "");
        assert_eq!(sanitize_transcript(" [ Silence ] "), "");
        assert_eq!(sanitize_transcript("Thanks for watching!"), "");
        assert_eq!(sanitize_transcript("Subscribe to my channel!"), "");
        assert_eq!(
            sanitize_transcript(
                "So, if you have any questions, please leave them in the comments below."
            ),
            ""
        );
        assert_eq!(sanitize_transcript("Bedankt voor het kijken!"), "");
    }

    #[test]
    fn sanitize_strips_markers_but_keeps_speech() {
        assert_eq!(
            sanitize_transcript("Hello world [BLANK_AUDIO]"),
            "Hello world"
        );
        assert_eq!(sanitize_transcript("Dit is een test."), "Dit is een test.");
        // Long real dictations mentioning blocklisted words are kept.
        let long = "Zet de ondertiteling aan voor deze video want ik wil hem kunnen volgen tijdens de lunch.";
        assert_eq!(sanitize_transcript(long), long);
    }

    #[test]
    fn smart_formatting_preserves_whitespace_and_capitalises_first_letter() {
        assert_eq!(
            apply_smart_formatting("hello   world"),
            "Hello   world"
        );
        assert_eq!(
            apply_smart_formatting("line one\nline two"),
            "Line one\nline two"
        );
        assert_eq!(apply_smart_formatting("already done?"), "Already done?");
    }

    #[test]
    fn filter_reports_which_rule_dropped_the_transcript() {
        assert_eq!(filter_transcript("[BLANK_AUDIO]", &[], true), Err(DropReason::Filtered));
        assert_eq!(filter_transcript("And Linux.", &[], true), Err(DropReason::PromptEcho));
        assert_eq!(filter_transcript("Ja, dat klopt.", &[], true), Err(DropReason::UnderWordFloor));
        assert_eq!(
            filter_transcript(" Dit is een gewone Nederlandse zin. ", &[], true),
            Ok("Dit is een gewone Nederlandse zin.".to_string())
        );
    }

    #[test]
    fn route_local_mode_uses_the_installed_model() {
        assert_eq!(choose_route("local", true), Route::Local);
    }

    #[test]
    fn route_local_mode_with_missing_model_is_an_error_not_an_upload() {
        // Regression: this used to fall back to the cloud when an API key was
        // configured. Keys are not an input any more, so no key state can
        // change the answer.
        assert_eq!(choose_route("local", false), Route::LocalModelMissing);
    }

    #[test]
    fn route_cloud_only_when_api_is_explicitly_selected() {
        // Cloud mode ignores whatever local model happens to be on disk.
        assert_eq!(choose_route("api", true), Route::Cloud);
        assert_eq!(choose_route("api", false), Route::Cloud);
    }

    #[test]
    fn route_is_never_cloud_unless_mode_is_exactly_api() {
        for mode in ["local", "", "API", "api ", "cloud", "groq", "openai"] {
            for installed in [true, false] {
                assert_ne!(
                    choose_route(mode, installed),
                    Route::Cloud,
                    "mode {mode:?}, installed {installed}"
                );
            }
        }
    }

    #[test]
    fn route_unknown_mode_fails_closed_to_local() {
        assert_eq!(choose_route("", true), Route::Local);
        assert_eq!(choose_route("cloud", false), Route::LocalModelMissing);
    }

    #[test]
    fn capture_gate_matches_the_live_thresholds() {
        assert!(is_capture_discarded(299, 0.5));
        assert!(is_capture_discarded(2_000, 0.014));
        assert!(!is_capture_discarded(300, 0.015));
    }
}
