//! Text normalization for scoring Dutch transcripts.
//!
//! Deliberately conservative: a rule only exists when two written forms are
//! the *same spoken words* ("'t" / "het", "vijftien" / "15", "€25" / "25
//! euro"). Anything a user would have to fix by hand after dictation — a
//! split compound, a misspelt loanword, a dropped diacritic — stays an error.
//! The rules are documented in `docs/DUTCH_EVAL.md`; keep the two in sync.

/// Normalize `text` into the tokens WER is computed over.
pub fn normalize(text: &str) -> Vec<String> {
    let lowered = text
        .replace(['\u{2019}', '\u{2018}', '`', '\u{00B4}'], "'")
        .to_lowercase();
    // Symbols that are spoken as words. Hyphens and slashes separate words:
    // "e-mail" scores as "e mail", and the lenient WER forgives the join.
    let spaced = lowered
        .replace('%', " procent ")
        .replace('&', " en ")
        .replace(['-', '\u{2013}', '\u{2014}', '/'], " ");

    let mut tokens: Vec<String> = Vec::new();
    let mut pending_euro = false;
    for raw in spaced.split_whitespace() {
        let (had_euro, raw) = match raw.strip_prefix('€') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        let cleaned = clean_token(raw);
        if cleaned.is_empty() {
            // A bare "€" applies to the amount that follows it.
            pending_euro |= had_euro;
            continue;
        }
        let is_amount = cleaned.starts_with(|c: char| c.is_ascii_digit());
        tokens.push(cleaned);
        if (had_euro || pending_euro) && is_amount {
            tokens.push("euro".to_string());
            pending_euro = false;
        }
    }
    join_thousands(tokens.into_iter().map(number_word_to_digits).collect())
}

/// `normalize`, joined back into one string — the input for CER.
pub fn normalize_to_string(text: &str) -> String {
    normalize(text).join(" ")
}

fn clean_token(raw: &str) -> String {
    let trimmed = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
    // Clitics are the same word as their full form; Whisper writes either.
    let expanded = match trimmed {
        "'t" => "het",
        "'n" => "een",
        "'k" => "ik",
        "m'n" => "mijn",
        "z'n" => "zijn",
        other => other,
    };
    let trimmed = expanded.trim_matches('\'');
    let chars: Vec<char> = trimmed.chars().collect();
    let mut out = String::with_capacity(trimmed.len());
    for (i, &c) in chars.iter().enumerate() {
        let between_digits = i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit();
        if c.is_alphanumeric() || c == '\'' || (between_digits && matches!(c, ',' | '.' | ':')) {
            out.push(c);
        }
    }
    normalize_separators(out)
}

/// "1.250" -> "1250" (thousands), "12.50" -> "12,50" (decimal point).
fn normalize_separators(token: String) -> String {
    if !token.contains('.')
        || !token
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return token;
    }
    let (int_part, decimals) = match token.split_once(',') {
        Some((i, d)) => (i, Some(d)),
        None => (token.as_str(), None),
    };
    let groups: Vec<&str> = int_part.split('.').collect();
    let thousands = groups.len() > 1
        && !groups[0].is_empty()
        && groups[0].len() <= 3
        && groups[1..].iter().all(|g| g.len() == 3);
    if thousands {
        let joined = groups.concat();
        return match decimals {
            Some(d) => format!("{joined},{d}"),
            None => joined,
        };
    }
    if decimals.is_none() && groups.len() == 2 && (1..=2).contains(&groups[1].len()) {
        return format!("{},{}", groups[0], groups[1]);
    }
    token
}

fn number_word_to_digits(token: String) -> String {
    match parse_dutch_number(&token) {
        Some(n) => n.to_string(),
        None => token,
    }
}

/// "tweeduizend zesentwintig" is one number (2026) spoken as two words.
fn join_thousands(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    for token in tokens {
        if let (Some(prev), Ok(n)) = (out.last_mut(), token.parse::<u64>()) {
            if let Ok(p) = prev.parse::<u64>() {
                if p >= 1000
                    && p % 1000 == 0
                    && n < 1000
                    && token.len() <= 3
                    && !token.starts_with('0')
                {
                    *prev = (p + n).to_string();
                    continue;
                }
            }
        }
        out.push(token);
    }
    out
}

const UNITS: [(&str, u64); 21] = [
    ("nul", 0),
    ("één", 1),
    ("eén", 1),
    ("twee", 2),
    ("drie", 3),
    ("vier", 4),
    ("vijf", 5),
    ("zes", 6),
    ("zeven", 7),
    ("acht", 8),
    ("negen", 9),
    ("tien", 10),
    ("elf", 11),
    ("twaalf", 12),
    ("dertien", 13),
    ("veertien", 14),
    ("vijftien", 15),
    ("zestien", 16),
    ("zeventien", 17),
    ("achttien", 18),
    ("negentien", 19),
];

const TENS: [(&str, u64); 8] = [
    ("twintig", 20),
    ("dertig", 30),
    ("veertig", 40),
    ("vijftig", 50),
    ("zestig", 60),
    ("zeventig", 70),
    ("tachtig", 80),
    ("negentig", 90),
];

/// Parse a single-token Dutch cardinal ("drieëntwintig", "tweehonderdvijftig",
/// "twaalfhonderd", "tweeduizend") below one million.
///
/// A bare "een" is never a number: it is almost always the article, and
/// turning every "een" into "1" would corrupt far more than it fixes.
pub fn parse_dutch_number(word: &str) -> Option<u64> {
    if word == "een" || word.is_empty() || !word.chars().all(char::is_alphabetic) {
        return None;
    }
    if let Some((left, right)) = word.split_once("duizend") {
        let thousands = if left.is_empty() {
            1
        } else {
            parse_below_1000(left)?
        };
        let rest = if right.is_empty() {
            0
        } else {
            parse_below_1000(right)?
        };
        return Some(thousands * 1000 + rest);
    }
    // "twaalfhonderd", "vijfentwintighonderd"
    if let Some((left, right)) = word.split_once("honderd") {
        if let Some(h) = parse_below_100(left).filter(|h| (11..100).contains(h)) {
            let rest = if right.is_empty() {
                0
            } else {
                parse_below_100(right)?
            };
            return Some(h * 100 + rest);
        }
    }
    parse_below_1000(word)
}

fn parse_below_1000(word: &str) -> Option<u64> {
    if let Some((left, right)) = word.split_once("honderd") {
        let hundreds = if left.is_empty() {
            1
        } else {
            parse_below_100(left).filter(|h| (2..10).contains(h))?
        };
        let rest = if right.is_empty() {
            0
        } else {
            parse_below_100(right)?
        };
        return Some(hundreds * 100 + rest);
    }
    parse_below_100(word)
}

fn parse_below_100(word: &str) -> Option<u64> {
    if let Some(&(_, n)) = UNITS.iter().chain(TENS.iter()).find(|(w, _)| *w == word) {
        return Some(n);
    }
    for (tens_word, tens) in TENS {
        let Some(prefix) = word.strip_suffix(tens_word) else {
            continue;
        };
        let unit_word = prefix
            .strip_suffix("ën")
            .or_else(|| prefix.strip_suffix("en"))?;
        let unit = match unit_word {
            // Inside a compound, "een" can only be the number.
            "een" => 1,
            other => UNITS.iter().find(|(w, _)| *w == other).map(|&(_, n)| n)?,
        };
        if (1..10).contains(&unit) {
            return Some(tens + unit);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        normalize_to_string(s)
    }

    #[test]
    fn casing_and_punctuation_are_ignored() {
        assert_eq!(norm("Ja, dat klopt!"), "ja dat klopt");
        assert_eq!(norm("  “Morgen”…  om drie uur? "), "morgen om 3 uur");
    }

    #[test]
    fn clitics_equal_their_full_forms() {
        assert_eq!(norm("'t Is zo'n mooie dag"), norm("Het is zo'n mooie dag"));
        assert_eq!(
            norm("Ik heb m’n sleutels en z'n jas"),
            "ik heb mijn sleutels en zijn jas"
        );
        assert_eq!(norm("'s Ochtends"), "s ochtends");
        // In-word apostrophes are spelling and stay.
        assert_eq!(norm("Twee auto's"), "2 auto's");
    }

    #[test]
    fn article_een_is_never_a_number() {
        assert_eq!(norm("Ik wil een koffie"), "ik wil een koffie");
        assert_eq!(norm("Ik wil één koffie"), "ik wil 1 koffie");
    }

    #[test]
    fn number_words_equal_digits() {
        for (word, n) in [
            ("nul", 0),
            ("zeven", 7),
            ("vijftien", 15),
            ("twintig", 20),
            ("eenentwintig", 21),
            ("tweeëntwintig", 22),
            ("drieentwintig", 23),
            ("negenennegentig", 99),
            ("honderd", 100),
            ("honderdvijf", 105),
            ("tweehonderdvijftig", 250),
            ("twaalfhonderd", 1200),
            ("twaalfhonderdvijftig", 1250),
            ("duizend", 1000),
            ("tweeduizend", 2000),
            ("tweeduizendzesentwintig", 2026),
            ("vijftienduizend", 15_000),
            ("driehonderdduizend", 300_000),
        ] {
            assert_eq!(parse_dutch_number(word), Some(n), "{word}");
        }
        for not_a_number in [
            "een",
            "enen",
            "tientje",
            "achter",
            "viering",
            "honderden",
            "",
        ] {
            assert_eq!(parse_dutch_number(not_a_number), None, "{not_a_number}");
        }
    }

    #[test]
    fn years_spoken_as_two_words_join() {
        assert_eq!(norm("in tweeduizend zesentwintig"), norm("in 2026"));
        // Two separate numbers stay separate.
        assert_eq!(norm("kamer 12 en 14"), "kamer 12 en 14");
        assert_eq!(norm("2000 26"), "2026");
        assert_eq!(norm("1000 1000"), "1000 1000");
    }

    #[test]
    fn amounts_and_separators() {
        assert_eq!(norm("€25"), "25 euro");
        assert_eq!(norm("€ 1.250,50"), "1250,50 euro");
        assert_eq!(norm("Dat kost 1.250 euro."), "dat kost 1250 euro");
        assert_eq!(norm("12.50 euro"), "12,50 euro");
        assert_eq!(norm("15 %"), "15 procent");
        assert_eq!(norm("om 15:30 uur"), "om 15:30 uur");
        assert_eq!(norm("versie 1.2.3"), "versie 1.2.3");
    }

    #[test]
    fn hyphens_split_and_loanwords_are_left_alone() {
        assert_eq!(norm("e-mail"), "e mail");
        assert_eq!(norm("API-key"), "api key");
        assert_eq!(norm("Node.js"), "nodejs");
        // No spelling forgiveness: this difference must surface as an error.
        assert_ne!(norm("gedeployd"), norm("gedeployed"));
        assert_ne!(norm("zorgverzekering"), norm("zorg verzekering"));
        assert_ne!(norm("coördinatie"), norm("coordinatie"));
    }
}
