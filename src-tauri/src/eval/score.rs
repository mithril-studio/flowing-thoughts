//! Scoring: word/character error rates, entity accuracy, readable diffs.
//!
//! Everything here works on plain strings so it runs in normal `cargo test`
//! with no model on disk.

use super::manifest::Entities;
use super::normalize::{normalize, normalize_to_string};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Match(String),
    /// Lenient only: adjacent words on one side equal one word on the other
    /// ("pull request" / "pullrequest"). Carries (reference, hypothesis).
    Joined(String, String),
    Sub(String, String),
    Del(String),
    Ins(String),
}

#[derive(Debug, Clone, Default)]
pub struct Alignment {
    pub ops: Vec<Op>,
    pub errors: usize,
}

/// Minimum-edit alignment of reference against hypothesis tokens.
///
/// With `lenient`, a compound written apart (or two words written together)
/// costs nothing. Strict WER is the headline number; the lenient one only
/// shows how much of it is word-joining rather than misrecognition.
pub fn align(reference: &[String], hypothesis: &[String], lenient: bool) -> Alignment {
    let (n, m) = (reference.len(), hypothesis.len());
    #[derive(Clone, Copy)]
    enum Step {
        None,
        Diag,
        Up,
        Left,
        JoinRef,
        JoinHyp,
    }
    // Cost first, then most matched words: among equally cheap alignments the
    // one that lines up identical words gives the readable diff.
    let mut cost = vec![vec![(0usize, 0isize); m + 1]; n + 1];
    let mut step = vec![vec![Step::None; m + 1]; n + 1];
    for i in 1..=n {
        cost[i][0] = (i, 0);
        step[i][0] = Step::Up;
    }
    for j in 1..=m {
        cost[0][j] = (j, 0);
        step[0][j] = Step::Left;
    }
    let plus = |(c, matched): (usize, isize), dc: usize, dm: isize| (c + dc, matched - dm);
    for i in 1..=n {
        for j in 1..=m {
            let same = reference[i - 1] == hypothesis[j - 1];
            let mut best = (
                plus(cost[i - 1][j - 1], usize::from(!same), isize::from(same)),
                Step::Diag,
            );
            for candidate in [
                (plus(cost[i - 1][j], 1, 0), Step::Up),
                (plus(cost[i][j - 1], 1, 0), Step::Left),
            ] {
                if candidate.0 < best.0 {
                    best = candidate;
                }
            }
            if lenient {
                if i >= 2
                    && format!("{}{}", reference[i - 2], reference[i - 1]) == hypothesis[j - 1]
                    && plus(cost[i - 2][j - 1], 0, 1) < best.0
                {
                    best = (plus(cost[i - 2][j - 1], 0, 1), Step::JoinRef);
                }
                if j >= 2
                    && format!("{}{}", hypothesis[j - 2], hypothesis[j - 1]) == reference[i - 1]
                    && plus(cost[i - 1][j - 2], 0, 1) < best.0
                {
                    best = (plus(cost[i - 1][j - 2], 0, 1), Step::JoinHyp);
                }
            }
            cost[i][j] = best.0;
            step[i][j] = best.1;
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        match step[i][j] {
            Step::Diag => {
                let (r, h) = (&reference[i - 1], &hypothesis[j - 1]);
                ops.push(if r == h {
                    Op::Match(r.clone())
                } else {
                    Op::Sub(r.clone(), h.clone())
                });
                i -= 1;
                j -= 1;
            }
            Step::Up => {
                ops.push(Op::Del(reference[i - 1].clone()));
                i -= 1;
            }
            Step::Left => {
                ops.push(Op::Ins(hypothesis[j - 1].clone()));
                j -= 1;
            }
            Step::JoinRef => {
                ops.push(Op::Joined(
                    format!("{} {}", reference[i - 2], reference[i - 1]),
                    hypothesis[j - 1].clone(),
                ));
                i -= 2;
                j -= 1;
            }
            Step::JoinHyp => {
                ops.push(Op::Joined(
                    reference[i - 1].clone(),
                    format!("{} {}", hypothesis[j - 2], hypothesis[j - 1]),
                ));
                i -= 1;
                j -= 2;
            }
            Step::None => unreachable!("alignment backtrace left the table"),
        }
    }
    ops.reverse();
    Alignment {
        ops,
        errors: cost[n][m].0,
    }
}

/// Character-level Levenshtein distance (two-row, so long dictations are cheap).
pub fn char_distance(reference: &str, hypothesis: &str) -> usize {
    let r: Vec<char> = reference.chars().collect();
    let h: Vec<char> = hypothesis.chars().collect();
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    let mut cur = vec![0usize; h.len() + 1];
    for i in 1..=r.len() {
        cur[0] = i;
        for j in 1..=h.len() {
            let sub = prev[j - 1] + usize::from(r[i - 1] != h[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[h.len()]
}

/// Hits and totals for one entity class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Hits {
    pub hit: usize,
    pub total: usize,
}

impl Hits {
    pub fn add(&mut self, other: Hits) {
        self.hit += other.hit;
        self.total += other.total;
    }

    pub fn rate(&self) -> Option<f64> {
        (self.total > 0).then(|| self.hit as f64 / self.total as f64)
    }
}

/// An entity counts as recognised when any of its `/`-separated alternates
/// appears, normalized, as a contiguous word sequence in the hypothesis.
pub fn entity_found(entity: &str, hypothesis_tokens: &[String]) -> bool {
    entity.split('/').any(|alternate| {
        let needle = normalize(alternate);
        !needle.is_empty()
            && hypothesis_tokens
                .windows(needle.len())
                .any(|window| window == needle.as_slice())
    })
}

fn entity_hits(entities: &[String], hypothesis_tokens: &[String]) -> Hits {
    Hits {
        hit: entities
            .iter()
            .filter(|e| entity_found(e, hypothesis_tokens))
            .count(),
        total: entities.len(),
    }
}

/// Scores of one hypothesis against one reference.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ClipScore {
    pub ref_words: usize,
    pub word_errors: usize,
    pub lenient_word_errors: usize,
    pub ref_chars: usize,
    pub char_errors: usize,
    pub names: Hits,
    pub numbers: Hits,
    pub terms: Hits,
}

impl ClipScore {
    pub fn wer(&self) -> f64 {
        ratio(self.word_errors, self.ref_words)
    }
}

pub fn ratio(errors: usize, total: usize) -> f64 {
    if total == 0 {
        // No reference words: any output is wholly wrong, none is perfect.
        return if errors == 0 { 0.0 } else { 1.0 };
    }
    errors as f64 / total as f64
}

pub fn score_clip(reference: &str, hypothesis: &str, entities: &Entities) -> ClipScore {
    let ref_tokens = normalize(reference);
    let hyp_tokens = normalize(hypothesis);
    let ref_string = ref_tokens.join(" ");
    let hyp_string = hyp_tokens.join(" ");
    ClipScore {
        ref_words: ref_tokens.len(),
        word_errors: align(&ref_tokens, &hyp_tokens, false).errors,
        lenient_word_errors: align(&ref_tokens, &hyp_tokens, true).errors,
        ref_chars: ref_string.chars().count(),
        char_errors: char_distance(&ref_string, &hyp_string),
        names: entity_hits(&entities.names, &hyp_tokens),
        numbers: entity_hits(&entities.numbers, &hyp_tokens),
        terms: entity_hits(&entities.terms, &hyp_tokens),
    }
}

/// One-line diff for error inspection: `[ref→hyp]` substitution, `[-ref-]`
/// deletion, `[+hyp+]` insertion. Matches print as plain words.
pub fn render_diff(reference: &str, hypothesis: &str) -> String {
    let alignment = align(&normalize(reference), &normalize(hypothesis), false);
    alignment
        .ops
        .iter()
        .map(|op| match op {
            Op::Match(w) => w.clone(),
            Op::Joined(r, h) | Op::Sub(r, h) => format!("[{r}→{h}]"),
            Op::Del(r) => format!("[-{r}-]"),
            Op::Ins(h) => format!("[+{h}+]"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when the normalized hypothesis carries no words at all.
pub fn is_blank(hypothesis: &str) -> bool {
    normalize_to_string(hypothesis).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn identical_text_has_no_errors() {
        let a = align(&toks("ja dat klopt"), &toks("ja dat klopt"), false);
        assert_eq!(a.errors, 0);
        assert!(a.ops.iter().all(|op| matches!(op, Op::Match(_))));
    }

    #[test]
    fn counts_substitutions_deletions_and_insertions() {
        // 1 substitution (klopt→klopte), 1 deletion (helemaal), 1 insertion (hoor)
        let a = align(
            &toks("ja dat klopt helemaal"),
            &toks("ja dat klopte hoor"),
            false,
        );
        assert_eq!(a.errors, 2, "sub + sub is cheaper than sub + del + ins");
        let a = align(&toks("a b c d"), &toks("a c d e"), false);
        assert_eq!(a.errors, 2);
        assert!(a.ops.contains(&Op::Del("b".into())));
        assert!(a.ops.contains(&Op::Ins("e".into())));
    }

    #[test]
    fn empty_sides() {
        assert_eq!(align(&toks("een twee drie"), &[], false).errors, 3);
        assert_eq!(align(&[], &toks("thanks for watching"), false).errors, 3);
        assert_eq!(align(&[], &[], false).errors, 0);
        assert_eq!(ratio(0, 0), 0.0);
        assert_eq!(ratio(3, 0), 1.0);
    }

    #[test]
    fn lenient_alignment_forgives_joins_only() {
        let reference = toks("open de pull request in de zorgverzekering app");
        let hypothesis = toks("open de pullrequest in de zorg verzekering app");
        assert_eq!(align(&reference, &hypothesis, false).errors, 4);
        let lenient = align(&reference, &hypothesis, true);
        assert_eq!(lenient.errors, 0);
        assert!(lenient
            .ops
            .contains(&Op::Joined("pull request".into(), "pullrequest".into())));
        // A real misrecognition is not forgiven.
        assert_eq!(
            align(&toks("pull request"), &toks("poolrequest"), true).errors,
            2
        );
    }

    #[test]
    fn wer_and_cer_on_dutch_sentences() {
        let none = Entities::default();
        let s = score_clip("Morgen om drie uur.", "morgen om 3 uur", &none);
        assert_eq!((s.ref_words, s.word_errors, s.char_errors), (4, 0, 0));

        let s = score_clip("Ik ga 't morgen doen.", "Ik ga het morgen doen", &none);
        assert_eq!(s.word_errors, 0);

        let s = score_clip(
            "De zorgverzekering is duur",
            "De zorg verzekering is duur",
            &none,
        );
        assert_eq!(
            (s.ref_words, s.word_errors, s.lenient_word_errors),
            (4, 2, 0)
        );
        assert_eq!(s.char_errors, 1);

        // Speech that was discarded entirely: every word is a deletion.
        let s = score_clip("Ja, dat klopt.", "", &none);
        assert_eq!((s.word_errors, s.wer()), (3, 1.0));
        assert_eq!(s.char_errors, s.ref_chars);
    }

    #[test]
    fn char_distance_is_levenshtein() {
        assert_eq!(char_distance("kitten", "sitting"), 3);
        assert_eq!(char_distance("", "abc"), 3);
        assert_eq!(char_distance("coördinatie", "coordinatie"), 1);
    }

    #[test]
    fn entity_accuracy_is_scored_separately() {
        let entities = Entities {
            names: vec!["Annelies".into(), "Van der Meer".into(), "Utrecht".into()],
            numbers: vec!["15".into(), "1250".into(), "12,50/12 euro 50".into()],
            terms: vec!["pull request".into()],
        };
        let s = score_clip(
            "irrelevant",
            "Anna-Lies van der Meer uit Utrecht betaalt vijftien keer €1.250 en 12 euro 50 voor de pullrequest",
            &entities,
        );
        assert_eq!(s.names, Hits { hit: 2, total: 3 });
        assert_eq!(s.numbers, Hits { hit: 3, total: 3 });
        assert_eq!(s.terms, Hits { hit: 0, total: 1 });
        assert_eq!(Hits::default().rate(), None);
    }

    #[test]
    fn entities_match_whole_words_only() {
        let hyp = normalize("We rijden naar Utrechtse heuvelrug met 150 man");
        assert!(!entity_found("Utrecht", &hyp));
        assert!(!entity_found("15", &hyp));
        assert!(entity_found("150", &hyp));
    }

    #[test]
    fn diff_marks_each_kind_of_error() {
        assert_eq!(
            render_diff(
                "Deploy de API key morgen",
                "De ploy de API morgen alsjeblieft"
            ),
            "[+de+] [deploy→ploy] de api [-key-] morgen [+alsjeblieft+]"
        );
        assert!(is_blank(" ... "));
        assert!(!is_blank("ja"));
    }
}
