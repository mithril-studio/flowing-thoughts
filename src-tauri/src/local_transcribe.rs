use crate::model_manager::{self, ModelId};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

static CONTEXT_CACHE: LazyLock<Mutex<HashMap<&'static str, Arc<WhisperContext>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const WHISPER_SAMPLE_RATE: u32 = 16_000;

fn get_or_load_context(model_id: ModelId) -> Result<Arc<WhisperContext>, String> {
    let key = match model_id {
        ModelId::TinyEn => "whisper-tiny-en",
        ModelId::BaseEn => "whisper-base-en",
        ModelId::DistilSmallEn => "distil-small-en",
    };
    {
        let guard = CONTEXT_CACHE
            .lock()
            .map_err(|_| "Whisper context cache lock poisoned".to_string())?;
        if let Some(ctx) = guard.get(key) {
            return Ok(ctx.clone());
        }
    }
    let path = model_manager::model_path(model_id)?;
    if !path.exists() {
        return Err(format!(
            "Model file not installed: {}",
            path.to_string_lossy()
        ));
    }
    let params = WhisperContextParameters::default();
    let ctx = WhisperContext::new_with_params(
        path.to_str()
            .ok_or_else(|| "Model path is not valid UTF-8".to_string())?,
        params,
    )
    .map_err(|e| format!("Failed to load whisper model {key}: {e}"))?;
    let arc = Arc::new(ctx);
    let mut guard = CONTEXT_CACHE
        .lock()
        .map_err(|_| "Whisper context cache lock poisoned".to_string())?;
    guard.insert(key, arc.clone());
    Ok(arc)
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

fn run_inference(ctx: &WhisperContext, audio: &[f32]) -> Result<String, String> {
    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create whisper state: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(num_cpus_threads());
    params.set_translate(false);
    params.set_language(Some("en"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);

    state
        .full(params, audio)
        .map_err(|e| format!("Whisper inference failed: {e}"))?;

    let n_segments = state.full_n_segments();
    let mut text = String::new();
    for i in 0..n_segments {
        let seg = state
            .get_segment(i)
            .ok_or_else(|| format!("Missing whisper segment {i}"))?;
        let seg_text = seg
            .to_str()
            .map_err(|e| format!("Failed to decode segment {i}: {e}"))?;
        text.push_str(seg_text);
    }
    Ok(text.trim().to_string())
}

fn num_cpus_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .min(8)
}

pub async fn transcribe_local(
    model_id: ModelId,
    wav_path: &Path,
) -> Result<(String, u64), String> {
    let wav_path = wav_path.to_path_buf();
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        let ctx = get_or_load_context(model_id)?;
        let audio = load_wav_as_mono_16k(&wav_path)?;
        run_inference(&ctx, &audio)
    })
    .await
    .map_err(|e| format!("Local transcription task panicked: {e}"))?;
    let text = result?;
    Ok((text, started.elapsed().as_millis() as u64))
}
