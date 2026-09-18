//! Mine the correction history for *suggested* read-aloud prompts.
//!
//! Corrections show which words the model keeps getting wrong, so they are a
//! good source of sentences worth recording. They are never ground truth and
//! never paired speech: the output is a suggestions file in the prompt-script
//! format, written into the (personal, untracked) dataset directory for a
//! human to review, edit and then record with `record --prompts <file>`.

use crate::corrections;
use crate::db::Correction;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SUGGESTIONS_FILE: &str = "suggested-prompts.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub wrong: String,
    pub intended: String,
    pub occurrences: usize,
    /// A sentence the term occurred in, with the correction applied.
    pub sentence: Option<String>,
}

/// Group corrections by (wrong, intended), most frequent first.
pub fn suggest(corrections_history: &[Correction], limit: usize) -> Vec<Suggestion> {
    let mut grouped: BTreeMap<(String, String), (usize, Option<String>)> = BTreeMap::new();
    for c in corrections_history {
        let key = (
            c.wrong_text.trim().to_lowercase(),
            c.intended_text.trim().to_string(),
        );
        let entry = grouped.entry(key).or_default();
        entry.0 += 1;
        if entry.1.is_none() {
            entry.1 = c.context_snippet.as_deref().and_then(|snippet| {
                let pair = [(c.wrong_text.clone(), c.intended_text.clone())];
                let fixed = corrections::apply_replacements(snippet.trim(), &pair);
                // The snippet is cut at 200 chars; a usable prompt is a whole sentence.
                (fixed.contains(c.intended_text.trim()) && snippet.chars().count() < 200)
                    .then_some(fixed)
            });
        }
    }
    let mut out: Vec<Suggestion> = grouped
        .into_iter()
        .map(|((wrong, intended), (occurrences, sentence))| Suggestion {
            wrong,
            intended,
            occurrences,
            sentence,
        })
        .collect();
    out.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then_with(|| a.intended.cmp(&b.intended))
    });
    out.truncate(limit);
    out
}

pub fn render(suggestions: &[Suggestion]) -> String {
    let mut out = String::from(
        "# SUGGESTIONS ONLY — mined from correction history, not ground truth.\n\
         # Review every line: fix the sentence so it is exactly what you will read aloud,\n\
         # delete what you do not want, then record with:\n\
         #   npm run eval -- record --prompts <this file>\n\
         # Personal content: this file stays in the dataset directory, outside git.\n\n\
         ## category: mined\n",
    );
    for (index, s) in suggestions.iter().enumerate() {
        let sentence = s
            .sentence
            .clone()
            .unwrap_or_else(|| format!("TODO schrijf een zin met {}.", s.intended));
        out.push_str(&format!(
            "# heard {:?} instead of {:?}, {}x\nmined-{:03} | {} | terms: {}\n",
            s.wrong,
            s.intended,
            s.occurrences,
            index + 1,
            sentence.replace('|', " ").replace('\n', " "),
            s.intended.replace(['|', ';', '/'], " "),
        ));
    }
    out
}

/// Read the live correction history (read-only) and write the suggestions file.
pub fn run(data_dir: &Path, limit: usize) -> Result<(PathBuf, usize), String> {
    let history = match crate::db::open_read_only()? {
        Some(conn) => crate::db::list_corrections(&conn)?,
        None => Vec::new(),
    };
    let suggestions = suggest(&history, limit);
    super::manifest::ensure_layout(data_dir)?;
    let path = data_dir.join(SUGGESTIONS_FILE);
    std::fs::write(&path, render(&suggestions))
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    Ok((path, suggestions.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn correction(wrong: &str, intended: &str, context: Option<&str>) -> Correction {
        Correction {
            id: uuid::Uuid::new_v4().to_string(),
            dictation_id: "d".into(),
            model: "whisper-small-q5".into(),
            wrong_text: wrong.into(),
            intended_text: intended.into(),
            context_snippet: context.map(str::to_string),
            created_at: "2026-09-17T10:00:00Z".into(),
        }
    }

    #[test]
    fn most_frequent_corrections_become_reviewable_prompt_lines() {
        let history = vec![
            correction(
                "work tree",
                "worktree",
                Some("Maak een nieuwe work tree aan voor deze branch."),
            ),
            correction("Work tree", "worktree", None),
            correction("versel", "Vercel", None),
        ];
        let suggestions = suggest(&history, 10);
        assert_eq!(suggestions[0].intended, "worktree");
        assert_eq!(suggestions[0].occurrences, 2);
        assert_eq!(
            suggestions[0].sentence.as_deref(),
            Some("Maak een nieuwe worktree aan voor deze branch.")
        );
        let rendered = render(&suggestions);
        assert!(rendered.starts_with("# SUGGESTIONS ONLY"));
        // The output is a valid prompt script the recorder can consume.
        let prompts = crate::eval::prompts::parse(&rendered).unwrap();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].entities.terms, ["worktree"]);
        assert!(prompts[1].text.contains("TODO"));
    }
}
