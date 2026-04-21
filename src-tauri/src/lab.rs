use crate::local_transcribe;
use crate::model_manager::ModelId;
use crate::transcribe;
use std::path::PathBuf;
use std::time::Instant;

pub const MODEL_OPENAI_API: &str = "openai-api";

#[derive(Debug, Clone, serde::Serialize)]
pub struct LabResult {
    pub model: String,
    pub text: Option<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

fn failure(model: &str, latency_ms: u64, message: String) -> LabResult {
    LabResult {
        model: model.to_string(),
        text: None,
        latency_ms,
        error: Some(message),
    }
}

fn success(model: &str, text: String, latency_ms: u64) -> LabResult {
    LabResult {
        model: model.to_string(),
        text: Some(text),
        latency_ms,
        error: None,
    }
}

pub async fn run_parallel(
    session_id: u64,
    wav_path: PathBuf,
    duration_ms: u64,
    api_key: Option<String>,
    language_mode: String,
) -> Vec<LabResult> {
    let api_task = {
        let wav_path = wav_path.clone();
        let language_mode = language_mode.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            if api_key.is_none() {
                return failure(
                    MODEL_OPENAI_API,
                    0,
                    "API key not configured — skipped".to_string(),
                );
            }
            let result = transcribe::transcribe_audio(
                session_id,
                &wav_path,
                duration_ms,
                api_key.as_deref(),
                Some(&language_mode),
            )
            .await;
            let latency = started.elapsed().as_millis() as u64;
            match result {
                Ok(text) => success(MODEL_OPENAI_API, text, latency),
                Err(e) => failure(MODEL_OPENAI_API, latency, e),
            }
        })
    };

    let local_tasks: Vec<_> = ModelId::all()
        .into_iter()
        .map(|id| {
            let wav_path = wav_path.clone();
            let label = match id {
                ModelId::TinyEn => "whisper-tiny-en",
                ModelId::BaseEn => "whisper-base-en",
                ModelId::DistilSmallEn => "distil-small-en",
            };
            tokio::spawn(async move {
                match local_transcribe::transcribe_local(id, &wav_path).await {
                    Ok((text, latency_ms)) => success(label, text, latency_ms),
                    Err(e) => failure(label, 0, e),
                }
            })
        })
        .collect();

    let api_out = match api_task.await {
        Ok(result) => result,
        Err(e) => failure(MODEL_OPENAI_API, 0, format!("API task panicked: {e}")),
    };

    let mut results = vec![api_out];
    for (idx, handle) in local_tasks.into_iter().enumerate() {
        let label = match idx {
            0 => "whisper-tiny-en",
            1 => "whisper-base-en",
            2 => "distil-small-en",
            _ => "unknown",
        };
        let out = match handle.await {
            Ok(result) => result,
            Err(e) => failure(label, 0, format!("Local task panicked: {e}")),
        };
        results.push(out);
    }
    results
}
