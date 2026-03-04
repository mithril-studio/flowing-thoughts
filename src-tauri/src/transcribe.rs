use std::path::Path;
use std::time::Duration;

use reqwest::multipart::{Form, Part};

#[derive(serde::Deserialize)]
struct OpenAiTranscriptionResponse {
    text: String,
}

pub async fn transcribe_audio(
    session_id: u64,
    wav_path: &Path,
    duration_ms: u64,
) -> Result<String, String> {
    // Small delay keeps state transitions readable while the request starts.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY is not set".to_string())?;
    let audio_bytes = std::fs::read(wav_path)
        .map_err(|e| format!("Failed to read captured audio for session {session_id}: {e}"))?;
    if audio_bytes.is_empty() {
        return Err("Captured audio file is empty".to_string());
    }

    let part = Part::bytes(audio_bytes)
        .file_name("capture.wav")
        .mime_str("audio/wav")
        .map_err(|e| format!("Failed to build audio upload part: {e}"))?;
    let form = Form::new()
        .text("model", "whisper-1")
        .part("file", part);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;
    let response = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Transcription request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unable to read error body".to_string());
        return Err(format!(
            "OpenAI transcription failed ({status}) for session {session_id}, duration {duration_ms}ms: {body}"
        ));
    }

    let parsed: OpenAiTranscriptionResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse transcription response: {e}"))?;

    Ok(parsed.text.trim().to_string())
}
