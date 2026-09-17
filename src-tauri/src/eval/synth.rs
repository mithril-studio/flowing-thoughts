//! Synthetic fixture dataset: proves the harness end to end without anyone's
//! voice. Speech comes from the macOS `say` Dutch voice, non-speech clips are
//! generated here. Nothing is committed — the fixture is rebuilt on demand —
//! and TTS scores say nothing about real dictation quality.

use super::manifest::{self, Clip, Expected, Split, AUDIO_DIR};
use super::prompts::Prompt;
use std::path::Path;

const TTS_VOICE: &str = "Xander";
/// Not 16 kHz on purpose, so the app's resampler is part of the fixture run.
const TTS_FORMAT: &str = "LEI16@22050";
/// Prompts taken per category.
const PER_CATEGORY: usize = 2;

fn write_generated(path: &Path, ms: u32, mut sample: impl FnMut(u32) -> i16) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| format!("Failed to create {}: {e}", path.display()))?;
    for i in 0..(16 * ms) {
        writer.write_sample(sample(i)).map_err(|e| format!("Failed to write sample: {e}"))?;
    }
    writer.finalize().map_err(|e| format!("Failed to finalize wav: {e}"))
}

fn tts(text: &str, out: &Path) -> Result<(), String> {
    let status = std::process::Command::new("say")
        .args(["-v", TTS_VOICE, "--file-format=WAVE", &format!("--data-format={TTS_FORMAT}"), "-o"])
        .arg(out)
        .arg(text)
        .status()
        .map_err(|e| format!("Failed to run `say`: {e}"))?;
    if !status.success() {
        return Err(format!(
            "`say -v {TTS_VOICE}` failed — install the Dutch voice in System Settings → Accessibility → Spoken Content"
        ));
    }
    Ok(())
}

/// Build the fixture in `data_dir` (which must be empty of clips).
pub fn build(data_dir: &Path, prompts: &[Prompt]) -> Result<usize, String> {
    if !manifest::load(data_dir)?.is_empty() {
        return Err(format!(
            "{} already holds clips — point --data-dir at an empty directory for the synthetic fixture",
            data_dir.display()
        ));
    }
    manifest::ensure_layout(data_dir)?;
    let mut count = 0usize;
    let mut add = |id: String, reference: &str, expected: Expected, prompt: Option<&Prompt>, speaker: &str| -> Result<(), String> {
        let audio = format!("{AUDIO_DIR}/{id}.wav");
        let stats = manifest::wav_stats(&data_dir.join(&audio))?;
        manifest::append(
            data_dir,
            &Clip {
                id,
                audio,
                reference: reference.to_string(),
                verified: true,
                expected,
                // The fixture has no held-out value; keep it all scoreable.
                split: Split::Dev,
                categories: vec![prompt.map(|p| p.category.clone()).unwrap_or_else(|| "non_speech".into())],
                language: "nl".to_string(),
                language_mode: None,
                source: "synthetic".to_string(),
                prompt_id: prompt.map(|p| p.id.clone()),
                speaker: speaker.to_string(),
                device: "generated".to_string(),
                sample_rate: stats.sample_rate,
                channels: stats.channels,
                duration_ms: stats.duration_ms,
                peak_amplitude: stats.peak_amplitude,
                condition: "synthetic".to_string(),
                noise: String::new(),
                entities: prompt.map(|p| p.entities.clone()).unwrap_or_default(),
                raw_transcript: None,
                raw_model: None,
                recorded_at: chrono::Utc::now().to_rfc3339(),
                notes: String::new(),
            },
        )?;
        count += 1;
        Ok(())
    };

    let mut taken: std::collections::HashMap<&str, usize> = Default::default();
    for prompt in prompts.iter().filter(|p| !p.is_non_speech()) {
        let n = taken.entry(prompt.category.as_str()).or_default();
        if *n >= PER_CATEGORY {
            continue;
        }
        *n += 1;
        let id = format!("synth-{}", prompt.id);
        tts(&prompt.text, &data_dir.join(format!("{AUDIO_DIR}/{id}.wav")))?;
        add(id, &prompt.text, Expected::Speech, Some(prompt), &format!("tts-{}", TTS_VOICE.to_lowercase()))?;
    }

    write_generated(&data_dir.join(format!("{AUDIO_DIR}/synth-silence.wav")), 2_000, |_| 0)?;
    add("synth-silence".into(), "", Expected::NonSpeech, None, "none")?;

    // Low-level noise that clears the app's peak gate but carries no speech —
    // the condition that used to produce "And Linux." in the field.
    let mut seed: u32 = 0x1234_5678;
    write_generated(&data_dir.join(format!("{AUDIO_DIR}/synth-noise.wav")), 2_000, |_| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((seed >> 16) as i32 % 651) as i16
    })?;
    add("synth-noise".into(), "", Expected::NonSpeech, None, "none")?;

    // Sharp clicks every 150 ms, a stand-in for typing.
    write_generated(&data_dir.join(format!("{AUDIO_DIR}/synth-clicks.wav")), 3_000, |i| {
        let phase = i % 2_400;
        if phase < 40 { (12_000 - phase as i32 * 300) as i16 * if phase % 2 == 0 { 1 } else { -1 } } else { 0 }
    })?;
    add("synth-clicks".into(), "", Expected::NonSpeech, None, "none")?;
    Ok(count)
}
