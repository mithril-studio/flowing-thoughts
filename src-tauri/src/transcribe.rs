use std::time::Duration;
use std::path::Path;

/// Placeholder transcription worker.
///
/// This gives us a real async pipeline today while audio capture and Whisper wiring
/// are still being implemented.
pub async fn transcribe_placeholder(
    session_id: u64,
    wav_path: &Path,
    duration_ms: u64,
) -> Result<String, String> {
    tokio::time::sleep(Duration::from_millis(900)).await;

    let file_size = std::fs::metadata(wav_path)
        .map_err(|e| format!("Failed to inspect audio capture file: {e}"))?
        .len();

    Ok(format!(
        "Session {session_id} captured ({duration_ms}ms, {file_size} bytes). Placeholder transcript until Whisper API integration is finished."
    ))
}
