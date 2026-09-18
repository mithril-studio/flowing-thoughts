//! The evaluation dataset on disk: `manifest.jsonl` plus `audio/*.wav`.
//!
//! The dataset is personal voice data and lives outside git, by default in
//! the app data directory so it survives worktrees and `git clean`.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "manifest.jsonl";
pub const AUDIO_DIR: &str = "audio";
pub const RESULTS_DIR: &str = "results";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Dev,
    Test,
}

impl Split {
    pub fn as_str(self) -> &'static str {
        match self {
            Split::Dev => "dev",
            Split::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expected {
    Speech,
    NonSpeech,
}

/// Items whose accuracy is scored separately from WER. `/` separates accepted
/// alternates of one entity ("12,50/12 euro 50").
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Entities {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub numbers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<String>,
}

/// One line of `manifest.jsonl`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    pub id: String,
    /// Path of the WAV, relative to the dataset directory.
    pub audio: String,
    /// What was actually said. Empty for non-speech clips.
    pub reference: String,
    /// Only verified clips are scored. Prompt-script clips are verified when
    /// the speaker keeps the take; kept dictations start unverified.
    pub verified: bool,
    pub expected: Expected,
    pub split: Split,
    pub categories: Vec<String>,
    /// Language of the speech itself: "nl", "en" or "nl-en" (mixed).
    pub language: String,
    /// App language mode active at capture time, for kept dictations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_mode: Option<String>,
    /// "prompt_script" | "kept_dictation" | "synthetic"
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    pub speaker: String,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_ms: u64,
    pub peak_amplitude: f32,
    /// Speaking/capture condition: "normal", "quiet", "far", "noisy",
    /// "other_mic", "clipping", …
    pub condition: String,
    /// Free-text noise description ("quiet room", "café", "fan on").
    #[serde(default)]
    pub noise: String,
    #[serde(default)]
    pub entities: Entities,
    /// What the app produced when this dictation was kept — model output, kept
    /// apart from the verified reference and never treated as ground truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_transcript: Option<String>,
    /// Model that produced `raw_transcript`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_model: Option<String>,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

/// `--data-dir`, else `FT_EVAL_DIR`, else the app data directory.
pub fn resolve_data_dir(cli_override: Option<&str>) -> Result<PathBuf, String> {
    if let Some(dir) = cli_override.filter(|d| !d.trim().is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("FT_EVAL_DIR") {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    default_data_dir()
}

pub fn default_data_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("FlowingThoughts")
        .join("eval"))
}

pub fn ensure_layout(data_dir: &Path) -> Result<(), String> {
    for dir in [
        data_dir.to_path_buf(),
        data_dir.join(AUDIO_DIR),
        data_dir.join(RESULTS_DIR),
    ] {
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    }
    Ok(())
}

pub fn load(data_dir: &Path) -> Result<Vec<Clip>, String> {
    let path = data_dir.join(MANIFEST_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    parse_manifest(&content)
}

pub fn parse_manifest(content: &str) -> Result<Vec<Clip>, String> {
    let mut clips = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let clip: Clip = serde_json::from_str(line)
            .map_err(|e| format!("manifest.jsonl line {}: {e}", index + 1))?;
        clips.push(clip);
    }
    Ok(clips)
}

/// Append one clip. A single `write` of one line in append mode, so the app
/// (kept dictations) and the recorder CLI can both add clips safely.
pub fn append(data_dir: &Path, clip: &Clip) -> Result<(), String> {
    ensure_layout(data_dir)?;
    let mut line =
        serde_json::to_string(clip).map_err(|e| format!("Failed to encode clip: {e}"))?;
    line.push('\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join(MANIFEST_FILE))
        .map_err(|e| format!("Failed to open manifest: {e}"))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("Failed to append to manifest: {e}"))
}

/// Replace the whole manifest (after verifying or deleting clips). Written to
/// a temp file first so a crash cannot truncate the dataset index.
pub fn rewrite(data_dir: &Path, clips: &[Clip]) -> Result<(), String> {
    ensure_layout(data_dir)?;
    let mut body = String::new();
    for clip in clips {
        body.push_str(
            &serde_json::to_string(clip).map_err(|e| format!("Failed to encode clip: {e}"))?,
        );
        body.push('\n');
    }
    let tmp = data_dir.join(format!("{MANIFEST_FILE}.tmp"));
    fs::write(&tmp, body).map_err(|e| format!("Failed to write manifest: {e}"))?;
    fs::rename(&tmp, data_dir.join(MANIFEST_FILE))
        .map_err(|e| format!("Failed to replace manifest: {e}"))
}

/// Share of clips held out for the test split.
pub const TEST_PERCENT: u64 = 20;

/// Changing this reshuffles every future assignment — don't, once clips exist.
/// (Splits already written to a manifest are frozen and unaffected.) The
/// value was picked once, before any clip existed, as the first salt that put
/// 15–26% of every category of the committed prompt script in test.
const SPLIT_SALT: &str = "flowing-thoughts-eval-split-v17";

/// Deterministic dev/test assignment.
///
/// The key is the prompt id for prompt-script clips — so every take of one
/// sentence (quiet, noisy, other mic) lands in the same split and the test
/// set never contains a sentence tuned against in dev — and the clip id for
/// everything else. Hashing each key independently means adding clips never
/// moves existing ones, and every category gets ~20% test on its own
/// (`prompts::tests` checks the committed script stays balanced per category).
pub fn assign_split(group_key: &str) -> Split {
    let digest = Sha256::digest(format!("{SPLIT_SALT}:{group_key}").as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    if u64::from_be_bytes(first) % 100 < TEST_PERCENT {
        Split::Test
    } else {
        Split::Dev
    }
}

/// Duration, peak and format of a WAV, computed the way `audio.rs` reports a
/// live capture — the capture gate is applied to these numbers.
#[derive(Debug, Clone, Copy)]
pub struct WavStats {
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_ms: u64,
    pub peak_amplitude: f32,
}

pub fn wav_stats(path: &Path) -> Result<WavStats, String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample != 16 {
        return Err(format!(
            "{}: only 16-bit PCM WAV is supported (got {:?} {}-bit)",
            path.display(),
            spec.sample_format,
            spec.bits_per_sample
        ));
    }
    let mut peak: i32 = 0;
    let mut samples: u64 = 0;
    for sample in reader.samples::<i16>() {
        let sample = sample.map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        peak = peak.max(i32::from(sample).abs());
        samples += 1;
    }
    let frames = samples / u64::from(spec.channels.max(1));
    Ok(WavStats {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        duration_ms: frames * 1000 / u64::from(spec.sample_rate.max(1)),
        peak_amplitude: peak as f32 / f32::from(i16::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_is_deterministic_and_roughly_twenty_percent() {
        assert_eq!(assign_split("short-001"), assign_split("short-001"));
        let test = (0..2000)
            .filter(|i| assign_split(&format!("clip-{i}")) == Split::Test)
            .count();
        assert!((340..=460).contains(&test), "test share was {test}/2000");
    }

    #[test]
    fn adding_clips_never_moves_existing_ones() {
        let before: Vec<Split> = (0..50)
            .map(|i| assign_split(&format!("day-{i:03}")))
            .collect();
        // "Adding" more keys is just hashing more strings; nothing is ranked.
        let _more: Vec<Split> = (50..500)
            .map(|i| assign_split(&format!("day-{i:03}")))
            .collect();
        let after: Vec<Split> = (0..50)
            .map(|i| assign_split(&format!("day-{i:03}")))
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn manifest_round_trips_and_tolerates_missing_optional_fields() {
        let line = r#"{"id":"short-001-normal-ab12","audio":"audio/short-001-normal-ab12.wav","reference":"Ja, dat klopt.","verified":true,"expected":"speech","split":"dev","categories":["short_reply"],"language":"nl","source":"prompt_script","prompt_id":"short-001","speaker":"joost","device":"MacBook Pro Microphone","sample_rate":48000,"channels":1,"duration_ms":1800,"peak_amplitude":0.41,"condition":"normal","recorded_at":"2026-09-17T10:00:00Z"}"#;
        let clips = parse_manifest(&format!("{line}\n\n")).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].expected, Expected::Speech);
        assert_eq!(clips[0].entities, Entities::default());
        assert!(clips[0].raw_transcript.is_none());
        let again = serde_json::to_string(&clips[0]).unwrap();
        assert_eq!(parse_manifest(&again).unwrap()[0].id, clips[0].id);
        assert!(parse_manifest("{not json}").unwrap_err().contains("line 1"));
    }

    #[test]
    fn append_then_rewrite() {
        let dir = std::env::temp_dir().join(format!("ft-eval-manifest-{}", uuid::Uuid::new_v4()));
        let line = r#"{"id":"a","audio":"audio/a.wav","reference":"","verified":false,"expected":"non_speech","split":"test","categories":["non_speech"],"language":"nl","source":"synthetic","speaker":"none","device":"generated","sample_rate":16000,"channels":1,"duration_ms":1000,"peak_amplitude":0.0,"condition":"normal","recorded_at":"2026-09-17T10:00:00Z"}"#;
        let mut clip = parse_manifest(line).unwrap().remove(0);
        append(&dir, &clip).unwrap();
        clip.id = "b".into();
        append(&dir, &clip).unwrap();
        let mut clips = load(&dir).unwrap();
        assert_eq!(
            clips.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        clips.retain(|c| c.id == "b");
        rewrite(&dir, &clips).unwrap();
        assert_eq!(load(&dir).unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
