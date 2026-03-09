use std::path::Path;
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use reqwest::StatusCode;

#[derive(serde::Deserialize)]
struct OpenAiTranscriptionResponse {
    text: String,
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

pub async fn transcribe_audio(
    session_id: u64,
    wav_path: &Path,
    duration_ms: u64,
    api_key_override: Option<&str>,
) -> Result<String, String> {
    // Small delay keeps state transitions readable while the request starts.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let api_key = if let Some(value) = api_key_override {
        value.to_string()
    } else {
        std::env::var("OPENAI_API_KEY").map_err(|_| {
            "No API key configured. Set one in onboarding or OPENAI_API_KEY.".to_string()
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
        let form = Form::new()
            .text("model", "whisper-1")
            .part("file", part);

        let response = client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .bearer_auth(&api_key)
            .multipart(form)
            .send()
            .await;

        match response {
            Ok(resp) if resp.status().is_success() => {
                let parsed: OpenAiTranscriptionResponse = resp
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
                    "OpenAI transcription failed ({status}) for session {session_id}, duration {duration_ms}ms: {body}"
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
    use super::should_retry_status;
    use reqwest::StatusCode;

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
}
