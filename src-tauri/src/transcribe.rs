use std::path::Path;
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use reqwest::StatusCode;

use crate::storage::Provider;

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
}

// Both providers speak the OpenAI `/v1/audio/transcriptions` multipart shape —
// same form fields, different host + model.
const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const GROQ_MODEL: &str = "whisper-large-v3-turbo";
const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/audio/transcriptions";
const OPENAI_MODEL: &str = "whisper-1";

fn endpoint_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Groq => GROQ_ENDPOINT,
        Provider::Openai => OPENAI_ENDPOINT,
    }
}

fn model_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Groq => GROQ_MODEL,
        Provider::Openai => OPENAI_MODEL,
    }
}

fn env_var_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Groq => "GROQ_API_KEY",
        Provider::Openai => "OPENAI_API_KEY",
    }
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn normalize_language_mode(mode: &str) -> Option<&'static str> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "en" | "english" => Some("en"),
        _ => None,
    }
}

pub async fn transcribe_audio(
    session_id: u64,
    wav_path: &Path,
    duration_ms: u64,
    provider: Provider,
    api_key_override: Option<&str>,
    language_mode: Option<&str>,
) -> Result<String, String> {
    // Small delay keeps state transitions readable while the request starts.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let env_var = env_var_for(provider);
    let api_key = if let Some(value) = api_key_override {
        value.to_string()
    } else {
        std::env::var(env_var).map_err(|_| {
            format!(
                "No {} API key configured. Set one in onboarding or {}.",
                provider.as_str(),
                env_var
            )
        })?
    };
    let audio_bytes = std::fs::read(wav_path)
        .map_err(|e| format!("Failed to read captured audio for session {session_id}: {e}"))?;
    if audio_bytes.is_empty() {
        return Err("Captured audio file is empty".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let mut last_error = String::from("Unknown transcription error");
    for attempt in 1..=2 {
        let part = Part::bytes(audio_bytes.clone())
            .file_name("capture.wav")
            .mime_str("audio/wav")
            .map_err(|e| format!("Failed to build audio upload part: {e}"))?;
        let mut form = Form::new()
            .text("model", model_for(provider))
            .part("file", part);
        if let Some(mode) = language_mode.and_then(normalize_language_mode) {
            form = form.text("language", mode.to_string());
        }

        let response = client
            .post(endpoint_for(provider))
            .bearer_auth(&api_key)
            .multipart(form)
            .send()
            .await;

        match response {
            Ok(resp) if resp.status().is_success() => {
                let parsed: TranscriptionResponse = resp
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse transcription response: {e}"))?;
                return Ok(parsed.text.trim().to_string());
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unable to read error body".to_string());
                last_error = format!(
                    "{} transcription failed ({status}) for session {session_id}, duration {duration_ms}ms: {body}",
                    provider.as_str()
                );
                if !should_retry_status(status) {
                    break;
                }
            }
            Err(e) => {
                last_error = format!(
                    "Transcription request failed for session {session_id} (attempt {attempt}): {e}"
                );
            }
        }

        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(450)).await;
        }
    }

    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::{endpoint_for, model_for, normalize_language_mode, should_retry_status};
    use crate::storage::Provider;
    use reqwest::StatusCode;

    #[test]
    fn provider_routing_picks_right_endpoint_and_model() {
        assert_eq!(endpoint_for(Provider::Groq), super::GROQ_ENDPOINT);
        assert_eq!(model_for(Provider::Groq), "whisper-large-v3-turbo");
        assert_eq!(endpoint_for(Provider::Openai), super::OPENAI_ENDPOINT);
        assert_eq!(model_for(Provider::Openai), "whisper-1");
    }

    #[test]
    fn retries_on_rate_limit_and_server_errors() {
        assert!(should_retry_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(should_retry_status(StatusCode::BAD_GATEWAY));
    }

    #[test]
    fn does_not_retry_on_other_client_errors() {
        assert!(!should_retry_status(StatusCode::UNAUTHORIZED));
        assert!(!should_retry_status(StatusCode::BAD_REQUEST));
        assert!(!should_retry_status(StatusCode::NOT_FOUND));
    }

    #[test]
    fn language_mode_normalization() {
        assert_eq!(normalize_language_mode("en"), Some("en"));
        assert_eq!(normalize_language_mode("english"), Some("en"));
        assert_eq!(normalize_language_mode("system"), None);
        assert_eq!(normalize_language_mode(""), None);
    }
}
