//! Parser for the read-aloud prompt script (`eval/prompts-nl.txt`).
//!
//! Format, one prompt per line:
//! `<id> | <text> [| names: A; B] [| numbers: 1; 2] [| terms: X; Y]`
//! under `## category: <name>` headers. `#` starts a comment.

use super::manifest::Entities;

pub const NON_SPEECH_CATEGORY: &str = "non_speech";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub id: String,
    pub category: String,
    pub text: String,
    pub entities: Entities,
}

impl Prompt {
    pub fn is_non_speech(&self) -> bool {
        self.category == NON_SPEECH_CATEGORY
    }
}

pub fn parse(script: &str) -> Result<Vec<Prompt>, String> {
    let mut prompts: Vec<Prompt> = Vec::new();
    let mut category: Option<String> = None;
    for (index, line) in script.lines().enumerate() {
        let line_no = index + 1;
        let line = line.trim();
        if let Some(name) = line.strip_prefix("## category:") {
            category = Some(name.trim().to_string());
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('|').map(str::trim);
        let id = fields.next().unwrap_or("").to_string();
        let text = fields.next().unwrap_or("").to_string();
        if id.is_empty() || text.is_empty() || id.contains(char::is_whitespace) {
            return Err(format!("prompt script line {line_no}: expected `<id> | <text>`"));
        }
        let category = category
            .clone()
            .ok_or_else(|| format!("prompt script line {line_no}: prompt before any `## category:`"))?;
        if prompts.iter().any(|p| p.id == id) {
            return Err(format!("prompt script line {line_no}: duplicate id {id}"));
        }
        let mut entities = Entities::default();
        for field in fields {
            let (key, values) = field
                .split_once(':')
                .ok_or_else(|| format!("prompt script line {line_no}: bad field {field:?}"))?;
            let values: Vec<String> = values
                .split(';')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
            match key.trim() {
                "names" => entities.names = values,
                "numbers" => entities.numbers = values,
                "terms" => entities.terms = values,
                other => {
                    return Err(format!("prompt script line {line_no}: unknown field {other:?}"))
                }
            }
        }
        prompts.push(Prompt {
            id,
            category,
            text,
            entities,
        });
    }
    Ok(prompts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::manifest::{assign_split, Split};
    use crate::eval::score::entity_found;
    use std::collections::BTreeMap;

    const COMMITTED_SCRIPT: &str = include_str!("../../../eval/prompts-nl.txt");

    #[test]
    fn parses_fields_and_categories() {
        let script = "# comment\n## category: numbers\nnum-001 | Het kost 1.250 euro. | numbers: 1250\n\n## category: names_places\nname-001 | Annelies woont in Utrecht. | names: Annelies; Utrecht | terms: x\n";
        let prompts = parse(script).unwrap();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].category, "numbers");
        assert_eq!(prompts[0].entities.numbers, ["1250"]);
        assert_eq!(prompts[1].entities.names, ["Annelies", "Utrecht"]);
        assert_eq!(prompts[1].entities.terms, ["x"]);
    }

    #[test]
    fn rejects_malformed_scripts() {
        assert!(parse("short-001 | Ja.").unwrap_err().contains("before any"));
        assert!(parse("## category: a\nx-1 | Ja.\nx-1 | Nee.").unwrap_err().contains("duplicate"));
        assert!(parse("## category: a\nx-1 | Ja. | bogus: 1").unwrap_err().contains("unknown field"));
        assert!(parse("## category: a\njust text").is_err());
    }

    #[test]
    fn committed_script_is_valid_and_large_enough() {
        let prompts = parse(COMMITTED_SCRIPT).expect("eval/prompts-nl.txt must parse");
        assert!(prompts.len() >= 300, "only {} prompts", prompts.len());
        for p in &prompts {
            // An entity the reference itself does not contain can never be hit.
            let reference = crate::eval::normalize::normalize(&p.text);
            for e in p.entities.names.iter().chain(&p.entities.numbers).chain(&p.entities.terms) {
                assert!(entity_found(e, &reference), "{}: entity {e:?} not in its own text", p.id);
            }
        }
        let short = prompts.iter().filter(|p| p.category == "short_reply");
        let under_floor = short
            .filter(|p| p.text.split_whitespace().count() < crate::pipeline::MIN_INJECT_WORDS)
            .count();
        assert!(under_floor >= 25, "need short replies under the five-word floor");
    }

    /// The hash split is only stratified in expectation. This pins that the
    /// committed script actually came out balanced in every category.
    #[test]
    fn committed_script_split_is_stratified() {
        let prompts = parse(COMMITTED_SCRIPT).unwrap();
        let mut per_category: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for p in &prompts {
            let entry = per_category.entry(p.category.as_str()).or_default();
            entry.1 += 1;
            if assign_split(&p.id) == Split::Test {
                entry.0 += 1;
            }
        }
        for (category, (test, total)) in per_category {
            let share = test as f64 / total as f64;
            assert!(
                (0.15..=0.26).contains(&share),
                "{category}: {test}/{total} prompts in test ({share:.2})"
            );
        }
    }
}
