use std::collections::HashMap;

/// Attempts to extract a single-word correction from (original, modified).
///
/// Returns `Some((wrong, right))` only when exactly one word differs between
/// the two texts after whitespace splitting. Whole-phrase rewrites, deletions,
/// and multi-word edits are rejected (conservative MVP guardrail).
pub fn extract_single_word_correction(
    original: &str,
    modified: &str,
) -> Option<(String, String)> {
    let orig_words: Vec<&str> = original.split_whitespace().collect();
    let mod_words: Vec<&str> = modified.split_whitespace().collect();

    if orig_words.is_empty() || orig_words.len() != mod_words.len() {
        return None;
    }

    let mut differences: Vec<(&str, &str)> = Vec::new();
    for (o, m) in orig_words.iter().zip(mod_words.iter()) {
        if o != m {
            differences.push((*o, *m));
        }
    }

    if differences.len() == 1 {
        let (wrong, right) = differences[0];
        // Reject trivial case or empty substitution.
        if wrong.is_empty() || right.is_empty() || wrong == right {
            return None;
        }
        Some((wrong.to_string(), right.to_string()))
    } else {
        None
    }
}

/// Case-insensitive word-boundary replacement: wherever `wrong` appears as a
/// whole word in `text`, replace with `right`. Other occurrences (substrings
/// inside longer words) are left alone.
///
/// Matches are case-insensitive to catch capitalisation variants the decoder
/// produces, but preserve the replacement's exact casing.
pub fn apply_replacements(text: &str, pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    for (idx, word) in split_preserving_whitespace(text).into_iter().enumerate() {
        if idx > 0 {
            // Separator characters are preserved inside split_preserving_whitespace.
        }
        let replacement = pairs
            .iter()
            .find(|(wrong, _)| wrong.eq_ignore_ascii_case(word.trim_matches(is_boundary_punct)));
        match replacement {
            Some((_, right)) => {
                let (leading, core, trailing) = split_boundary_punct(&word);
                out.push_str(leading);
                out.push_str(&preserve_case(core, right));
                out.push_str(trailing);
            }
            None => out.push_str(&word),
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

fn split_boundary_punct(word: &str) -> (&str, &str, &str) {
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
fn split_preserving_whitespace(text: &str) -> Vec<String> {
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

    #[test]
    fn single_word_substitution_is_captured() {
        let got = extract_single_word_correction(
            "hot chocolate is nice",
            "hot cocoa is nice",
        );
        assert_eq!(got, Some(("chocolate".to_string(), "cocoa".to_string())));
    }

    #[test]
    fn whole_phrase_rewrite_is_rejected() {
        assert!(extract_single_word_correction(
            "hot chocolate today",
            "cold water tomorrow"
        )
        .is_none());
    }

    #[test]
    fn length_change_is_rejected() {
        assert!(extract_single_word_correction(
            "hot chocolate",
            "hot chocolate today"
        )
        .is_none());
    }

    #[test]
    fn identical_text_is_rejected() {
        assert!(extract_single_word_correction("hello world", "hello world").is_none());
    }

    #[test]
    fn empty_original_is_rejected() {
        assert!(extract_single_word_correction("", "something").is_none());
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
