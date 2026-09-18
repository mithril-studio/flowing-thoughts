//! Opt-in "keep this dictation for evaluation".
//!
//! Off by default (`extras.keep_audio_for_eval`). When on, the live session
//! calls `keep_dictation` just before it deletes the capture: the WAV is
//! copied into the eval dataset with the app's raw transcript, as an
//! *unverified* clip. It is never scored until `verify` confirms what was
//! really said — model output is not ground truth. Everything stays on disk,
//! local; nothing here talks to the network.

use super::manifest::{self, Clip, Entities, Expected, AUDIO_DIR};
use std::path::Path;

pub struct KeptDictation<'a> {
    pub wav_path: &'a Path,
    /// Raw model output, before the text filters. `None` when transcription failed.
    pub raw_transcript: Option<&'a str>,
    pub model: &'a str,
    pub language_mode: &'a str,
}

pub fn keep_dictation(kept: &KeptDictation) -> Result<String, String> {
    keep_dictation_in(&manifest::default_data_dir()?, kept)
}

fn keep_dictation_in(data_dir: &Path, kept: &KeptDictation) -> Result<String, String> {
    manifest::ensure_layout(data_dir)?;
    let stats = manifest::wav_stats(kept.wav_path)?;
    let id = format!(
        "kept-{}-{}",
        chrono::Utc::now().format("%Y%m%d"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let audio_rel = format!("{AUDIO_DIR}/{id}.wav");
    std::fs::copy(kept.wav_path, data_dir.join(&audio_rel))
        .map_err(|e| format!("Failed to keep audio for evaluation: {e}"))?;
    let clip = Clip {
        split: manifest::assign_split(&id),
        id: id.clone(),
        audio: audio_rel,
        reference: String::new(),
        verified: false,
        expected: Expected::Speech,
        categories: vec!["real_world".to_string()],
        language: "nl".to_string(),
        language_mode: Some(kept.language_mode.to_string()),
        source: "kept_dictation".to_string(),
        prompt_id: None,
        speaker: "owner".to_string(),
        device: crate::audio::default_input_device_name(),
        sample_rate: stats.sample_rate,
        channels: stats.channels,
        duration_ms: stats.duration_ms,
        peak_amplitude: stats.peak_amplitude,
        condition: "real_world".to_string(),
        noise: String::new(),
        entities: Entities::default(),
        raw_transcript: kept.raw_transcript.map(str::to_string),
        raw_model: Some(kept.model.to_string()),
        recorded_at: chrono::Utc::now().to_rfc3339(),
        notes: String::new(),
    };
    manifest::append(data_dir, &clip)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_dictations_start_unverified_with_the_raw_transcript_apart() {
        let dir = std::env::temp_dir().join(format!("ft-eval-keep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("session.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..48_000 {
            writer
                .write_sample(if i % 2 == 0 { 8_000i16 } else { -8_000 })
                .unwrap();
        }
        writer.finalize().unwrap();

        let id = keep_dictation_in(
            &dir,
            &KeptDictation {
                wav_path: &wav,
                raw_transcript: Some("Ja dat klopt"),
                model: "whisper-small-q5",
                language_mode: "system",
            },
        )
        .unwrap();

        let clips = manifest::load(&dir).unwrap();
        assert_eq!(clips.len(), 1);
        let clip = &clips[0];
        assert_eq!(clip.id, id);
        assert!(
            !clip.verified,
            "model output must never count as ground truth"
        );
        assert!(clip.reference.is_empty());
        assert_eq!(clip.raw_transcript.as_deref(), Some("Ja dat klopt"));
        assert_eq!((clip.sample_rate, clip.duration_ms), (48_000, 1_000));
        assert!(dir.join(&clip.audio).exists());
        assert!(
            wav.exists(),
            "the live session still owns (and deletes) its capture"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
