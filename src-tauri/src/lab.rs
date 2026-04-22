use crate::local_transcribe;
use crate::model_manager::ModelId;
use crate::storage::Provider;
use crate::transcribe;
use std::path::PathBuf;
use std::time::Instant;
use tokio::task::JoinHandle;

pub const MODEL_GROQ_API: &str = "groq-api";
pub const MODEL_OPENAI_API: &str = "openai-api";

fn api_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Groq => MODEL_GROQ_API,
        Provider::Openai => MODEL_OPENAI_API,
    }
}

fn local_label(id: ModelId) -> &'static str {
    match id {
        ModelId::TinyEn => "whisper-tiny-en",
        ModelId::BaseEn => "whisper-base-en",
        ModelId::DistilSmallEn => "distil-small-en",
    }
}

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

/// Handles to the non-primary transcription tasks. Callers await `join_all`
/// on a detached task so the primary can be injected immediately while the
/// rest of the fan-out finishes in the background for data collection.
pub struct PendingResults {
    handles: Vec<(&'static str, JoinHandle<LabResult>)>,
}

impl PendingResults {
    pub async fn join_all(self) -> Vec<LabResult> {
        let mut out = Vec::with_capacity(self.handles.len());
        for (label, handle) in self.handles {
            let result = match handle.await {
                Ok(r) => r,
                Err(e) => failure(label, 0, format!("Task panicked: {e}")),
            };
            out.push(result);
        }
        out
    }
}

/// Spawns all four transcription tasks (API + 3 local models), awaits only
/// the primary, and returns it alongside handles to the remaining three.
/// The caller injects on the primary and detaches a task to await the rest
/// for DB persistence.
pub async fn run_with_primary_first(
    session_id: u64,
    wav_path: PathBuf,
    duration_ms: u64,
    api_provider: Provider,
    api_key: Option<String>,
    language_mode: String,
    primary_label: String,
) -> (LabResult, PendingResults) {
    let api_label_str = api_label(api_provider);

    let api_handle: JoinHandle<LabResult> = {
        let wav_path = wav_path.clone();
        let language_mode = language_mode.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            if api_key.is_none() {
                return failure(
                    api_label_str,
                    0,
                    "API key not configured — skipped".to_string(),
                );
            }
            let result = transcribe::transcribe_audio(
                session_id,
                &wav_path,
                duration_ms,
                api_provider,
                api_key.as_deref(),
                Some(&language_mode),
            )
            .await;
            let latency = started.elapsed().as_millis() as u64;
            match result {
                Ok(text) => success(api_label_str, text, latency),
                Err(e) => failure(api_label_str, latency, e),
            }
        })
    };

    let local_handles: Vec<(&'static str, JoinHandle<LabResult>)> = ModelId::all()
        .into_iter()
        .map(|id| {
            let wav_path = wav_path.clone();
            let label = local_label(id);
            let handle = tokio::spawn(async move {
                match local_transcribe::transcribe_local(id, &wav_path).await {
                    Ok((text, latency_ms)) => success(label, text, latency_ms),
                    Err(e) => failure(label, 0, e),
                }
            });
            (label, handle)
        })
        .collect();

    let mut all: Vec<(&'static str, JoinHandle<LabResult>)> =
        Vec::with_capacity(1 + local_handles.len());
    all.push((api_label_str, api_handle));
    all.extend(local_handles);

    let primary_idx = all.iter().position(|(label, _)| *label == primary_label);
    let (primary_result, rest) = match primary_idx {
        Some(idx) => {
            let (label, handle) = all.swap_remove(idx);
            let result = match handle.await {
                Ok(r) => r,
                Err(e) => failure(label, 0, format!("Primary task panicked: {e}")),
            };
            (result, all)
        }
        None => (
            failure(
                &primary_label,
                0,
                format!("Unknown primary model label: {primary_label}"),
            ),
            all,
        ),
    };

    (primary_result, PendingResults { handles: rest })
}
