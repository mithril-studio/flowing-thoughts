use std::sync::LazyLock;
use std::time::Duration;

use reqwest::StatusCode;

use crate::storage::HistoryEntry;

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("Failed to build shared HTTP client for coaching")
});

const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

// Keep any single transcript from blowing up the token budget.
const MAX_CHARS_PER_ENTRY: usize = 800;

const SYSTEM_PROMPT: &str = "You are a concise English coach for a non-native speaker. \
Below are their recent dictation transcripts (speech turned into text). \
Give brief, high-signal, actionable tips as plain-text bullet points (max 7 bullets). \
Cover, when relevant: filler words they overuse, words they repeat too often (with better \
alternatives), and grammar or phrasing that sounds non-native (show the natural version). \
Focus only on the top recurring patterns — do NOT rewrite everything and do NOT summarize \
the content. Start each tip with '- '. No preamble, no closing remarks.";

/// The coaching result returned to the frontend and cached in the kv store.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CoachingResult {
    pub tips: String,
    pub sample_count: usize,
    pub generated_at: String,
}

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(serde::Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(serde::Deserialize)]
struct ChatMessage {
    content: String,
}

/// Build the numbered transcript block sent as the user message.
fn build_user_message(entries: &[HistoryEntry]) -> String {
    let mut out = String::from("Recent dictations:\n");
    for (i, entry) in entries.iter().enumerate() {
        let text = entry.text.trim();
        let clipped: String = if text.chars().count() > MAX_CHARS_PER_ENTRY {
            text.chars().take(MAX_CHARS_PER_ENTRY).collect::<String>() + "…"
        } else {
            text.to_string()
        };
        out.push_str(&format!("{}. {}\n", i + 1, clipped));
    }
    out
}

/// Send the last N transcripts to OpenRouter and return concise coaching tips.
pub async fn generate_tips(
    entries: &[HistoryEntry],
    model: &str,
    api_key: &str,
) -> Result<String, String> {
    if api_key.trim().is_empty() {
        return Err("No OpenRouter API key configured. Add one in Settings → Coaching.".to_string());
    }
    // Only consider entries that actually contain words.
    let usable: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|e| !e.text.trim().is_empty())
        .collect();
    if usable.is_empty() {
        return Err("Dictate a bit more first — there's nothing to analyze yet.".to_string());
    }
    let owned: Vec<HistoryEntry> = usable.into_iter().cloned().collect();
    let user_message = build_user_message(&owned);

    let body = serde_json::json!({
        "model": model,
        "temperature": 0.3,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": user_message },
        ],
    });

    let response = HTTP_CLIENT
        .post(OPENROUTER_ENDPOINT)
        .bearer_auth(api_key.trim())
        .header("X-Title", "FlowingThoughts")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Coaching request failed: {e}"))?;

    let status = response.status();
    if status.is_success() {
        let parsed: ChatResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse coaching response: {e}"))?;
        let tips = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content.trim().to_string())
            .unwrap_or_default();
        if tips.is_empty() {
            return Err("The model returned no tips. Try again.".to_string());
        }
        return Ok(tips);
    }

    let body_text = response
        .text()
        .await
        .unwrap_or_else(|_| "Unable to read error body".to_string());
    let friendly = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            "OpenRouter rejected the API key. Check it in Settings → Coaching.".to_string()
        }
        StatusCode::TOO_MANY_REQUESTS => {
            "OpenRouter rate limit hit. Wait a moment and try again.".to_string()
        }
        StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST => {
            format!("OpenRouter couldn't use that model ({status}). Check the model id in Settings.")
        }
        _ => format!("Coaching failed ({status}): {body_text}"),
    };
    Err(friendly)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            session_id: 1,
            text: text.to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn user_message_numbers_entries() {
        let entries = vec![entry("hello world"), entry("second one")];
        let msg = build_user_message(&entries);
        assert!(msg.contains("1. hello world"));
        assert!(msg.contains("2. second one"));
    }

    #[test]
    fn long_entries_are_clipped() {
        let long = "a ".repeat(1000);
        let msg = build_user_message(&[entry(&long)]);
        assert!(msg.chars().count() < long.chars().count());
        assert!(msg.contains('…'));
    }
}
