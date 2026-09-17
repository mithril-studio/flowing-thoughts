//! Results files: machine-readable JSON plus a readable Markdown summary.

use super::harness::{worst_clips, RunResult, Tally};
use super::manifest::{Split, RESULTS_DIR};
use super::score::{self, Hits};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

/// Number of worst clips listed per configuration.
const WORST_CLIPS: usize = 15;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub created_at: String,
    pub app_version: String,
    pub git_commit: Option<String>,
    pub data_dir: String,
    pub split: Split,
    pub clips_scored: usize,
    pub unverified_skipped: usize,
    pub runs: Vec<RunResult>,
}

impl Report {
    /// Held-out discipline: a test-split report carries aggregates only. With
    /// no per-clip transcripts or diffs there is nothing to tune against.
    pub fn strip_clip_detail_if_held_out(&mut self) {
        if self.split == Split::Test {
            for run in &mut self.runs {
                run.clips.clear();
            }
        }
    }
}

fn pct(value: f64) -> String {
    format!("{:.1}%", value * 100.0)
}

fn pct_opt(value: Option<f64>) -> String {
    value.map(pct).unwrap_or_else(|| "–".to_string())
}

fn hits(h: Hits) -> String {
    match h.rate() {
        Some(rate) => format!("{} ({}/{})", pct(rate), h.hit, h.total),
        None => "–".to_string(),
    }
}

/// Markdown table with padded columns, so it also reads well in a terminal.
pub fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: Vec<String>| {
        let padded: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<width$}", width = widths[i]))
            .collect();
        format!("| {} |\n", padded.join(" | "))
    };
    let mut out = line(header.iter().map(|h| h.to_string()).collect());
    out.push_str(&line(widths.iter().map(|w| "-".repeat(*w)).collect()));
    for row in rows {
        out.push_str(&line(row.clone()));
    }
    out
}

fn tally_row(name: &str, t: &Tally) -> Vec<String> {
    // Error rates only mean something where there was speech to get wrong.
    let rate = |value: f64| if t.speech_clips > 0 { pct(value) } else { "–".to_string() };
    vec![
        name.to_string(),
        (t.speech_clips + t.non_speech_clips).to_string(),
        rate(t.raw.wer()),
        rate(t.final_stage.wer()),
        rate(t.final_stage.lenient_wer()),
        rate(t.final_stage.cer()),
        hits(t.final_stage.names),
        hits(t.final_stage.numbers),
        hits(t.final_stage.terms),
        pct_opt(t.speech_discarded_rate()),
        pct_opt(t.hallucination_rate_final()),
    ]
}

const TALLY_HEADER: [&str; 11] = [
    "",
    "clips",
    "WER raw",
    "WER final",
    "WER final (lenient)",
    "CER final",
    "names",
    "numbers",
    "terms",
    "speech discarded",
    "non-speech typed",
];

fn breakdown(title: &str, groups: &BTreeMap<String, Tally>) -> String {
    let rows: Vec<Vec<String>> = groups.iter().map(|(name, t)| tally_row(name, t)).collect();
    format!("**{title}**\n\n{}\n", table(&TALLY_HEADER, &rows))
}

pub fn render_markdown(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Dutch eval — {} split\n", report.split.as_str());
    let _ = writeln!(
        out,
        "{} · app {} · commit {} · {} clips scored, {} unverified skipped\n",
        report.created_at,
        report.app_version,
        report.git_commit.as_deref().unwrap_or("unknown"),
        report.clips_scored,
        report.unverified_skipped,
    );
    if report.split == Split::Test {
        let _ = writeln!(
            out,
            "> Held-out test split: aggregates only, no per-clip output. Do not tune against this.\n"
        );
    }

    let rows: Vec<Vec<String>> = report
        .runs
        .iter()
        .map(|run| {
            let t = &run.overall;
            let mut row = tally_row(&run.label, t);
            row.extend([
                pct_opt(t.hallucination_rate_raw()),
                format!("{} / {}", t.latency_ms_p50, t.latency_ms_p95),
                t.real_time_factor().map(|r| format!("{r:.2}")).unwrap_or_else(|| "–".into()),
                run.peak_memory_mb.map(|m| format!("{m:.0}")).unwrap_or_else(|| "–".into()),
            ]);
            row
        })
        .collect();
    let mut header: Vec<&str> = TALLY_HEADER.to_vec();
    header[0] = "config";
    header.extend(["non-speech raw", "latency p50 / p95 ms", "RTF", "peak MB"]);
    let _ = writeln!(out, "## Summary\n\n{}", table(&header, &rows));
    let _ = writeln!(
        out,
        "WER/CER are corpus-level over speech clips. *raw* = model output; *final* = after the \
         text filters, i.e. what would be typed. A discarded speech clip counts as all deletions \
         in *final*. *lenient* forgives words written together or apart. *non-speech typed* = \
         hallucinations that survived every filter; *non-speech raw* = before the filters.\n"
    );

    for run in &report.runs {
        let _ = writeln!(out, "## {}\n", run.label);
        let _ = writeln!(
            out,
            "model load {} ms · {} personal terms, {} correction pairs · prompt sha256 {}\n",
            run.model_load_ms,
            run.user_terms,
            run.correction_pairs,
            run.prompt_sha256.as_deref().map(|h| &h[..12]).unwrap_or("none"),
        );
        if !run.overall.speech_discarded_by.is_empty() {
            let reasons: Vec<String> = run
                .overall
                .speech_discarded_by
                .iter()
                .map(|(stage, n)| format!("{stage} {n}"))
                .collect();
            let _ = writeln!(
                out,
                "Real speech discarded: {} of {} clips — {}\n",
                run.overall.speech_discarded,
                run.overall.speech_clips,
                reasons.join(", ")
            );
        }
        if !run.detected_languages.is_empty() {
            let languages: Vec<String> = run
                .detected_languages
                .iter()
                .map(|(language, n)| format!("{language} {n}"))
                .collect();
            let _ = writeln!(
                out,
                "Decoded language: {} · re-decoded as Dutch after an out-of-set detection: {}\n",
                languages.join(", "),
                run.redecoded_as_dutch
            );
        }
        out.push_str(&breakdown("By category", &run.by_category));
        out.push_str(&breakdown("By condition", &run.by_condition));

        let worst = worst_clips(run, WORST_CLIPS);
        if !worst.is_empty() {
            let _ = writeln!(out, "**Worst clips** (final stage)\n");
            for clip in worst {
                let wer = clip.final_score.as_ref().map(|s| s.wer()).unwrap_or(0.0);
                let _ = writeln!(
                    out,
                    "- `{}` [{}] WER {}{}",
                    clip.id,
                    clip.categories.join(", "),
                    pct(wer),
                    clip.discarded_by
                        .as_deref()
                        .map(|stage| format!(" — discarded by **{stage}**"))
                        .unwrap_or_default(),
                );
                if score::is_blank(&clip.final_text) && !score::is_blank(&clip.raw) {
                    let _ = writeln!(out, "  - raw:  {}", score::render_diff(&clip.reference, &clip.raw));
                } else {
                    let _ = writeln!(
                        out,
                        "  - diff: {}",
                        score::render_diff(&clip.reference, &clip.final_text)
                    );
                }
            }
            out.push('\n');
        }
        let typed: Vec<_> = run
            .clips
            .iter()
            .filter(|c| c.reference.is_empty() && !score::is_blank(&c.raw))
            .collect();
        if !typed.is_empty() {
            let _ = writeln!(out, "**Hallucinations on non-speech**\n");
            for clip in typed {
                let fate = match (&clip.discarded_by, score::is_blank(&clip.final_text)) {
                    (Some(stage), true) => format!("caught by {stage}"),
                    _ => "TYPED".to_string(),
                };
                let _ = writeln!(out, "- `{}` {:?} — {fate}", clip.id, clip.raw);
            }
            out.push('\n');
        }
    }
    out
}

/// Write `<stamp>-<split>.json` and `.md` into `results/`; returns both paths.
pub fn write(report: &Report, data_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let dir = data_dir.join(RESULTS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create results dir: {e}"))?;
    let stamp = report.created_at.replace([':', '-'], "");
    let stamp = stamp.split('.').next().unwrap_or(&stamp).trim_end_matches('Z');
    let base = format!("{stamp}-{}", report.split.as_str());
    let json_path = dir.join(format!("{base}.json"));
    let md_path = dir.join(format!("{base}.md"));
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| format!("Failed to encode results: {e}"))?;
    std::fs::write(&json_path, json).map_err(|e| format!("Failed to write results: {e}"))?;
    std::fs::write(&md_path, render_markdown(report))
        .map_err(|e| format!("Failed to write summary: {e}"))?;
    Ok((json_path, md_path))
}

/// Every scoring of the held-out split leaves a line here, so "how often has
/// the test set been looked at?" has an answer.
pub fn log_held_out_run(report: &Report, data_dir: &Path) -> Result<(), String> {
    use std::io::Write;
    let labels: Vec<&str> = report.runs.iter().map(|r| r.label.as_str()).collect();
    let line = format!(
        "{}\tcommit {}\t{} clips\t{}\n",
        report.created_at,
        report.git_commit.as_deref().unwrap_or("unknown"),
        report.clips_scored,
        labels.join(" ; ")
    );
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("test-runs.log"))
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| format!("Failed to log held-out run: {e}"))
}

pub fn git_commit() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_pads_columns() {
        let t = table(&["a", "value"], &[vec!["long name".into(), "1".into()]]);
        assert_eq!(
            t,
            "| a         | value |\n| --------- | ----- |\n| long name | 1     |\n"
        );
    }
}
