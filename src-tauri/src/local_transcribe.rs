use crate::model_manager::{self, Engine, ModelId};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams};
use transcribe_rs::onnx::Quantization;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext,
    WhisperVadContextParams, WhisperVadParams,
};

static CONTEXT_CACHE: LazyLock<Mutex<HashMap<String, Arc<WhisperContext>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The VAD model, loaded once and reused. Rebuilding it per dictation would
/// mean an init/free cycle on every keypress for no benefit.
static VAD_CONTEXT: LazyLock<Mutex<Option<(String, WhisperVadContext)>>> =
    LazyLock::new(|| Mutex::new(None));

/// The loaded Parakeet model, tagged with its id. Decoding needs `&mut`, so the
/// lock is held for the whole transcription — fine, since exactly one
/// dictation runs at a time.
static PARAKEET: LazyLock<Mutex<Option<(String, ParakeetModel)>>> =
    LazyLock::new(|| Mutex::new(None));

const WHISPER_SAMPLE_RATE: u32 = 16_000;

fn get_or_load_context(model_id: &ModelId) -> Result<Arc<WhisperContext>, String> {
    let key = model_id.id();
    // Held across the load on purpose: a dictation that arrives while the
    // startup preload is still reading the file waits for it instead of
    // loading a second copy of the model alongside.
    let mut guard = CONTEXT_CACHE
        .lock()
        .map_err(|_| "Whisper context cache lock poisoned".to_string())?;
    if let Some(ctx) = guard.get(&key) {
        return Ok(ctx.clone());
    }
    let path = model_manager::model_path(model_id)?;
    if !path.exists() {
        return Err(format!(
            "Model file not installed: {}",
            path.to_string_lossy()
        ));
    }
    let mut params = WhisperContextParameters::default();
    // Off by default in whisper-rs. Same output, less attention memory
    // traffic — a free speed-up on Metal.
    params.flash_attn(true);
    let ctx = WhisperContext::new_with_params(
        path.to_str()
            .ok_or_else(|| "Model path is not valid UTF-8".to_string())?,
        params,
    )
    .map_err(|e| format!("Failed to load whisper model {key}: {e}"))?;
    let arc = Arc::new(ctx);
    guard.insert(key, arc.clone());
    Ok(arc)
}

fn with_parakeet<T>(
    model_id: &ModelId,
    run: impl FnOnce(&mut ParakeetModel) -> Result<T, String>,
) -> Result<T, String> {
    let key = model_id.id();
    let mut guard = PARAKEET
        .lock()
        .map_err(|_| "Parakeet model lock poisoned".to_string())?;
    if guard.as_ref().map(|(k, _)| k.as_str()) != Some(key.as_str()) {
        if !model_manager::is_installed(model_id) {
            return Err(format!("Model not installed: {key}"));
        }
        let dir = model_manager::model_path(model_id)?;
        let model = ParakeetModel::load(&dir, &Quantization::Int8)
            .map_err(|e| format!("Failed to load Parakeet model {key}: {e}"))?;
        *guard = Some((key, model));
    }
    run(&mut guard.as_mut().expect("just initialised").1)
}

/// Parakeet has no language switch and no prompt: it picks the language from
/// the audio and decodes. The VAD gate still runs first — cheaper than the
/// encoder, and it keeps the "no speech" behaviour identical across engines.
fn run_parakeet(
    model_id: &ModelId,
    audio: &[f32],
    vad_model: Option<&str>,
) -> Result<Decoded, String> {
    if let Some(path) = vad_model {
        if !contains_speech(path, audio)? {
            return Ok(Decoded {
                text: String::new(),
                lang_id: 0,
                vad_rejected: true,
            });
        }
    }
    let text = with_parakeet(model_id, |model| {
        model
            .transcribe_with(audio, &ParakeetParams::default())
            .map(|r| r.text)
            .map_err(|e| format!("Parakeet inference failed: {e}"))
    })?;
    let vad_state = if vad_model.is_some() { "on" } else { "off" };
    let _ = crate::storage::append_log(
        "INFO",
        &format!("Parakeet decoded {} chars (VAD {vad_state})", text.trim().len()),
    );
    // Parakeet does not report which language it decoded, so `lang_id` is
    // a placeholder the caller ignores.
    Ok(Decoded {
        text: text.trim().to_string(),
        lang_id: 0,
        vad_rejected: false,
    })
}

/// Load the model and push one second of silence through it, so the first
/// real dictation after launch doesn't pay for reading the file, building the
/// Metal pipelines and allocating the compute buffers.
pub fn preload(model_id: &ModelId) -> Result<u64, String> {
    let started = Instant::now();
    let silence = vec![0.0f32; WHISPER_SAMPLE_RATE as usize];
    match model_id.engine() {
        Engine::Parakeet => with_parakeet(model_id, |model| {
            model
                .transcribe_with(&silence, &ParakeetParams::default())
                .map(|_| ())
                .map_err(|e| format!("Parakeet warm-up failed: {e}"))
        })?,
        Engine::Whisper => {
            let ctx = get_or_load_context(model_id)?;
            let mut state = ctx
                .create_state()
                .map_err(|e| format!("Failed to create whisper state: {e}"))?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_n_threads(num_cpus_threads());
            params.set_language(Some("en"));
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            state
                .full(params, &silence)
                .map_err(|e| format!("Whisper warm-up failed: {e}"))?;
        }
    }
    Ok(started.elapsed().as_millis() as u64)
}

fn load_wav_as_mono_16k(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| format!("Failed to open wav: {e}"))?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample != 16 {
        return Err(format!(
            "Unsupported wav format: {:?} {}-bit",
            spec.sample_format, spec.bits_per_sample
        ));
    }
    let channels = spec.channels.max(1) as usize;
    let src_rate = spec.sample_rate;

    let samples: Vec<i16> = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to read wav samples: {e}"))?;

    let mono: Vec<f32> = samples
        .chunks(channels)
        .map(|frame| {
            let sum: f32 = frame
                .iter()
                .map(|&s| f32::from(s) / f32::from(i16::MAX))
                .sum();
            sum / channels as f32
        })
        .collect();

    if src_rate == WHISPER_SAMPLE_RATE {
        return Ok(mono);
    }

    let ratio = WHISPER_SAMPLE_RATE as f32 / src_rate as f32;
    let out_len = ((mono.len() as f32) * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_idx = i as f32 / ratio;
        let floor = src_idx.floor() as usize;
        let frac = src_idx - floor as f32;
        let a = mono.get(floor).copied().unwrap_or(0.0);
        let b = mono.get(floor + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    Ok(out)
}

/// Languages the app supports. When auto-detecting, any detection outside
/// this set is treated as a misdetection and the audio is re-decoded with a
/// forced language, so dictations can't come out in e.g. German or Afrikaans.
const ALLOWED_LANGS: [&str; 2] = ["en", "nl"];

/// Segments whose no-speech probability exceeds this are dropped. Whisper
/// hallucinates YouTube outros ("Thanks for watching!", "Subscribe to my
/// channel!") on silence and breath/keyboard noise; those segments carry a
/// high no-speech probability while real dictation stays well below it. 0.6
/// is the threshold OpenAI's reference implementation uses.
const NO_SPEECH_PROB_THRESHOLD: f32 = 0.6;

/// Silero VAD tuning. Defaults except `min_speech_duration_ms`, which is
/// raised from 250 ms: a quarter-second blip is a door, a cough, or a key
/// press, and letting it through would put the decoder right back in the
/// situation this gate exists to prevent. Dictation always clears 400 ms
/// because the hotkey must be held a full second before a session commits.
const VAD_MIN_SPEECH_MS: i32 = 400;

fn vad_params() -> WhisperVadParams {
    let mut vad = WhisperVadParams::default();
    vad.set_min_speech_duration(VAD_MIN_SPEECH_MS);
    vad
}

/// Does this capture contain speech at all?
///
/// Deliberately runs as a standalone pre-pass rather than via
/// `FullParams::enable_vad`. That flag is only honoured by `whisper_full()`;
/// this app decodes through `whisper_full_with_state()` (per-dictation state),
/// which ignores `params.vad` outright — setting it looks like it works and
/// silently does nothing. Verified by test: with the flag set, near-silent
/// audio still came back "Thanks for watching."
///
/// Used purely as a gate, not a splicer. When speech is present the original
/// untouched audio goes to the decoder, so transcription accuracy is exactly
/// as before; only the all-silence case changes, and it never reaches Whisper.
fn contains_speech(vad_model: &str, audio: &[f32]) -> Result<bool, String> {
    let mut guard = VAD_CONTEXT
        .lock()
        .map_err(|_| "VAD context lock poisoned".to_string())?;
    if guard.as_ref().map(|(p, _)| p.as_str()) != Some(vad_model) {
        // Silero is ~865 KB and runs in well under a millisecond on CPU.
        // Keeping it off the GPU avoids standing up a second Metal device
        // purely for the gate — which also trips a teardown assert in ggml
        // when the context is freed.
        let mut ctx_params = WhisperVadContextParams::default();
        ctx_params.set_use_gpu(false);
        let ctx = WhisperVadContext::new(vad_model, ctx_params)
            .map_err(|e| format!("Failed to load VAD model: {e}"))?;
        *guard = Some((vad_model.to_string(), ctx));
    }
    let vad_ctx = &mut guard.as_mut().expect("just initialised").1;
    let segments = vad_ctx
        .segments_from_samples(vad_params(), audio)
        .map_err(|e| format!("VAD failed: {e}"))?;
    let n = segments.num_segments();
    let speech_ms: f32 = segments
        .into_iter()
        .map(|s| (s.end - s.start) * 10.0)
        .sum();
    let total_ms = audio.len() as f32 / WHISPER_SAMPLE_RATE as f32 * 1000.0;
    let _ = crate::storage::append_log(
        "INFO",
        &format!("VAD found {n} speech segment(s), {speech_ms:.0}ms of {total_ms:.0}ms captured"),
    );
    Ok(n > 0)
}

/// One decode pass. `vad_rejected` separates "the VAD gate kept this audio
/// away from the decoder" from "the decoder ran and produced nothing".
struct Decoded {
    text: String,
    lang_id: i32,
    vad_rejected: bool,
}

fn run_inference(
    ctx: &WhisperContext,
    audio: &[f32],
    language: &str,
    prompt: Option<&str>,
    vad_model: Option<&str>,
) -> Result<Decoded, String> {
    // Voice activity detection, when the model is on disk. This runs *before*
    // the decoder: audio with no detected speech never reaches Whisper, so
    // there are no tokens to hallucinate from. Every other guard in this app
    // asks the model to grade its own output, which fails precisely because
    // the model is confident about its inventions.
    //
    // Strictly optional — a missing VAD model degrades to the old behaviour
    // rather than breaking transcription.
    if let Some(path) = vad_model {
        if !contains_speech(path, audio)? {
            return Ok(Decoded {
                text: String::new(),
                lang_id: 0,
                vad_rejected: true,
            });
        }
    }

    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create whisper state: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(num_cpus_threads());
    params.set_translate(false);
    params.set_language(Some(language));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    // Suppress non-speech tokens ([BLANK_AUDIO], music markers, etc.) at the
    // decoder level; lib.rs additionally filters hallucinated credit lines.
    params.set_suppress_nst(true);
    if let Some(p) = prompt.filter(|s| !s.trim().is_empty()) {
        params.set_initial_prompt(p);
    }

    state
        .full(params, audio)
        .map_err(|e| format!("Whisper inference failed: {e}"))?;

    let n_segments = state.full_n_segments();
    let mut text = String::new();
    let mut dropped = 0;
    let mut probs: Vec<String> = Vec::with_capacity(n_segments.max(0) as usize);
    for i in 0..n_segments {
        let seg = state
            .get_segment(i)
            .ok_or_else(|| format!("Missing whisper segment {i}"))?;
        let no_speech = seg.no_speech_probability();
        probs.push(format!("{no_speech:.2}"));
        if no_speech > NO_SPEECH_PROB_THRESHOLD {
            dropped += 1;
            continue;
        }
        let seg_text = seg
            .to_str()
            .map_err(|e| format!("Failed to decode segment {i}: {e}"))?;
        text.push_str(seg_text);
    }
    // Always record the no-speech distribution, not just the drops. The gate
    // above had never once fired across the whole log history, and a silent
    // guard is indistinguishable from a guard that is working — this makes the
    // difference visible.
    let vad_state = if vad_model.is_some() { "on" } else { "off" };
    if n_segments == 0 {
        let _ = crate::storage::append_log(
            "INFO",
            &format!("Whisper returned no segments (VAD {vad_state}) — no speech in this capture"),
        );
    } else {
        let _ = crate::storage::append_log(
            "INFO",
            &format!(
                "Whisper {n_segments} segment(s) (VAD {vad_state}), dropped {dropped} over no-speech threshold {NO_SPEECH_PROB_THRESHOLD}, probs [{}]",
                probs.join(", ")
            ),
        );
    }
    Ok(Decoded {
        text: text.trim().to_string(),
        lang_id: state.full_lang_id_from_state(),
        vad_rejected: false,
    })
}

fn num_cpus_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .min(8)
}

/// Map the app-level language mode ("en" | "nl" | "system") to the whisper
/// language code for a given model. English-only models always decode as
/// English; multilingual models honour the hint or auto-detect.
fn whisper_language(model_id: &ModelId, language_mode: &str) -> &'static str {
    if !model_id.is_multilingual() {
        return "en";
    }
    match language_mode.trim().to_ascii_lowercase().as_str() {
        "en" | "english" => "en",
        "nl" | "dutch" | "nederlands" => "nl",
        _ => "auto",
    }
}

/// What one local transcription produced, plus the diagnostics the eval
/// harness needs to tell a model error from a gate that discarded speech.
#[derive(Debug, Clone)]
pub struct LocalTranscript {
    pub text: String,
    /// Whisper language code of the decode that produced `text`; `None` when
    /// the VAD gate stopped the audio before any decode, or when the engine
    /// (Parakeet) does not report one.
    pub language: Option<String>,
    /// The VAD gate found no speech, so Whisper never ran.
    pub vad_rejected: bool,
    /// Auto-detection landed outside `ALLOWED_LANGS` and the audio was
    /// re-decoded as Dutch.
    pub redecoded_as_dutch: bool,
}

/// Decoder inputs beyond the audio itself.
#[derive(Debug, Clone)]
pub struct DecodeOptions {
    /// App-level language mode: "en" | "nl" | "system".
    pub language_mode: String,
    pub prompt: Option<String>,
    /// Gate audio behind Silero VAD when its model is installed. Dictation
    /// always passes `true`; only the eval harness turns it off, to measure
    /// what the gate costs and buys.
    pub use_vad: bool,
}

/// Route whisper.cpp/ggml's stderr chatter into whisper-rs's logging hooks —
/// with no log backend enabled that silences it. For command-line tooling
/// whose own output must stay readable; the app leaves the default alone.
pub fn quiet_native_logging() {
    whisper_rs::install_logging_hooks();
}

/// Load (or fetch from cache) the model without decoding anything, so callers
/// that time transcription can keep model load out of the measurement.
pub fn preload_model(model_id: &ModelId) -> Result<(), String> {
    match model_id.engine() {
        Engine::Parakeet => with_parakeet(model_id, |_| Ok(())),
        Engine::Whisper => get_or_load_context(model_id).map(|_| ()),
    }
}

/// The whole local transcription of one WAV, synchronously: resample, VAD
/// gate, decode, and the out-of-set language retry. `transcribe_local` runs
/// this on a blocking thread; the eval harness calls it directly.
pub fn transcribe_wav_blocking(
    model_id: &ModelId,
    wav_path: &Path,
    options: &DecodeOptions,
) -> Result<LocalTranscript, String> {
    let language = whisper_language(model_id, &options.language_mode);
    // Resolved once per dictation, not per decode pass, so the retry below
    // cannot disagree with the first pass about whether VAD is on.
    let vad_model = (options.use_vad && model_manager::vad_model_installed())
        .then(|| model_manager::vad_model_path().ok())
        .flatten()
        .and_then(|p| p.to_str().map(str::to_owned));
    if options.use_vad && vad_model.is_none() {
        let _ = crate::storage::append_log(
            "WARN",
            "VAD model missing — decoding without the silence gate, so Whisper may invent text on near-silent audio",
        );
    }
    let audio = load_wav_as_mono_16k(wav_path)?;
    let vad = vad_model.as_deref();
    if model_id.engine() == Engine::Parakeet {
        let decoded = run_parakeet(model_id, &audio, vad)?;
        return Ok(LocalTranscript {
            text: decoded.text,
            language: None,
            vad_rejected: decoded.vad_rejected,
            redecoded_as_dutch: false,
        });
    }
    let ctx = get_or_load_context(model_id)?;
    let prompt = options.prompt.as_deref();
    let decoded = run_inference(&ctx, &audio, language, prompt, vad)?;
    if decoded.vad_rejected {
        return Ok(LocalTranscript {
            text: decoded.text,
            language: None,
            vad_rejected: true,
            redecoded_as_dutch: false,
        });
    }
    if language == "auto" {
        let detected = whisper_rs::get_lang_str(decoded.lang_id).unwrap_or("");
        if !ALLOWED_LANGS.contains(&detected) {
            // Detection landed outside the supported set — in practice
            // almost always Dutch misread as Afrikaans/German. Re-decode
            // forced to Dutch (English detection is reliable, so an
            // out-of-set detection was not English speech).
            let retry = run_inference(&ctx, &audio, "nl", prompt, vad)?;
            return Ok(LocalTranscript {
                text: retry.text,
                language: Some("nl".to_string()),
                vad_rejected: false,
                redecoded_as_dutch: true,
            });
        }
    }
    let language = if language == "auto" {
        whisper_rs::get_lang_str(decoded.lang_id).unwrap_or("").to_string()
    } else {
        language.to_string()
    };
    Ok(LocalTranscript {
        text: decoded.text,
        language: Some(language),
        vad_rejected: false,
        redecoded_as_dutch: false,
    })
}

pub async fn transcribe_local(
    model_id: ModelId,
    wav_path: &Path,
    language_mode: &str,
    prompt: Option<String>,
) -> Result<(String, u64), String> {
    let wav_path = wav_path.to_path_buf();
    let options = DecodeOptions {
        language_mode: language_mode.to_string(),
        prompt,
        use_vad: true,
    };
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        transcribe_wav_blocking(&model_id, &wav_path, &options)
    })
    .await
    .map_err(|e| format!("Local transcription task panicked: {e}"))?;
    let text = result?.text;
    Ok((text, started.elapsed().as_millis() as u64))
}

#[cfg(test)]
mod tests {
    use super::whisper_language;
    use crate::model_manager::ModelId;

    #[test]
    fn multilingual_models_honour_language_mode() {
        assert_eq!(whisper_language(&ModelId::SmallQ5, "nl"), "nl");
        assert_eq!(whisper_language(&ModelId::SmallQ5, "en"), "en");
        assert_eq!(whisper_language(&ModelId::SmallQ5, "system"), "auto");
        assert_eq!(whisper_language(&ModelId::BaseQ5, "nl"), "nl");
        assert_eq!(whisper_language(&ModelId::LargeV3TurboQ5, "nl"), "nl");
        assert_eq!(whisper_language(&ModelId::LargeV3TurboQ5, "system"), "auto");
    }

    #[test]
    fn english_only_models_always_decode_english() {
        assert_eq!(whisper_language(&ModelId::TinyEn, "nl"), "en");
        assert_eq!(whisper_language(&ModelId::DistilSmallEn, "system"), "en");
    }

    /// Write low-level noise that clears the app's 0.015 peak gate but carries
    /// no speech — the exact condition that produced "And Linux." in the field.
    /// Deterministic LCG so the check is reproducible.
    fn write_near_silence(path: &std::path::Path, duration_ms: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: super::WHISPER_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let n = super::WHISPER_SAMPLE_RATE * duration_ms / 1000;
        let mut seed: u32 = 0x1234_5678;
        for _ in 0..n {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // ±650 of i16 range ≈ 0.02 peak, just over the silence threshold.
            let sample = ((seed >> 16) as i32 % 651) as i16;
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    #[ignore = "needs the 190 MB speech model + VAD model installed; run with --ignored"]
    fn vad_stops_whisper_inventing_text_on_near_silence() {
        let dir = std::env::temp_dir().join("flowing_thoughts_vad_check");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("near_silence.wav");
        write_near_silence(&wav, 1_700);

        let ctx = super::get_or_load_context(&ModelId::SmallQ5).expect("speech model installed");
        let audio = super::load_wav_as_mono_16k(&wav).unwrap();
        let peak = audio.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(
            peak > 0.015,
            "premise broke: audio must clear the app's silence gate, peak was {peak}"
        );

        // Same biased prompt the app sends — that is what the decoder continues.
        let prompt = crate::dev_vocab::build_biased_prompt(&[], 800).unwrap();
        let vad_path = crate::model_manager::vad_model_path().unwrap();

        let without_vad = super::run_inference(&ctx, &audio, "en", Some(&prompt), None)
            .unwrap()
            .text;
        let with_vad =
            super::run_inference(&ctx, &audio, "en", Some(&prompt), vad_path.to_str())
                .unwrap()
                .text;

        println!("  without VAD: {without_vad:?}");
        println!("  with VAD:    {with_vad:?}");
        assert!(
            with_vad.trim().is_empty(),
            "VAD let non-speech reach the decoder: {with_vad:?}"
        );
    }

    /// Parakeet end to end: loads, stays silent on near-silence, and — with
    /// FT_SAMPLE_WAV set to a 16 kHz mono WAV — prints a transcript and timing.
    #[test]
    #[ignore = "needs the 670 MB Parakeet model + VAD model installed; run with --ignored"]
    fn parakeet_loads_and_decodes() {
        let id = ModelId::ParakeetV3;
        let load_ms = super::preload(&id).expect("parakeet model installed");
        println!("  parakeet preload: {load_ms} ms");
        let vad_path = crate::model_manager::vad_model_path().unwrap();

        let dir = std::env::temp_dir().join("flowing_thoughts_parakeet_check");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("near_silence.wav");
        write_near_silence(&wav, 1_700);
        let audio = super::load_wav_as_mono_16k(&wav).unwrap();
        let gated = super::run_parakeet(&id, &audio, vad_path.to_str()).unwrap();
        assert!(gated.vad_rejected, "VAD let non-speech through: {:?}", gated.text);
        let ungated = super::run_parakeet(&id, &audio, None).unwrap().text;
        println!("  parakeet on near-silence without VAD: {ungated:?}");

        let sample = std::env::var("FT_SAMPLE_WAV").unwrap_or_default();
        if !sample.is_empty() {
            let audio = super::load_wav_as_mono_16k(std::path::Path::new(&sample)).unwrap();
            let started = std::time::Instant::now();
            let text = super::run_parakeet(&id, &audio, vad_path.to_str()).unwrap().text;
            println!(
                "  parakeet transcript ({} ms for {:.1}s audio): {text:?}",
                started.elapsed().as_millis(),
                audio.len() as f32 / super::WHISPER_SAMPLE_RATE as f32
            );
        }
    }

    /// Smoke test for the large-v3-turbo GGML file: it must load through the
    /// bundled whisper.cpp (v3 models use 128 mel bins) and decode. Set
    /// FT_SAMPLE_WAV to a 16 kHz mono WAV to also print a real transcript.
    #[test]
    #[ignore = "needs the 574 MB large-v3-turbo model + VAD model installed; run with --ignored"]
    fn large_v3_turbo_loads_and_decodes() {
        let ctx = super::get_or_load_context(&ModelId::LargeV3TurboQ5)
            .expect("large-v3-turbo model installed");
        let vad_path = crate::model_manager::vad_model_path().unwrap();

        let dir = std::env::temp_dir().join("flowing_thoughts_turbo_check");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("near_silence.wav");
        write_near_silence(&wav, 1_700);
        let audio = super::load_wav_as_mono_16k(&wav).unwrap();
        let silence_text = super::run_inference(&ctx, &audio, "nl", None, vad_path.to_str())
            .unwrap()
            .text;
        assert!(
            silence_text.trim().is_empty(),
            "turbo invented text on near-silence: {silence_text:?}"
        );

        let sample = std::env::var("FT_SAMPLE_WAV").unwrap_or_default();
        if !sample.is_empty() {
            let audio = super::load_wav_as_mono_16k(std::path::Path::new(&sample)).unwrap();
            // FT_SAMPLE_LANG=nl shows what forcing the language saves over "auto".
            let lang = std::env::var("FT_SAMPLE_LANG").unwrap_or_else(|_| "auto".to_string());
            for (label, model) in [("turbo", ModelId::LargeV3TurboQ5), ("small", ModelId::SmallQ5)] {
                let Ok(ctx) = super::get_or_load_context(&model) else { continue };
                let started = std::time::Instant::now();
                let decoded =
                    super::run_inference(&ctx, &audio, &lang, None, vad_path.to_str()).unwrap();
                let (text, lang) = (decoded.text, decoded.lang_id);
                println!(
                    "  {label} transcript ({} ms, lang {}): {text:?}",
                    started.elapsed().as_millis(),
                    whisper_rs::get_lang_str(lang).unwrap_or("?")
                );
            }
        }
    }
}
