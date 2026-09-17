//! Terminal recorder: shows the next prompt, records through the app's own
//! capture path (`audio.rs`), and appends the clip to the manifest.
//!
//! Also hosts `verify`, which turns kept dictations (unverified) into scored
//! clips once a human has confirmed what was really said.

use super::manifest::{self, Clip, Entities, Expected, AUDIO_DIR};
use super::prompts::Prompt;
use crate::audio;
use std::io::{BufRead, Write};
use std::path::Path;

pub struct RecordOptions {
    pub speaker: String,
    pub condition: String,
    pub noise: String,
    /// Overrides the detected input device name in the manifest.
    pub device_label: Option<String>,
    pub category: Option<String>,
    /// Record only a deterministic, category-stratified sample of this many
    /// prompts — for the condition passes (quiet, far, noisy, other mic).
    pub sample: Option<usize>,
}

fn read_line(stdin: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = String::new();
    let n = stdin
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read input: {e}"))?;
    Ok((n > 0).then(|| line.trim().to_string()))
}

fn say(text: &str) {
    println!("{text}");
    let _ = std::io::stdout().flush();
}

fn play(path: &Path) {
    let _ = std::process::Command::new("afplay").arg(path).status();
}

/// Prompts still to record for this speaker + condition, in script order.
/// With `sample`, every k-th prompt of each category is taken, so the sample
/// stays spread over the categories and stable between sessions.
pub fn pending_prompts<'a>(
    prompts: &'a [Prompt],
    clips: &[Clip],
    options: &RecordOptions,
) -> Vec<&'a Prompt> {
    let in_scope: Vec<&Prompt> = prompts
        .iter()
        .filter(|p| options.category.as_deref().is_none_or(|c| p.category == c))
        .collect();
    let sampled: Vec<&Prompt> = match options.sample {
        Some(n) if n > 0 && n < in_scope.len() => {
            let stride = in_scope.len().div_ceil(n);
            let mut seen_in_category: std::collections::HashMap<&str, usize> = Default::default();
            in_scope
                .into_iter()
                .filter(|p| {
                    let index = seen_in_category.entry(p.category.as_str()).or_default();
                    *index += 1;
                    (*index - 1) % stride == 0
                })
                .collect()
        }
        _ => in_scope,
    };
    sampled
        .into_iter()
        .filter(|p| {
            !clips.iter().any(|c| {
                c.prompt_id.as_deref() == Some(p.id.as_str())
                    && c.condition == options.condition
                    && c.speaker == options.speaker
            })
        })
        .collect()
}

pub fn run_recorder(data_dir: &Path, prompts: &[Prompt], options: &RecordOptions) -> Result<(), String> {
    manifest::ensure_layout(data_dir)?;
    let existing = manifest::load(data_dir)?;
    let pending = pending_prompts(prompts, &existing, options);
    let device = options
        .device_label
        .clone()
        .unwrap_or_else(audio::default_input_device_name);

    say(&format!(
        "Dataset: {}\nMicrophone: {device}\nSpeaker: {} · condition: {} · {} prompt(s) to go ({} clips already in the dataset)\n",
        data_dir.display(),
        options.speaker,
        options.condition,
        pending.len(),
        existing.len(),
    ));
    say("Hold nothing: Enter starts a take, Enter again stops it. Speak the way you dictate.\n");

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let mut take: u64 = 0;
    let mut kept = 0usize;
    'prompts: for (index, prompt) in pending.iter().enumerate() {
        let mut reference = if prompt.is_non_speech() {
            String::new()
        } else {
            prompt.text.clone()
        };
        loop {
            say(&format!(
                "[{}/{}] {} · {}\n\n    {}\n",
                index + 1,
                pending.len(),
                prompt.category,
                prompt.id,
                prompt.text
            ));
            say("Enter = record · s = skip · q = quit");
            match read_line(&mut stdin)?.as_deref() {
                None | Some("q") => break 'prompts,
                Some("s") => continue 'prompts,
                _ => {}
            }
            let recording = audio::start_recording()?;
            say("● recording — Enter to stop");
            let _ = read_line(&mut stdin)?;
            take += 1;
            // A failed take (no samples yet from a Bluetooth mic, microphone
            // permission missing) must not end the session.
            let capture = match audio::stop_and_finalize(recording, take) {
                Ok(capture) => capture,
                Err(e) => {
                    say(&format!(
                        "  Take failed: {e}. Check that your terminal has microphone access (System Settings → Privacy & Security → Microphone), then try again.\n"
                    ));
                    continue;
                }
            };
            let stats = manifest::wav_stats(&capture.wav_path)?;

            loop {
                let warning = if crate::pipeline::is_capture_discarded(stats.duration_ms, stats.peak_amplitude)
                    && !prompt.is_non_speech()
                {
                    " — WARNING: too short/quiet, the app would discard this capture"
                } else if stats.peak_amplitude >= 0.999 && options.condition != "clipping" {
                    " — WARNING: clipped"
                } else {
                    ""
                };
                say(&format!(
                    "  {:.1}s, peak {:.2}{warning}\n  Enter = keep · r = re-record · p = play back · e = I said something else (edit reference) · d = discard",
                    stats.duration_ms as f64 / 1000.0,
                    stats.peak_amplitude,
                ));
                match read_line(&mut stdin)?.as_deref() {
                    Some("p") => play(&capture.wav_path),
                    Some("e") => {
                        say("  Type exactly what you said, then Enter:");
                        if let Some(text) = read_line(&mut stdin)?.filter(|t| !t.is_empty()) {
                            reference = text;
                        }
                    }
                    Some("r") => {
                        let _ = std::fs::remove_file(&capture.wav_path);
                        break;
                    }
                    Some("d") => {
                        let _ = std::fs::remove_file(&capture.wav_path);
                        continue 'prompts;
                    }
                    None | Some("") => {
                        let id = format!(
                            "{}-{}-{}",
                            prompt.id,
                            options.condition,
                            &uuid::Uuid::new_v4().simple().to_string()[..8]
                        );
                        let audio_rel = format!("{AUDIO_DIR}/{id}.wav");
                        move_file(&capture.wav_path, &data_dir.join(&audio_rel))?;
                        let clip =
                            build_clip(prompt, &reference, options, &device, id, audio_rel, stats);
                        manifest::append(data_dir, &clip)?;
                        kept += 1;
                        continue 'prompts;
                    }
                    _ => {}
                }
            }
        }
    }
    say(&format!(
        "\nKept {kept} clip(s) this session. Dataset now holds {} clips.",
        existing.len() + kept
    ));
    Ok(())
}

/// Manifest entry for a kept take of `prompt`.
fn build_clip(
    prompt: &Prompt,
    reference: &str,
    options: &RecordOptions,
    device: &str,
    id: String,
    audio: String,
    stats: manifest::WavStats,
) -> Clip {
    Clip {
        id,
        audio,
        reference: reference.to_string(),
        verified: true,
        expected: if prompt.is_non_speech() {
            Expected::NonSpeech
        } else {
            Expected::Speech
        },
        // Keyed on the prompt, not the take: every condition variant of one
        // sentence shares a split.
        split: manifest::assign_split(&prompt.id),
        categories: vec![prompt.category.clone()],
        language: language_of(prompt),
        language_mode: None,
        source: "prompt_script".to_string(),
        prompt_id: Some(prompt.id.clone()),
        speaker: options.speaker.clone(),
        device: device.to_string(),
        sample_rate: stats.sample_rate,
        channels: stats.channels,
        duration_ms: stats.duration_ms,
        peak_amplitude: stats.peak_amplitude,
        condition: options.condition.clone(),
        noise: options.noise.clone(),
        // An edited reference no longer contains the prompt's entities for
        // certain; keep only those it still has.
        entities: retain_present(&prompt.entities, reference),
        raw_transcript: None,
        raw_model: None,
        recorded_at: chrono::Utc::now().to_rfc3339(),
        notes: String::new(),
    }
}

fn language_of(prompt: &Prompt) -> String {
    if prompt.category == "mixed_tech" {
        "nl-en".to_string()
    } else {
        "nl".to_string()
    }
}

fn retain_present(entities: &Entities, reference: &str) -> Entities {
    let tokens = super::normalize::normalize(reference);
    let keep = |list: &[String]| -> Vec<String> {
        list.iter()
            .filter(|e| super::score::entity_found(e, &tokens))
            .cloned()
            .collect()
    };
    Entities {
        names: keep(&entities.names),
        numbers: keep(&entities.numbers),
        terms: keep(&entities.terms),
    }
}

/// Rename, falling back to copy + delete when the temp dir is another volume.
pub fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(|e| format!("Failed to store {}: {e}", to.display()))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// Walk the unverified clips (kept dictations): listen, then confirm or type
/// what was really said. Until then a clip is never scored.
pub fn run_verify(data_dir: &Path) -> Result<(), String> {
    let mut clips = manifest::load(data_dir)?;
    let todo: Vec<usize> = (0..clips.len()).filter(|&i| !clips[i].verified).collect();
    say(&format!("{} unverified clip(s) in {}\n", todo.len(), data_dir.display()));
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let mut deleted: Vec<usize> = Vec::new();
    let mut verified = 0usize;
    'clips: for (n, &i) in todo.iter().enumerate() {
        let wav = data_dir.join(&clips[i].audio);
        let suggestion = clips[i].raw_transcript.clone().unwrap_or_default();
        say(&format!(
            "[{}/{}] {} · {:.1}s\n  app heard: {suggestion:?}",
            n + 1,
            todo.len(),
            clips[i].id,
            clips[i].duration_ms as f64 / 1000.0,
        ));
        play(&wav);
        loop {
            say("  Enter = that is exactly what I said · e = type what I said · n = no speech · p = play again · x = delete clip · s = skip · q = quit");
            match read_line(&mut stdin)?.as_deref() {
                None | Some("q") => break 'clips,
                Some("s") => continue 'clips,
                Some("p") => play(&wav),
                Some("x") => {
                    let _ = std::fs::remove_file(&wav);
                    deleted.push(i);
                    continue 'clips;
                }
                Some("n") => {
                    clips[i].reference.clear();
                    clips[i].expected = Expected::NonSpeech;
                    clips[i].categories = vec!["non_speech".to_string(), "real_world".to_string()];
                }
                Some("e") => {
                    say("  Type exactly what you said, then Enter:");
                    match read_line(&mut stdin)?.filter(|t| !t.is_empty()) {
                        Some(text) => clips[i].reference = text,
                        None => continue,
                    }
                }
                Some("") => {
                    if suggestion.trim().is_empty() {
                        say("  The app heard nothing — use e or n.");
                        continue;
                    }
                    clips[i].reference = suggestion.trim().to_string();
                }
                _ => continue,
            }
            clips[i].verified = true;
            verified += 1;
            continue 'clips;
        }
    }
    let remaining: Vec<Clip> = clips
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !deleted.contains(i))
        .map(|(_, c)| c)
        .collect();
    manifest::rewrite(data_dir, &remaining)?;
    say(&format!("\nVerified {verified}, deleted {}.", deleted.len()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::prompts;

    fn options(sample: Option<usize>) -> RecordOptions {
        RecordOptions {
            speaker: "joost".into(),
            condition: "normal".into(),
            noise: String::new(),
            device_label: None,
            category: None,
            sample,
        }
    }

    #[test]
    fn sample_is_spread_over_categories_and_stable() {
        let mut script = String::new();
        for category in ["a", "b"] {
            script.push_str(&format!("## category: {category}\n"));
            for i in 0..10 {
                script.push_str(&format!("{category}-{i:03} | Zin {i}.\n"));
            }
        }
        let prompts = prompts::parse(&script).unwrap();
        let sample = pending_prompts(&prompts, &[], &options(Some(5)));
        let ids: Vec<&str> = sample.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a-000", "a-004", "a-008", "b-000", "b-004", "b-008"]);
        assert_eq!(pending_prompts(&prompts, &[], &options(None)).len(), 20);
    }

    #[test]
    fn edited_reference_keeps_only_entities_it_still_contains() {
        let entities = Entities {
            names: vec!["Annelies".into(), "Utrecht".into()],
            numbers: vec!["15".into()],
            terms: vec![],
        };
        let kept = retain_present(&entities, "Annelies komt om vijftien uur.");
        assert_eq!(kept.names, ["Annelies"]);
        assert_eq!(kept.numbers, ["15"]);
    }

    #[test]
    fn takes_of_one_prompt_share_a_split_and_non_speech_has_no_reference() {
        let prompts = prompts::parse(
            "## category: numbers\nnum-001 | Het kost 25 euro. | numbers: 25\n## category: non_speech\nns-001 | [Geen spraak] Blijf stil.\n",
        )
        .unwrap();
        let stats = manifest::WavStats {
            sample_rate: 48_000,
            channels: 1,
            duration_ms: 2_000,
            peak_amplitude: 0.4,
        };
        let mut quiet = options(None);
        quiet.condition = "quiet".into();
        let normal = build_clip(&prompts[0], &prompts[0].text, &options(None), "mic", "a".into(), "audio/a.wav".into(), stats);
        let variant = build_clip(&prompts[0], &prompts[0].text, &quiet, "mic", "b".into(), "audio/b.wav".into(), stats);
        assert_eq!(normal.split, variant.split);
        assert_eq!(normal.entities.numbers, ["25"]);
        assert!(normal.verified);

        let silence = build_clip(&prompts[1], "", &options(None), "mic", "c".into(), "audio/c.wav".into(), stats);
        assert_eq!(silence.expected, Expected::NonSpeech);
        assert!(silence.reference.is_empty());
    }
}
