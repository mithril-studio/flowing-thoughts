//! Runs dataset clips through the real dictation pipeline and tallies results.
//!
//! Each clip goes through exactly what a live dictation goes through — capture
//! gate, resampling, VAD gate, Whisper, then the text filters — via
//! `pipeline.rs` and `local_transcribe.rs`. Output is recorded at two stages:
//! `raw` (what the model said) and `final` (what would have been typed), so a
//! model error is distinguishable from a filter discarding correct speech.
//!
//! Fully offline: nothing in here touches the network.

use super::manifest::{self, Clip, Expected, Split};
use super::score::{self, ClipScore, Hits};
use crate::local_transcribe::{DecodeOptions, LocalTranscript};
use crate::model_manager::ModelId;
use crate::pipeline;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VocabVariant {
    /// No prompt, no dictionary, no learned corrections.
    None,
    /// The user's learned terms and corrections only (read from the app DB).
    Personal,
    /// The built-in developer vocabulary only.
    Developer,
    /// Developer vocabulary plus personal terms — what the app does by default.
    Both,
}

impl VocabVariant {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "personal" => Some(Self::Personal),
            "developer" => Some(Self::Developer),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Personal => "personal",
            Self::Developer => "developer",
            Self::Both => "both",
        }
    }

    fn uses_personal(self) -> bool {
        matches!(self, Self::Personal | Self::Both)
    }

    fn uses_developer(self) -> bool {
        matches!(self, Self::Developer | Self::Both)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resampler {
    /// The app's own linear-interpolation resampler (what dictation uses).
    App,
    /// Pre-convert to 16 kHz with macOS `afconvert` (anti-aliased), bypassing
    /// the app resampler. Measures what the linear resampler costs.
    Afconvert,
}

impl Resampler {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "app" => Some(Self::App),
            "afconvert" => Some(Self::Afconvert),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Afconvert => "afconvert",
        }
    }
}

/// One point on the comparison axes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunConfig {
    pub model: String,
    /// App language mode: "nl", "en" or "system" (auto-detect).
    pub language_mode: String,
    pub vocab: VocabVariant,
    pub use_vad: bool,
    pub resampler: Resampler,
}

impl RunConfig {
    pub fn label(&self) -> String {
        let language = if self.language_mode == "system" { "auto" } else { &self.language_mode };
        let mut label = format!("{} · {language} · vocab={}", self.model, self.vocab.as_str());
        if !self.use_vad {
            label.push_str(" · vad=off");
        }
        if self.resampler != Resampler::App {
            label.push_str(&format!(" · resample={}", self.resampler.as_str()));
        }
        label
    }
}

/// The vocabulary inputs of a run, resolved once.
#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    pub user_terms: Vec<String>,
    pub correction_pairs: Vec<(String, String)>,
    pub developer_dictionary: bool,
}

impl Vocabulary {
    /// Personal variants read the live app database, strictly read-only.
    pub fn resolve(variant: VocabVariant) -> Result<Self, String> {
        let mut vocabulary = Vocabulary {
            developer_dictionary: variant.uses_developer(),
            ..Default::default()
        };
        if variant.uses_personal() {
            if let Some(conn) = crate::db::open_read_only()? {
                vocabulary.user_terms =
                    crate::db::top_mistranscribed_words(&conn, None, pipeline::CORRECTION_PROMPT_LIMIT)?
                        .into_iter()
                        .map(|row| row.intended_text)
                        .collect();
                vocabulary.correction_pairs = crate::db::list_correction_pairs(&conn)?;
            }
        }
        Ok(vocabulary)
    }

    pub fn prompt(&self) -> Option<String> {
        pipeline::build_vocabulary_prompt(&self.user_terms, self.developer_dictionary)
    }
}

/// Why a clip ended with no text, named after the stage that emptied it.
pub mod discard {
    pub const CAPTURE_GATE: &str = "capture_gate";
    pub const VAD: &str = "vad";
    pub const MODEL_EMPTY: &str = "model_empty";
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClipResult {
    pub id: String,
    pub split: Split,
    pub categories: Vec<String>,
    pub condition: String,
    pub expected: Expected,
    pub duration_ms: u64,
    pub latency_ms: u64,
    pub reference: String,
    /// Model output, before any text filter.
    pub raw: String,
    /// What dictation would have typed. Empty when discarded.
    pub final_text: String,
    /// Stage that emptied the output, when it is empty.
    pub discarded_by: Option<String>,
    pub detected_language: Option<String>,
    pub redecoded_as_dutch: bool,
    /// Present for speech clips only.
    pub raw_score: Option<ClipScore>,
    pub final_score: Option<ClipScore>,
}

impl ClipResult {
    fn final_wer(&self) -> f64 {
        self.final_score.as_ref().map(ClipScore::wer).unwrap_or(0.0)
    }
}

pub type TranscribeFn<'a> = dyn Fn(&Path, &DecodeOptions) -> Result<LocalTranscript, String> + 'a;

/// Run one clip through the dictation pipeline.
pub fn run_clip(
    clip: &Clip,
    data_dir: &Path,
    config: &RunConfig,
    vocabulary: &Vocabulary,
    transcribe: &TranscribeFn,
) -> Result<ClipResult, String> {
    let wav = data_dir.join(&clip.audio);
    let stats = manifest::wav_stats(&wav)?;
    let mut result = ClipResult {
        id: clip.id.clone(),
        split: clip.split,
        categories: clip.categories.clone(),
        condition: clip.condition.clone(),
        expected: clip.expected,
        duration_ms: stats.duration_ms,
        latency_ms: 0,
        reference: clip.reference.clone(),
        raw: String::new(),
        final_text: String::new(),
        discarded_by: None,
        detected_language: None,
        redecoded_as_dutch: false,
        raw_score: None,
        final_score: None,
    };

    if pipeline::is_capture_discarded(stats.duration_ms, stats.peak_amplitude) {
        // The live session drops these before any transcription runs.
        result.discarded_by = Some(discard::CAPTURE_GATE.to_string());
    } else {
        let converted = match config.resampler {
            Resampler::App => None,
            Resampler::Afconvert => Some(afconvert_to_16k(&wav)?),
        };
        let options = DecodeOptions {
            language_mode: config.language_mode.clone(),
            prompt: vocabulary.prompt(),
            use_vad: config.use_vad,
        };
        let started = Instant::now();
        let transcript = transcribe(converted.as_deref().unwrap_or(&wav), &options);
        result.latency_ms = started.elapsed().as_millis() as u64;
        if let Some(tmp) = converted {
            let _ = std::fs::remove_file(tmp);
        }
        let transcript = transcript?;
        result.detected_language = transcript.language;
        result.redecoded_as_dutch = transcript.redecoded_as_dutch;
        result.raw = transcript.text;

        if transcript.vad_rejected {
            result.discarded_by = Some(discard::VAD.to_string());
        } else if score::is_blank(&result.raw) {
            result.discarded_by = Some(discard::MODEL_EMPTY.to_string());
        } else {
            match pipeline::filter_transcript(
                &result.raw,
                &vocabulary.user_terms,
                vocabulary.developer_dictionary,
            ) {
                Ok(filtered) => {
                    result.final_text = pipeline::finalize_transcript(
                        filtered,
                        vocabulary.developer_dictionary,
                        &vocabulary.correction_pairs,
                        false,
                    );
                }
                Err(reason) => {
                    result.discarded_by = Some(reason.as_str().replace(' ', "_"));
                }
            }
        }
    }

    if clip.expected == Expected::Speech {
        result.raw_score = Some(score::score_clip(&clip.reference, &result.raw, &clip.entities));
        result.final_score =
            Some(score::score_clip(&clip.reference, &result.final_text, &clip.entities));
    }
    Ok(result)
}

fn afconvert_to_16k(wav: &Path) -> Result<PathBuf, String> {
    let out = std::env::temp_dir().join(format!("ft-eval-16k-{}.wav", uuid::Uuid::new_v4()));
    let status = std::process::Command::new("afconvert")
        .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
        .arg(wav)
        .arg(&out)
        .status()
        .map_err(|e| format!("Failed to run afconvert: {e}"))?;
    if !status.success() {
        return Err(format!("afconvert failed on {}", wav.display()));
    }
    Ok(out)
}

/// Error counts of one output stage, summed over speech clips (corpus-level
/// WER: total errors over total reference words, not a mean of per-clip WERs).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StageTally {
    pub ref_words: usize,
    pub word_errors: usize,
    pub lenient_word_errors: usize,
    pub ref_chars: usize,
    pub char_errors: usize,
    pub names: Hits,
    pub numbers: Hits,
    pub terms: Hits,
}

impl StageTally {
    fn add(&mut self, s: &ClipScore) {
        self.ref_words += s.ref_words;
        self.word_errors += s.word_errors;
        self.lenient_word_errors += s.lenient_word_errors;
        self.ref_chars += s.ref_chars;
        self.char_errors += s.char_errors;
        self.names.add(s.names);
        self.numbers.add(s.numbers);
        self.terms.add(s.terms);
    }

    pub fn wer(&self) -> f64 {
        score::ratio(self.word_errors, self.ref_words)
    }

    pub fn lenient_wer(&self) -> f64 {
        score::ratio(self.lenient_word_errors, self.ref_words)
    }

    pub fn cer(&self) -> f64 {
        score::ratio(self.char_errors, self.ref_chars)
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Tally {
    pub speech_clips: usize,
    pub non_speech_clips: usize,
    pub raw: StageTally,
    #[serde(rename = "final")]
    pub final_stage: StageTally,
    /// Speech clips that ended with no text at all.
    pub speech_discarded: usize,
    /// …broken down by the stage that discarded them.
    pub speech_discarded_by: BTreeMap<String, usize>,
    /// Non-speech clips where the model produced text.
    pub hallucinated_raw: usize,
    /// Non-speech clips where text survived every filter and would be typed.
    pub hallucinated_final: usize,
    pub audio_ms: u64,
    pub latency_ms_total: u64,
    #[serde(skip)]
    latencies: Vec<u64>,
    pub latency_ms_p50: u64,
    pub latency_ms_p95: u64,
}

impl Tally {
    pub fn add(&mut self, r: &ClipResult) {
        match r.expected {
            Expected::Speech => {
                self.speech_clips += 1;
                if let Some(s) = &r.raw_score {
                    self.raw.add(s);
                }
                if let Some(s) = &r.final_score {
                    self.final_stage.add(s);
                }
                if score::is_blank(&r.final_text) {
                    self.speech_discarded += 1;
                    let by = r.discarded_by.clone().unwrap_or_else(|| "unknown".to_string());
                    *self.speech_discarded_by.entry(by).or_default() += 1;
                }
            }
            Expected::NonSpeech => {
                self.non_speech_clips += 1;
                self.hallucinated_raw += usize::from(!score::is_blank(&r.raw));
                self.hallucinated_final += usize::from(!score::is_blank(&r.final_text));
            }
        }
        self.audio_ms += r.duration_ms;
        self.latency_ms_total += r.latency_ms;
        self.latencies.push(r.latency_ms);
        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();
        self.latency_ms_p50 = percentile(&sorted, 50);
        self.latency_ms_p95 = percentile(&sorted, 95);
    }

    pub fn speech_discarded_rate(&self) -> Option<f64> {
        (self.speech_clips > 0).then(|| self.speech_discarded as f64 / self.speech_clips as f64)
    }

    pub fn hallucination_rate_final(&self) -> Option<f64> {
        (self.non_speech_clips > 0)
            .then(|| self.hallucinated_final as f64 / self.non_speech_clips as f64)
    }

    pub fn hallucination_rate_raw(&self) -> Option<f64> {
        (self.non_speech_clips > 0)
            .then(|| self.hallucinated_raw as f64 / self.non_speech_clips as f64)
    }

    /// Processing time over audio time; below 1.0 is faster than real time.
    pub fn real_time_factor(&self) -> Option<f64> {
        (self.audio_ms > 0).then(|| self.latency_ms_total as f64 / self.audio_ms as f64)
    }
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunResult {
    pub config: RunConfig,
    pub label: String,
    pub user_terms: usize,
    pub correction_pairs: usize,
    /// SHA-256 of the Whisper prompt, so two runs can be checked for the same
    /// vocabulary without storing personal terms in the results file.
    pub prompt_sha256: Option<String>,
    pub model_load_ms: u64,
    /// Peak physical footprint of the process so far. Monotonic across a
    /// multi-config run: for a clean per-model number, run one config.
    pub peak_memory_mb: Option<f64>,
    pub overall: Tally,
    pub by_category: BTreeMap<String, Tally>,
    pub by_condition: BTreeMap<String, Tally>,
    pub detected_languages: BTreeMap<String, usize>,
    pub redecoded_as_dutch: usize,
    /// Per-clip detail. Left out for held-out test runs — see `report`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<ClipResult>,
}

/// Run every clip through one configuration.
pub fn run_config(
    clips: &[Clip],
    data_dir: &Path,
    config: &RunConfig,
    transcribe: &TranscribeFn,
    mut progress: impl FnMut(usize, &ClipResult),
) -> Result<RunResult, String> {
    let vocabulary = Vocabulary::resolve(config.vocab)?;
    let prompt = vocabulary.prompt();
    let mut run = RunResult {
        config: config.clone(),
        label: config.label(),
        user_terms: vocabulary.user_terms.len(),
        correction_pairs: vocabulary.correction_pairs.len(),
        prompt_sha256: prompt.as_deref().map(sha256_hex),
        model_load_ms: 0,
        peak_memory_mb: None,
        overall: Tally::default(),
        by_category: BTreeMap::new(),
        by_condition: BTreeMap::new(),
        detected_languages: BTreeMap::new(),
        redecoded_as_dutch: 0,
        clips: Vec::with_capacity(clips.len()),
    };
    for (index, clip) in clips.iter().enumerate() {
        let result = run_clip(clip, data_dir, config, &vocabulary, transcribe)
            .map_err(|e| format!("clip {}: {e}", clip.id))?;
        run.overall.add(&result);
        for category in &result.categories {
            run.by_category.entry(category.clone()).or_default().add(&result);
        }
        run.by_condition.entry(result.condition.clone()).or_default().add(&result);
        if let Some(language) = &result.detected_language {
            *run.detected_languages.entry(language.clone()).or_default() += 1;
        }
        run.redecoded_as_dutch += usize::from(result.redecoded_as_dutch);
        progress(index, &result);
        run.clips.push(result);
    }
    run.peak_memory_mb = peak_memory_bytes().map(|b| b as f64 / (1024.0 * 1024.0));
    Ok(run)
}

/// Transcriber backed by the real local Whisper path.
pub fn local_transcriber(model: ModelId) -> impl Fn(&Path, &DecodeOptions) -> Result<LocalTranscript, String> {
    move |wav, options| crate::local_transcribe::transcribe_wav_blocking(&model, wav, options)
}

/// The clips of `split` that can be scored, plus how many were skipped as
/// unverified. Optional filters narrow the set for quick iterations.
pub fn select_clips(
    clips: Vec<Clip>,
    split: Split,
    category: Option<&str>,
    condition: Option<&str>,
    limit: Option<usize>,
) -> (Vec<Clip>, usize) {
    let in_scope: Vec<Clip> = clips
        .into_iter()
        .filter(|c| c.split == split)
        .filter(|c| category.is_none_or(|cat| c.categories.iter().any(|x| x == cat)))
        .filter(|c| condition.is_none_or(|cond| c.condition == cond))
        .collect();
    let unverified = in_scope.iter().filter(|c| !c.verified).count();
    let mut selected: Vec<Clip> = in_scope.into_iter().filter(|c| c.verified).collect();
    if let Some(limit) = limit {
        selected.truncate(limit);
    }
    (selected, unverified)
}

/// The `n` speech clips with the highest final-stage WER.
pub fn worst_clips(run: &RunResult, n: usize) -> Vec<&ClipResult> {
    let mut speech: Vec<&ClipResult> = run
        .clips
        .iter()
        .filter(|c| c.expected == Expected::Speech && c.final_wer() > 0.0)
        .collect();
    speech.sort_by(|a, b| {
        b.final_wer()
            .partial_cmp(&a.final_wer())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    speech.truncate(n);
    speech
}

pub fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Peak physical memory footprint of this process (includes Metal buffers on
/// Apple Silicon, which plain RSS misses).
#[cfg(target_os = "macos")]
pub fn peak_memory_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `info` is a zeroed rusage_info_v4 and the flavor matches it.
    let rc = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr() as *mut libc::rusage_info_t,
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: proc_pid_rusage returned success, so the struct is initialised.
    let info = unsafe { info.assume_init() };
    Some(info.ri_lifetime_max_phys_footprint)
}

#[cfg(not(target_os = "macos"))]
pub fn peak_memory_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::manifest::Entities;

    fn write_wav(path: &Path, amplitude: i16, ms: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(16 * ms) {
            writer
                .write_sample(if i % 2 == 0 { amplitude } else { -amplitude })
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    fn clip(id: &str, reference: &str, expected: Expected, category: &str) -> Clip {
        Clip {
            id: id.to_string(),
            audio: format!("audio/{id}.wav"),
            reference: reference.to_string(),
            verified: true,
            expected,
            split: Split::Dev,
            categories: vec![category.to_string()],
            language: "nl".to_string(),
            language_mode: None,
            source: "synthetic".to_string(),
            prompt_id: None,
            speaker: "test".to_string(),
            device: "generated".to_string(),
            sample_rate: 16_000,
            channels: 1,
            duration_ms: 1_000,
            peak_amplitude: 0.3,
            condition: "normal".to_string(),
            noise: String::new(),
            entities: Entities::default(),
            raw_transcript: None,
            raw_model: None,
            recorded_at: "2026-09-17T10:00:00Z".to_string(),
            notes: String::new(),
        }
    }

    fn config() -> RunConfig {
        RunConfig {
            model: "fake".to_string(),
            language_mode: "nl".to_string(),
            vocab: VocabVariant::Developer,
            use_vad: true,
            resampler: Resampler::App,
        }
    }

    /// End-to-end over the harness with a canned "model", so the stage
    /// attribution and the tallies are tested without Whisper on disk.
    #[test]
    fn separates_model_errors_from_filter_discards() {
        let dir = std::env::temp_dir().join(format!("ft-eval-harness-{}", uuid::Uuid::new_v4()));
        manifest::ensure_layout(&dir).unwrap();
        let mut clips = vec![
            clip("long", "Dit is een gewone Nederlandse zin.", Expected::Speech, "everyday"),
            clip("short", "Ja, dat klopt.", Expected::Speech, "short_reply"),
            clip("wrong", "Deploy de API key morgen.", Expected::Speech, "mixed_tech"),
            clip("vad", "Morgen om drie uur.", Expected::Speech, "short_reply"),
            clip("noise", "", Expected::NonSpeech, "non_speech"),
            clip("echo", "", Expected::NonSpeech, "non_speech"),
            clip("silent", "", Expected::NonSpeech, "non_speech"),
        ];
        clips[2].entities.terms = vec!["API key".to_string()];
        for c in &clips {
            let amplitude = if c.id == "silent" { 10 } else { 9_000 };
            write_wav(&dir.join(&c.audio), amplitude, 1_200);
        }

        let transcribe = |wav: &Path, _: &DecodeOptions| -> Result<LocalTranscript, String> {
            let name = wav.file_stem().unwrap().to_str().unwrap();
            let text = match name {
                "long" => "Dit is een gewone Nederlandse zin.",
                "short" => "Ja, dat klopt.",
                "wrong" => "De ploy de API kie morgen.",
                "noise" => "Bedankt voor het kijken!",
                "echo" => "And Linux.",
                "silent" => panic!("the capture gate must stop silent clips before the model"),
                _ => "",
            };
            Ok(LocalTranscript {
                text: text.to_string(),
                language: (name != "vad").then(|| "nl".to_string()),
                vad_rejected: name == "vad",
                redecoded_as_dutch: false,
            })
        };

        let run = run_config(&clips, &dir, &config(), &transcribe, |_, _| {}).unwrap();
        let overall = &run.overall;
        assert_eq!((overall.speech_clips, overall.non_speech_clips), (4, 3));

        // The model got "short" right; only the word floor lost it.
        let short = run.clips.iter().find(|c| c.id == "short").unwrap();
        assert_eq!(short.raw_score.as_ref().unwrap().word_errors, 0);
        assert_eq!(short.final_score.as_ref().unwrap().word_errors, 3);
        assert_eq!(short.discarded_by.as_deref(), Some("under_word_floor"));

        assert_eq!(overall.speech_discarded, 2);
        assert_eq!(overall.speech_discarded_by["under_word_floor"], 1);
        assert_eq!(overall.speech_discarded_by["vad"], 1);

        // raw: "wrong" has 3 errors (deploy→de, +ploy, key→kie), "vad" loses 4 words.
        assert_eq!(overall.raw.ref_words, 6 + 3 + 5 + 4);
        assert_eq!(overall.raw.word_errors, 3 + 4);
        // final: additionally loses the 3 words of "short".
        assert_eq!(overall.final_stage.word_errors, 3 + 4 + 3);
        assert_eq!(overall.final_stage.terms, Hits { hit: 0, total: 1 });

        // Both hallucinations reach the raw stage; the filters catch both.
        assert_eq!((overall.hallucinated_raw, overall.hallucinated_final), (2, 0));
        let silent = run.clips.iter().find(|c| c.id == "silent").unwrap();
        assert_eq!(silent.discarded_by.as_deref(), Some(discard::CAPTURE_GATE));

        assert_eq!(run.by_category["short_reply"].speech_discarded, 2);
        assert_eq!(run.by_category["everyday"].final_stage.word_errors, 0);
        assert_eq!(worst_clips(&run, 2).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn selection_defaults_to_verified_clips_of_one_split() {
        let mut clips = vec![
            clip("a", "x", Expected::Speech, "everyday"),
            clip("b", "x", Expected::Speech, "numbers"),
            clip("c", "x", Expected::Speech, "everyday"),
            clip("d", "x", Expected::Speech, "everyday"),
        ];
        clips[1].split = Split::Test;
        clips[2].verified = false;
        let (dev, unverified) = select_clips(clips.clone(), Split::Dev, None, None, None);
        assert_eq!(dev.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["a", "d"]);
        assert_eq!(unverified, 1);
        let (test, _) = select_clips(clips.clone(), Split::Test, None, None, None);
        assert_eq!(test.len(), 1);
        let (filtered, _) = select_clips(clips, Split::Dev, Some("numbers"), None, None);
        assert!(filtered.is_empty());
    }

    #[test]
    fn percentiles_and_labels() {
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[10, 20, 30, 40], 50), 20);
        assert_eq!(percentile(&[10, 20, 30, 40], 95), 40);
        assert_eq!(config().label(), "fake · nl · vocab=developer");
        let mut c = config();
        c.language_mode = "system".into();
        c.use_vad = false;
        c.resampler = Resampler::Afconvert;
        assert_eq!(c.label(), "fake · auto · vocab=developer · vad=off · resample=afconvert");
    }
}
