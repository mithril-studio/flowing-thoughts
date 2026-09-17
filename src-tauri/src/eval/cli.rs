//! Command-line front end of the eval tooling (`examples/ft_eval.rs`).

use super::harness::{self, Resampler, RunConfig, VocabVariant};
use super::manifest::{self, Split};
use super::record::RecordOptions;
use super::report::{self, Report};
use super::{mine, prompts, record, synth};
use crate::model_manager::{self, ModelId};
use std::collections::BTreeMap;
use std::io::Write;

pub const USAGE: &str = "\
FlowingThoughts eval — measure local transcription quality. Runs fully offline.

USAGE: npm run eval -- <command> [options]

COMMANDS
  record    Read prompts aloud and record clips into the dataset
  run       Score the dataset through the real dictation pipeline
  verify    Confirm what was said in kept dictations (unverified clips)
  stats     Show what the dataset holds
  mine      Suggest new prompts from the correction history (suggestions only)
  synth     Build a synthetic TTS fixture dataset (needs --data-dir)

COMMON
  --data-dir <dir>      Dataset directory (default: $FT_EVAL_DIR, else
                        ~/Library/Application Support/FlowingThoughts/eval)

record
  --speaker <name>      Speaker id (default: owner)
  --condition <name>    normal | quiet | far | noisy | other_mic | clipping (default: normal)
  --noise <text>        Free-text noise description, e.g. \"fan on\"
  --device <label>      Override the detected microphone name
  --category <name>     Only prompts of one category
  --sample <n>          Only ~n prompts, spread over the categories (for condition passes)
  --prompts <file>      Another prompt script (default: the committed eval/prompts-nl.txt)

run
  --model <ids>         Comma list: base | small | turbo | any model id or custom
                        ggml filename stem (default: best installed)
  --language <modes>    Comma list of nl | auto | en (default: nl)
  --vocab <variants>    Comma list of none | personal | developer | both (default: developer)
  --vad <on,off>        Comma list (default: on)
  --resampler <kinds>   Comma list of app | afconvert (default: app)
  --category <name>     Only clips with this category tag
  --condition <name>    Only clips recorded under this condition
  --limit <n>           Only the first n clips
  --held-out            Score the held-out TEST split instead of dev. Aggregates
                        only, and every use is logged in test-runs.log.

mine
  --limit <n>           Number of suggestions (default: 40)
";

struct Args {
    flags: BTreeMap<String, String>,
}

impl Args {
    fn parse(raw: &[String], switches: &[&str]) -> Result<Self, String> {
        let mut flags = BTreeMap::new();
        let mut iter = raw.iter();
        while let Some(arg) = iter.next() {
            let name = arg
                .strip_prefix("--")
                .ok_or_else(|| format!("Unexpected argument {arg:?}"))?;
            if switches.contains(&name) {
                flags.insert(name.to_string(), "true".to_string());
            } else {
                let value = iter.next().ok_or_else(|| format!("--{name} needs a value"))?;
                flags.insert(name.to_string(), value.clone());
            }
        }
        Ok(Args { flags })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }

    fn list(&self, name: &str, default: &str) -> Vec<String> {
        self.get(name)
            .unwrap_or(default)
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn number(&self, name: &str) -> Result<Option<usize>, String> {
        self.get(name)
            .map(|v| v.parse::<usize>().map_err(|_| format!("--{name} must be a number")))
            .transpose()
    }

    fn reject_unknown(&self, known: &[&str]) -> Result<(), String> {
        match self.flags.keys().find(|k| !known.contains(&k.as_str())) {
            Some(unknown) => Err(format!("Unknown option --{unknown}")),
            None => Ok(()),
        }
    }
}

pub fn parse_model(name: &str) -> Result<ModelId, String> {
    let id = match name {
        "base" => "whisper-base-q5",
        "small" => "whisper-small-q5",
        "turbo" | "large-v3-turbo" => "whisper-large-v3-turbo-q5",
        other => other,
    };
    ModelId::from_str(id).ok_or_else(|| format!("Unknown model {name:?}"))
}

fn default_model() -> Result<String, String> {
    [ModelId::LargeV3TurboQ5, ModelId::SmallQ5, ModelId::BaseQ5]
        .into_iter()
        .find(|id| model_manager::model_path(id).map(|p| p.exists()).unwrap_or(false))
        .map(|id| id.id())
        .ok_or_else(|| {
            "No multilingual model installed. Download one in the app (Settings → Transcription) first.".to_string()
        })
}

/// Cartesian product of the comparison axes, in the order given.
pub fn expand_configs(
    models: &[String],
    languages: &[String],
    vocabs: &[String],
    vads: &[String],
    resamplers: &[String],
) -> Result<Vec<RunConfig>, String> {
    let mut configs = Vec::new();
    for model in models {
        let model_id = parse_model(model)?;
        for language in languages {
            let language_mode = match language.as_str() {
                "nl" => "nl",
                "en" => "en",
                "auto" | "system" => "system",
                other => return Err(format!("Unknown language mode {other:?} (nl | auto | en)")),
            };
            for vocab in vocabs {
                let vocab = VocabVariant::parse(vocab)
                    .ok_or_else(|| format!("Unknown vocab variant {vocab:?}"))?;
                for vad in vads {
                    let use_vad = match vad.as_str() {
                        "on" => true,
                        "off" => false,
                        other => return Err(format!("--vad takes on/off, got {other:?}")),
                    };
                    for resampler in resamplers {
                        let resampler = Resampler::parse(resampler)
                            .ok_or_else(|| format!("Unknown resampler {resampler:?}"))?;
                        configs.push(RunConfig {
                            model: model_id.id(),
                            language_mode: language_mode.to_string(),
                            vocab,
                            use_vad,
                            resampler,
                        });
                    }
                }
            }
        }
    }
    Ok(configs)
}

/// `default_prompts` is the committed script, embedded by the example binary.
pub fn main(raw_args: &[String], default_prompts: &str) -> Result<(), String> {
    let Some((command, rest)) = raw_args.split_first() else {
        println!("{USAGE}");
        return Ok(());
    };
    match command.as_str() {
        "record" => cmd_record(rest, default_prompts),
        "run" => cmd_run(rest),
        "verify" => {
            let args = Args::parse(rest, &[])?;
            args.reject_unknown(&["data-dir"])?;
            record::run_verify(&manifest::resolve_data_dir(args.get("data-dir"))?)
        }
        "stats" => cmd_stats(rest),
        "mine" => {
            let args = Args::parse(rest, &[])?;
            args.reject_unknown(&["data-dir", "limit"])?;
            let data_dir = manifest::resolve_data_dir(args.get("data-dir"))?;
            let (path, n) = mine::run(&data_dir, args.number("limit")?.unwrap_or(40))?;
            println!("{n} suggestion(s) written to {}\nReview them, then: npm run eval -- record --prompts \"{}\"", path.display(), path.display());
            Ok(())
        }
        "synth" => {
            let args = Args::parse(rest, &[])?;
            args.reject_unknown(&["data-dir"])?;
            let dir = args
                .get("data-dir")
                .ok_or("synth needs an explicit --data-dir so it can never land in your real dataset")?;
            let n = synth::build(std::path::Path::new(dir), &prompts::parse(default_prompts)?)?;
            println!("Synthetic fixture: {n} clips in {dir}\nScore it with: npm run eval -- run --data-dir \"{dir}\"");
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("Unknown command {other:?}\n\n{USAGE}")),
    }
}

fn cmd_record(rest: &[String], default_prompts: &str) -> Result<(), String> {
    let args = Args::parse(rest, &[])?;
    args.reject_unknown(&[
        "data-dir", "speaker", "condition", "noise", "device", "category", "sample", "prompts",
    ])?;
    let script = match args.get("prompts") {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("Failed to read {path}: {e}"))?,
        None => default_prompts.to_string(),
    };
    let options = RecordOptions {
        speaker: args.get("speaker").unwrap_or("owner").to_string(),
        condition: args.get("condition").unwrap_or("normal").to_string(),
        noise: args.get("noise").unwrap_or("").to_string(),
        device_label: args.get("device").map(str::to_string),
        category: args.get("category").map(str::to_string),
        sample: args.number("sample")?,
    };
    record::run_recorder(
        &manifest::resolve_data_dir(args.get("data-dir"))?,
        &prompts::parse(&script)?,
        &options,
    )
}

fn cmd_stats(rest: &[String]) -> Result<(), String> {
    let args = Args::parse(rest, &[])?;
    args.reject_unknown(&["data-dir"])?;
    let data_dir = manifest::resolve_data_dir(args.get("data-dir"))?;
    let clips = manifest::load(&data_dir)?;
    // category -> (dev, test, unverified)
    let mut per_category: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    let mut per_condition: BTreeMap<String, usize> = BTreeMap::new();
    let mut audio_ms = 0u64;
    for clip in &clips {
        audio_ms += clip.duration_ms;
        *per_condition.entry(clip.condition.clone()).or_default() += 1;
        for category in &clip.categories {
            let entry = per_category.entry(category.clone()).or_default();
            if !clip.verified {
                entry.2 += 1;
            } else if clip.split == Split::Dev {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
    }
    println!(
        "{}\n{} clips · {:.1} min of audio\n",
        data_dir.display(),
        clips.len(),
        audio_ms as f64 / 60_000.0
    );
    let rows: Vec<Vec<String>> = per_category
        .iter()
        .map(|(c, (dev, test, unverified))| {
            vec![c.clone(), dev.to_string(), test.to_string(), unverified.to_string()]
        })
        .collect();
    println!("{}", report::table(&["category", "dev", "test", "unverified"], &rows));
    let rows: Vec<Vec<String>> = per_condition
        .iter()
        .map(|(c, n)| vec![c.clone(), n.to_string()])
        .collect();
    println!("{}", report::table(&["condition", "clips"], &rows));
    Ok(())
}

fn cmd_run(rest: &[String]) -> Result<(), String> {
    let args = Args::parse(rest, &["held-out"])?;
    args.reject_unknown(&[
        "data-dir", "model", "language", "vocab", "vad", "resampler", "category", "condition",
        "limit", "held-out",
    ])?;
    let data_dir = manifest::resolve_data_dir(args.get("data-dir"))?;
    let split = if args.get("held-out").is_some() { Split::Test } else { Split::Dev };
    let default_model = match args.get("model") {
        Some(_) => String::new(),
        None => default_model()?,
    };
    let configs = expand_configs(
        &args.list("model", &default_model),
        &args.list("language", "nl"),
        &args.list("vocab", "developer"),
        &args.list("vad", "on"),
        &args.list("resampler", "app"),
    )?;

    let (clips, unverified) = harness::select_clips(
        manifest::load(&data_dir)?,
        split,
        args.get("category"),
        args.get("condition"),
        args.number("limit")?,
    );
    if clips.is_empty() {
        return Err(format!(
            "No verified {} clips in {} — record some first: npm run eval -- record",
            split.as_str(),
            data_dir.display()
        ));
    }
    if !model_manager::vad_model_installed() {
        eprintln!("WARNING: VAD model not installed — results will not match the app with VAD on.");
    }

    crate::local_transcribe::quiet_native_logging();
    let mut runs = Vec::new();
    for (index, config) in configs.iter().enumerate() {
        eprintln!("[{}/{}] {} — {} clips", index + 1, configs.len(), config.label(), clips.len());
        let model = parse_model(&config.model)?;
        let load_started = std::time::Instant::now();
        crate::local_transcribe::preload_model(&model)?;
        let model_load_ms = load_started.elapsed().as_millis() as u64;
        let transcribe = harness::local_transcriber(model);
        let mut run = harness::run_config(&clips, &data_dir, config, &transcribe, |i, _| {
            eprint!("\r  {}/{}", i + 1, clips.len());
            let _ = std::io::stderr().flush();
        })?;
        eprintln!();
        run.model_load_ms = model_load_ms;
        runs.push(run);
    }

    let mut report = Report {
        schema_version: report::SCHEMA_VERSION,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: report::git_commit(),
        data_dir: data_dir.to_string_lossy().into_owned(),
        split,
        clips_scored: clips.len(),
        unverified_skipped: unverified,
        runs,
    };
    report.strip_clip_detail_if_held_out();
    if split == Split::Test {
        report::log_held_out_run(&report, &data_dir)?;
    }
    let (json_path, md_path) = report::write(&report, &data_dir)?;
    println!("{}", report::render_markdown(&report));
    println!("Results: {}\nSummary: {}", json_path.display(), md_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn axes_expand_to_every_combination() {
        let configs = expand_configs(
            &strings(&["small", "turbo"]),
            &strings(&["nl", "auto"]),
            &strings(&["none", "developer"]),
            &strings(&["on"]),
            &strings(&["app"]),
        )
        .unwrap();
        assert_eq!(configs.len(), 8);
        assert_eq!(configs[0].label(), "whisper-small-q5 · nl · vocab=none");
        assert_eq!(configs[7].label(), "whisper-large-v3-turbo-q5 · auto · vocab=developer");
        assert!(expand_configs(&strings(&["small"]), &strings(&["de"]), &[], &[], &[]).is_err());
        assert_eq!(parse_model("ggml-medium-q5_0").unwrap().id(), "ggml-medium-q5_0");
    }

    #[test]
    fn held_out_is_a_switch_and_unknown_flags_fail() {
        let args = Args::parse(&strings(&["--held-out", "--limit", "5"]), &["held-out"]).unwrap();
        assert!(args.get("held-out").is_some());
        assert_eq!(args.number("limit").unwrap(), Some(5));
        assert!(args.reject_unknown(&["held-out"]).is_err());
        assert!(Args::parse(&strings(&["--limit"]), &[]).is_err());
    }
}
