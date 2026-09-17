use std::collections::HashMap;

/// Longest run of words (per side) a single edit hunk may span and still be
/// learned. Bigger hunks are rewrites, not corrections.
pub const MAX_HUNK_WORDS: usize = 3;
/// Hard cap on hunks learned from one edit, so a heavily rewritten paragraph
/// can't flood the table.
pub const MAX_HUNKS: usize = 8;
/// Fraction of the original's words that must survive unchanged. Below this
/// the two texts are probably unrelated (different field, different
/// dictation) rather than an edited copy.
pub const MIN_SIMILARITY: f32 = 0.5;
/// Word-count ceilings for the diff table (n * m cells).
const MAX_ORIGINAL_WORDS: usize = 1_000;
const MAX_MODIFIED_WORDS: usize = 4_000;

/// Why an (original, modified) pair produced no corrections. Surfaced in the
/// log so a silent no-op is diagnosable.
#[derive(Debug, PartialEq, Eq)]
pub enum RejectReason {
    Empty,
    TooLong,
    Unchanged,
    TooDifferent { similarity_pct: u8 },
    NoLearnableHunks,
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RejectReason::Empty => write!(f, "empty text"),
            RejectReason::TooLong => write!(f, "text too long to diff"),
            RejectReason::Unchanged => write!(f, "no word changed"),
            RejectReason::TooDifferent { similarity_pct } => write!(
                f,
                "texts too different ({similarity_pct}% of words kept, need {}%)",
                (MIN_SIMILARITY * 100.0) as u8
            ),
            RejectReason::NoLearnableHunks => write!(
                f,
                "only insertions, deletions or hunks over {MAX_HUNK_WORDS} words"
            ),
        }
    }
}

/// Extract word-level corrections from (original, modified).
///
/// Runs a longest-common-subsequence diff over the words (boundary
/// punctuation ignored) and turns every substitution hunk of up to
/// `MAX_HUNK_WORDS` words per side into a `(wrong, right)` pair. Pure
/// insertions and deletions are skipped: they can't be replayed as a
/// replacement. Text added around the dictation (a field that already had
/// content) is therefore harmless.
///
/// Returns `Err` with the reason when nothing could be learned.
pub fn extract_corrections(
    original: &str,
    modified: &str,
) -> Result<Vec<(String, String)>, RejectReason> {
    let orig = core_words(original);
    let modi = core_words(modified);
    if orig.is_empty() || modi.is_empty() {
        return Err(RejectReason::Empty);
    }
    if orig.len() > MAX_ORIGINAL_WORDS || modi.len() > MAX_MODIFIED_WORDS {
        return Err(RejectReason::TooLong);
    }

    let (kept, ops) = word_diff(&orig, &modi);
    let similarity = kept as f32 / orig.len() as f32;
    if kept == orig.len() && kept == modi.len() {
        return Err(RejectReason::Unchanged);
    }
    if similarity < MIN_SIMILARITY {
        return Err(RejectReason::TooDifferent {
            similarity_pct: (similarity * 100.0).round() as u8,
        });
    }

    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut deleted: Vec<&str> = Vec::new();
    let mut inserted: Vec<&str> = Vec::new();
    let flush = |deleted: &mut Vec<&str>, inserted: &mut Vec<&str>, pairs: &mut Vec<(String, String)>| {
        let learnable = !deleted.is_empty()
            && !inserted.is_empty()
            && deleted.len() <= MAX_HUNK_WORDS
            && inserted.len() <= MAX_HUNK_WORDS;
        if learnable {
            let wrong = deleted.join(" ");
            let right = inserted.join(" ");
            if wrong != right && !pairs.iter().any(|(w, _)| *w == wrong) {
                pairs.push((wrong, right));
            }
        }
        deleted.clear();
        inserted.clear();
    };
    for op in ops {
        match op {
            DiffOp::Keep => flush(&mut deleted, &mut inserted, &mut pairs),
            DiffOp::Delete(w) => deleted.push(w),
            DiffOp::Insert(w) => inserted.push(w),
        }
    }
    flush(&mut deleted, &mut inserted, &mut pairs);

    if pairs.is_empty() {
        return Err(RejectReason::NoLearnableHunks);
    }
    pairs.truncate(MAX_HUNKS);
    Ok(pairs)
}

enum DiffOp<'a> {
    Keep,
    Delete(&'a str),
    Insert(&'a str),
}

/// Words with boundary punctuation stripped; tokens that were pure
/// punctuation are dropped.
fn core_words(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .map(|w| split_boundary_punct(w).1)
        .filter(|w| !w.is_empty())
        .collect()
}

/// LCS diff. Returns the number of kept words and the edit script in order.
fn word_diff<'a>(orig: &[&'a str], modi: &[&'a str]) -> (usize, Vec<DiffOp<'a>>) {
    let n = orig.len();
    let m = modi.len();
    let width = m + 1;
    let mut table = vec![0u16; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if orig[i] == modi[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let kept = table[0] as usize;

    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if orig[i] == modi[j] {
            ops.push(DiffOp::Keep);
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            ops.push(DiffOp::Delete(orig[i]));
            i += 1;
        } else {
            ops.push(DiffOp::Insert(modi[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffOp::Delete(orig[i]));
        i += 1;
    }
    while j < m {
        ops.push(DiffOp::Insert(modi[j]));
        j += 1;
    }
    (kept, ops)
}

/// Case-insensitive word-boundary replacement: wherever `wrong` appears as a
/// whole word (or whole run of words) in `text`, replace with `right`.
/// Substrings inside longer words are left alone.
///
/// Matches are case-insensitive to catch capitalisation variants the decoder
/// produces, but preserve the replacement's exact casing (plus a leading
/// capital when the matched text had one). Longer `wrong` phrases win over
/// shorter ones at the same position.
pub fn apply_replacements(text: &str, pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return text.to_string();
    }
    let mut rules: Vec<(Vec<&str>, &str)> = pairs
        .iter()
        .map(|(wrong, right)| (wrong.split_whitespace().collect(), right.as_str()))
        .filter(|(words, _): &(Vec<&str>, &str)| !words.is_empty())
        .collect();
    rules.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let tokens = split_preserving_whitespace(text);
    let mut out = String::with_capacity(text.len());
    let mut idx = 0;
    while idx < tokens.len() {
        let token = &tokens[idx];
        if token.chars().all(char::is_whitespace) {
            out.push_str(token);
            idx += 1;
            continue;
        }
        // Word tokens sit at idx, idx+2, idx+4, ... (whitespace runs between).
        let matched = rules.iter().find(|(words, _)| {
            words.iter().enumerate().all(|(k, w)| {
                tokens
                    .get(idx + 2 * k)
                    .map(|t| w.eq_ignore_ascii_case(split_boundary_punct(t).1))
                    .unwrap_or(false)
            })
        });
        match matched {
            Some((words, right)) => {
                let last = idx + 2 * (words.len() - 1);
                let (leading, core, _) = split_boundary_punct(&tokens[idx]);
                let (_, _, trailing) = split_boundary_punct(&tokens[last]);
                out.push_str(leading);
                out.push_str(&preserve_case(core, right));
                out.push_str(trailing);
                idx = last + 1;
            }
            None => {
                out.push_str(token);
                idx += 1;
            }
        }
    }
    out
}

/// Build a Whisper `prompt` string from the top-N intended (correct) terms.
/// Whisper accepts up to ~224 tokens; we cap at a conservative character
/// budget so we don't overflow.
pub fn build_prompt_from_corrections(intended_terms: &[String], max_chars: usize) -> Option<String> {
    if intended_terms.is_empty() {
        return None;
    }
    let mut seen: HashMap<&str, ()> = HashMap::new();
    let mut buffer = String::new();
    for term in intended_terms {
        let trimmed = term.trim();
        if trimmed.is_empty() || seen.contains_key(trimmed) {
            continue;
        }
        let addition_len = trimmed.len() + if buffer.is_empty() { 0 } else { 2 };
        if buffer.len() + addition_len > max_chars {
            break;
        }
        if !buffer.is_empty() {
            buffer.push_str(", ");
        }
        buffer.push_str(trimmed);
        seen.insert(trimmed, ());
    }
    if buffer.is_empty() {
        None
    } else {
        Some(buffer)
    }
}

fn is_boundary_punct(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
    )
}

pub(crate) fn split_boundary_punct(word: &str) -> (&str, &str, &str) {
    let leading_end = word
        .char_indices()
        .find(|(_, c)| !is_boundary_punct(*c))
        .map(|(i, _)| i)
        .unwrap_or(word.len());
    let trailing_start = word
        .char_indices()
        .rev()
        .find(|(_, c)| !is_boundary_punct(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0)
        .max(leading_end);
    (
        &word[..leading_end],
        &word[leading_end..trailing_start],
        &word[trailing_start..],
    )
}

fn preserve_case(original: &str, replacement: &str) -> String {
    // If the original word started uppercase, preserve that on the replacement.
    // Otherwise, keep replacement's own casing verbatim.
    let mut orig_chars = original.chars();
    let first = orig_chars.next();
    if first.map(|c| c.is_uppercase()).unwrap_or(false) {
        let mut rep_chars = replacement.chars();
        match rep_chars.next() {
            Some(c) => {
                let mut out = c.to_uppercase().collect::<String>();
                out.push_str(rep_chars.as_str());
                out
            }
            None => replacement.to_string(),
        }
    } else {
        replacement.to_string()
    }
}

// Splits text into tokens while preserving whitespace as separate "tokens" so
// we can reconstruct the original spacing on re-join. Each returned piece is
// either a run of non-whitespace or a run of whitespace.
pub(crate) fn split_preserving_whitespace(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_whitespace: Option<bool> = None;
    for c in text.chars() {
        let is_ws = c.is_whitespace();
        match in_whitespace {
            None => {
                current.push(c);
                in_whitespace = Some(is_ws);
            }
            Some(prev_ws) if prev_ws == is_ws => current.push(c),
            Some(_) => {
                out.push(std::mem::take(&mut current));
                current.push(c);
                in_whitespace = Some(is_ws);
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn single_word_substitution_is_captured() {
        let got = extract_corrections("hot chocolate is nice", "hot cocoa is nice");
        assert_eq!(got, Ok(pairs(&[("chocolate", "cocoa")])));
    }

    #[test]
    fn several_single_word_edits_are_all_captured() {
        let got = extract_corrections(
            "ik wil dit morgen naar de klant sturen",
            "ik wil dit vanavond naar de client sturen",
        );
        assert_eq!(
            got,
            Ok(pairs(&[("morgen", "vanavond"), ("klant", "client")]))
        );
    }

    #[test]
    fn multi_word_hunk_within_cap_is_captured() {
        let got = extract_corrections(
            "we ship the flowing thoughts app on friday",
            "we ship the FlowingThoughts app on friday",
        );
        assert_eq!(got, Ok(pairs(&[("flowing thoughts", "FlowingThoughts")])));
    }

    #[test]
    fn hunk_over_cap_is_skipped_but_others_survive() {
        let got = extract_corrections(
            "alpha beta gamma delta epsilon zeta eta theta iota kappa",
            "alpha one two three four five zeta eta theta iota kapa",
        );
        assert_eq!(got, Ok(pairs(&[("kappa", "kapa")])));
    }

    #[test]
    fn insertions_and_deletions_are_not_learned() {
        assert_eq!(
            extract_corrections("hot chocolate", "hot chocolate today"),
            Err(RejectReason::NoLearnableHunks)
        );
        assert_eq!(
            extract_corrections("hot chocolate today", "hot chocolate"),
            Err(RejectReason::NoLearnableHunks)
        );
    }

    #[test]
    fn deleted_filler_word_next_to_a_fix_is_still_learned() {
        // "eigenlijk" removed, "de" -> "the": one hunk, deletion + substitution.
        let got = extract_corrections(
            "dit is eigenlijk de beste optie",
            "dit is the beste optie",
        );
        assert_eq!(got, Ok(pairs(&[("eigenlijk de", "the")])));
    }

    #[test]
    fn whole_phrase_rewrite_is_rejected() {
        assert!(matches!(
            extract_corrections("hot chocolate today", "cold water tomorrow"),
            Err(RejectReason::TooDifferent { .. })
        ));
    }

    #[test]
    fn identical_text_is_rejected() {
        assert_eq!(
            extract_corrections("hello world", "hello world"),
            Err(RejectReason::Unchanged)
        );
    }

    #[test]
    fn punctuation_only_edit_is_unchanged() {
        assert_eq!(
            extract_corrections("hello world.", "hello, world"),
            Err(RejectReason::Unchanged)
        );
    }

    #[test]
    fn empty_original_is_rejected() {
        assert_eq!(extract_corrections("", "something"), Err(RejectReason::Empty));
    }

    #[test]
    fn pre_existing_field_content_does_not_block_learning() {
        // The focused field already held a paragraph; the dictation was
        // appended and one word in it fixed.
        let injected = "please review the mitril proposal today";
        let focused = "Hi team, quick note before lunch. Please review the Mithril proposal today";
        let got = extract_corrections(injected, focused);
        assert_eq!(got, Ok(pairs(&[("mitril", "Mithril")])));
    }

    #[test]
    fn casing_fix_is_learned() {
        let got = extract_corrections("talk to joost tomorrow", "talk to Joost tomorrow");
        assert_eq!(got, Ok(pairs(&[("joost", "Joost")])));
    }

    #[test]
    fn apply_multi_word_replacement() {
        let p = pairs(&[("flowing thoughts", "FlowingThoughts")]);
        assert_eq!(
            apply_replacements("Open flowing thoughts, then dictate.", &p),
            "Open FlowingThoughts, then dictate."
        );
    }

    #[test]
    fn apply_prefers_longest_match() {
        let p = pairs(&[("new", "nieuw"), ("new york", "New York")]);
        assert_eq!(
            apply_replacements("a new york trip and a new car", &p),
            "a New York trip and a nieuw car"
        );
    }

    #[test]
    fn apply_replacement_on_whole_word() {
        let pairs = vec![("chocolate".to_string(), "cocoa".to_string())];
        let got = apply_replacements("hot chocolate today", &pairs);
        assert_eq!(got, "hot cocoa today");
    }

    #[test]
    fn apply_replacement_is_case_insensitive_match_but_preserves_case_on_leading_upper() {
        let pairs = vec![("joost".to_string(), "joost".to_string())];
        // Wrong has different case in text; match should still fire, and the
        // original's leading-upper should carry through.
        let got = apply_replacements("Hello Jooost and joost", &[("Jooost".to_string(), "Joost".to_string())]);
        assert_eq!(got, "Hello Joost and joost");
        // Unused var suppression.
        drop(pairs);
    }

    #[test]
    fn apply_replacement_handles_trailing_punctuation() {
        let pairs = vec![("chocolate".to_string(), "cocoa".to_string())];
        let got = apply_replacements("I love chocolate!", &pairs);
        assert_eq!(got, "I love cocoa!");
    }

    #[test]
    fn apply_replacement_does_not_match_substring_of_longer_word() {
        let pairs = vec![("cat".to_string(), "dog".to_string())];
        let got = apply_replacements("concatenate the cat", &pairs);
        assert_eq!(got, "concatenate the dog");
    }

    #[test]
    fn apply_replacement_no_op_when_pairs_empty() {
        let got = apply_replacements("hello world", &[]);
        assert_eq!(got, "hello world");
    }

    #[test]
    fn build_prompt_from_corrections_returns_none_when_empty() {
        assert!(build_prompt_from_corrections(&[], 200).is_none());
    }

    #[test]
    fn build_prompt_from_corrections_deduplicates_and_respects_char_cap() {
        let terms = vec![
            "Joost".to_string(),
            "Mithril".to_string(),
            "Joost".to_string(),
            "FlowingThoughts".to_string(),
        ];
        let got = build_prompt_from_corrections(&terms, 200).unwrap();
        assert!(got.contains("Joost"));
        assert!(got.contains("Mithril"));
        assert!(got.contains("FlowingThoughts"));
        assert_eq!(got.matches("Joost").count(), 1);
    }

    #[test]
    fn build_prompt_from_corrections_stops_at_budget() {
        let terms = vec![
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
        ];
        let got = build_prompt_from_corrections(&terms, 12).unwrap();
        // "alpha, beta" = 11 chars, fits. Adding ", gamma" = 7 more → over cap.
        assert_eq!(got, "alpha, beta");
    }
}
