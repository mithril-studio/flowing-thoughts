use std::time::Duration;

/// Placeholder transcription worker.
///
/// This gives us a real async pipeline today while audio capture and Whisper wiring
/// are still being implemented.
pub async fn transcribe_placeholder(session_id: u64) -> Result<String, String> {
    tokio::time::sleep(Duration::from_millis(900)).await;
    Ok(format!(
        "Session {} captured. This is a placeholder transcription while Whisper integration is in progress.",
        session_id
    ))
}
