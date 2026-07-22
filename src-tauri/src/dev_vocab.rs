//! Built-in "developer" vocabulary.
//!
//! Whisper mangles the jargon a developer dictates all day — "API" comes out
//! as "api", "TypeScript" as "type script", "JSON file" as "jason file".
//! This module fixes that in two places:
//!
//! 1. **Decoder bias** — [`build_biased_prompt`] feeds the canonical terms
//!    into the Whisper prompt (local and cloud paths both accept it), so the
//!    decoder prefers these spellings on ambiguous audio. User-learned
//!    corrections always take budget priority and sit at the end of the
//!    prompt, where they survive Whisper's keep-the-tail token truncation.
//! 2. **Output normalization** — [`normalize`] repairs what still comes out
//!    wrong: known split/mishear phrases first, then casing of unambiguous
//!    tokens.
//!
//! The vocabulary is deliberately role-scoped (this is the "developer" role;
//! more roles can follow) and the normalization lists are conservative:
//! nothing that is also a common English word ("go", "rest", "ram", "swift")
//! is ever rewritten, and lowercase command spellings ("git push", "docker
//! run", "python main.py") are left alone.

use crate::corrections;

/// Canonical spellings fed to the Whisper prompt. Order matters loosely —
/// most-dictated terms first, so they survive if the character budget runs
/// out before the list does.
pub const PROMPT_TERMS: &[&str] = &[
    // Everyday acronyms
    "API", "CLI", "SDK", "JSON", "YAML", "XML", "HTML", "CSS", "SQL", "HTTP",
    "HTTPS", "REST", "gRPC", "GraphQL", "OAuth", "JWT", "SSH", "TLS", "DNS",
    "TCP", "UDP", "URL", "URI", "UUID", "ORM", "CDN", "CRUD", "IDE", "UI",
    "UX", "CPU", "GPU", "RAM", "regex", "cron", "localhost", "npm", "pnpm",
    "CI/CD", "WebSocket", "webhook",
    // Workflow & architecture
    "Git", "GitHub", "GitLab", "Bitbucket", "pull request", "merge conflict",
    "refactor", "monorepo", "changelog", "README", "Markdown", "frontend",
    "backend", "middleware", "DevOps", "Kubernetes", "Docker", "Terraform",
    "serverless", "microservices",
    // Languages & runtimes
    "TypeScript", "JavaScript", "Python", "Rust", "Golang", "Java", "Kotlin",
    "Swift", "C#", "C++", "Ruby", "PHP", "Node.js", "Deno", "Bash", "zsh",
    // Frameworks & tools
    "React", "Next.js", "Vue", "Svelte", "Tailwind", "Vite", "Django",
    "Flask", "FastAPI", "Rails", "Laravel", "VS Code", "Xcode", "Vim",
    "Tauri", "Electron",
    // Data & infra
    "PostgreSQL", "Postgres", "MySQL", "SQLite", "MongoDB", "Redis", "Kafka",
    "Nginx", "AWS", "Azure", "GCP", "Vercel", "Netlify", "Supabase",
    "Firebase", "Linux", "Ubuntu", "macOS", "iOS",
    // AI engineering
    "LLM", "RAG", "MCP", "OpenAI", "Anthropic", "Claude", "ChatGPT", "GPT",
    "Whisper", "Hugging Face", "PyTorch", "TensorFlow", "LangChain",
    "embeddings", "fine-tuning", "tokenizer", "inference", "transformer",
    "prompt engineering", "vector database", "agentic",
];

/// Single tokens whose casing gets repaired in the transcript. Strict subset
/// of [`PROMPT_TERMS`]: only tokens that are never a common English word and
/// never deliberately dictated lowercase. "git"/"docker"/"python" are
/// excluded (lowercase is correct in dictated commands), as are "go",
/// "rest", "ram", "swift", "react" (real words).
const CASING_TERMS: &[&str] = &[
    "API", "CLI", "SDK", "JSON", "YAML", "XML", "HTML", "CSS", "SQL", "HTTP",
    "HTTPS", "gRPC", "GraphQL", "OAuth", "JWT", "SSH", "TLS", "DNS", "TCP",
    "UDP", "URL", "URI", "UUID", "ORM", "CDN", "CRUD", "IDE", "UI", "UX",
    "CPU", "GPU", "LLM", "RAG", "MCP", "GPT", "npm", "pnpm", "zsh", "DevOps",
    "GitHub", "GitLab", "Bitbucket", "TypeScript", "JavaScript",
    "PostgreSQL", "Postgres", "MySQL", "SQLite", "MongoDB", "Redis", "Kafka",
    "Nginx", "AWS", "GCP", "Vercel", "Netlify", "Supabase", "Firebase",
    "Kubernetes", "Terraform", "Tauri", "Xcode", "Ubuntu", "Linux", "macOS",
    "README", "Markdown", "OpenAI", "Anthropic", "Claude", "ChatGPT",
    "PyTorch", "TensorFlow", "LangChain", "FastAPI", "Node.js", "Next.js",
    "Vite", "Tailwind", "Svelte",
];

/// Known Whisper split-word outputs and mishears, repaired as whole phrases.
/// Phrase context keeps ambiguous fixes safe: "jason file" is always a JSON
/// file, while a bare "Jason" may be a person.
const PHRASE_PAIRS: &[(&str, &str)] = &[
    ("type script", "TypeScript"),
    ("java script", "JavaScript"),
    ("node js", "Node.js"),
    ("next js", "Next.js"),
    ("vs code", "VS Code"),
    ("get hub", "GitHub"),
    ("git hub", "GitHub"),
    ("get lab", "GitLab"),
    ("git lab", "GitLab"),
    ("dev ops", "DevOps"),
    ("web hook", "webhook"),
    ("web hooks", "webhooks"),
    ("open ai", "OpenAI"),
    ("chat gpt", "ChatGPT"),
    ("chat gbt", "ChatGPT"),
    ("claude code", "Claude Code"),
    ("hugging face", "Hugging Face"),
    ("lang chain", "LangChain"),
    ("fast api", "FastAPI"),
    ("tail wind", "Tailwind"),
    ("post gres", "Postgres"),
    ("mongo db", "MongoDB"),
    ("my sql", "MySQL"),
    ("sql lite", "SQLite"),
    ("py torch", "PyTorch"),
    ("pi torch", "PyTorch"),
    ("tensor flow", "TensorFlow"),
    ("o auth", "OAuth"),
    ("oh auth", "OAuth"),
    ("jason file", "JSON file"),
    ("jason files", "JSON files"),
    ("jason object", "JSON object"),
    ("jason schema", "JSON schema"),
    ("jason response", "JSON response"),
    ("jason payload", "JSON payload"),
    ("jason format", "JSON format"),
    ("jason data", "JSON data"),
    ("jason string", "JSON string"),
    ("sequel query", "SQL query"),
    ("sequel database", "SQL database"),
    ("sequel table", "SQL table"),
    ("read me file", "README file"),
];

/// Repair developer terms in a transcript: phrase fixes first (so "type
/// script" becomes "TypeScript" before any casing pass could touch the
/// pieces), then single-token casing.
pub fn normalize(text: &str) -> String {
    let mut result = text.to_string();
    for (wrong, right) in PHRASE_PAIRS {
        result = replace_phrase_ascii_ci(&result, wrong, right);
    }
    normalize_casing(&result)
}

fn normalize_casing(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for token in corrections::split_preserving_whitespace(text) {
        let (leading, core, trailing) = corrections::split_boundary_punct(&token);
        match CASING_TERMS
            .iter()
            .find(|term| term.eq_ignore_ascii_case(core))
        {
            Some(canonical) if core != *canonical => {
                out.push_str(leading);
                out.push_str(canonical);
                out.push_str(trailing);
            }
            _ => out.push_str(&token),
        }
    }
    out
}

/// Replace every whole-word occurrence of `phrase` (ASCII case-insensitive)
/// with `replacement`. Matching is done on raw bytes so indices stay aligned
/// with the original text — safe because every phrase in [`PHRASE_PAIRS`] is
/// ASCII, and a byte that case-insensitively equals an ASCII letter is
/// itself ASCII (so match edges always land on char boundaries).
fn replace_phrase_ascii_ci(text: &str, phrase: &str, replacement: &str) -> String {
    let haystack = text.as_bytes();
    let needle = phrase.as_bytes();
    if needle.is_empty() || haystack.len() < needle.len() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    while cursor + needle.len() <= haystack.len() {
        if !haystack[cursor..cursor + needle.len()].eq_ignore_ascii_case(needle) {
            let ch_len = text[cursor..].chars().next().map(char::len_utf8).unwrap_or(1);
            out.push_str(&text[cursor..cursor + ch_len]);
            cursor += ch_len;
            continue;
        }
        let end = cursor + needle.len();
        let boundary_before = out
            .chars()
            .next_back()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        let boundary_after = text[end..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        if boundary_before && boundary_after {
            out.push_str(replacement);
            cursor = end;
        } else {
            out.push_str(&text[cursor..cursor + 1]);
            cursor += 1;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// Whisper prompt combining the built-in vocabulary with the user's learned
/// terms. User terms get the character budget first and go at the *end* of
/// the prompt: Whisper truncates long prompts from the front, so the tail is
/// the safe spot.
pub fn build_biased_prompt(user_terms: &[String], max_chars: usize) -> Option<String> {
    let user_part = corrections::build_prompt_from_corrections(user_terms, max_chars);
    let remaining =
        max_chars.saturating_sub(user_part.as_ref().map(|s| s.len() + 2).unwrap_or(0));
    let dev_terms: Vec<String> = PROMPT_TERMS
        .iter()
        .filter(|term| {
            !user_terms
                .iter()
                .any(|u| u.trim().eq_ignore_ascii_case(term))
        })
        .map(|term| term.to_string())
        .collect();
    let dev_part = corrections::build_prompt_from_corrections(&dev_terms, remaining);
    match (dev_part, user_part) {
        (Some(dev), Some(user)) => Some(format!("{dev}, {user}")),
        (Some(dev), None) => Some(dev),
        (None, user) => user,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casing_repairs_acronyms_and_products() {
        assert_eq!(
            normalize("the api returns json over http"),
            "the API returns JSON over HTTP"
        );
        assert_eq!(normalize("push it to github"), "push it to GitHub");
        assert_eq!(normalize("Ci/cd"), "Ci/cd"); // compound token, untouched
        assert_eq!(normalize("deploy to vercel."), "deploy to Vercel.");
    }

    #[test]
    fn casing_lowercases_wrongly_uppercased_lowercase_brands() {
        assert_eq!(normalize("run NPM install"), "run npm install");
        assert_eq!(normalize("my ZSH config"), "my zsh config");
    }

    #[test]
    fn casing_never_touches_common_english_words() {
        assert_eq!(normalize("go to the store"), "go to the store");
        assert_eq!(
            normalize("the rest of the ram in the swift river"),
            "the rest of the ram in the swift river"
        );
        assert_eq!(normalize("git status then docker run"), "git status then docker run");
    }

    #[test]
    fn casing_ignores_substrings_of_longer_words() {
        assert_eq!(normalize("rapid capitulation"), "rapid capitulation");
        assert_eq!(normalize("the essay topic"), "the essay topic");
    }

    #[test]
    fn phrases_repair_split_words() {
        assert_eq!(
            normalize("I write type script and java script"),
            "I write TypeScript and JavaScript"
        );
        assert_eq!(normalize("Get hub actions"), "GitHub actions");
        assert_eq!(normalize("a next js app on vs code"), "a Next.js app on VS Code");
    }

    #[test]
    fn phrases_repair_contextual_mishears() {
        assert_eq!(normalize("open the jason file"), "open the JSON file");
        assert_eq!(normalize("a sequel query"), "a SQL query");
        // Bare "Jason" stays a person.
        assert_eq!(normalize("ask Jason about it"), "ask Jason about it");
    }

    #[test]
    fn phrases_respect_word_boundaries() {
        assert_eq!(normalize("prototype scripting"), "prototype scripting");
        assert_eq!(normalize("stereotype script"), "stereotype script");
    }

    #[test]
    fn prompt_puts_user_terms_last_and_dedupes() {
        let user = vec!["Joost".to_string(), "GitHub".to_string()];
        let prompt = build_biased_prompt(&user, 800).unwrap();
        assert!(prompt.ends_with("Joost, GitHub"));
        assert_eq!(prompt.matches("GitHub").count(), 1);
        assert!(prompt.starts_with("API"));
    }

    #[test]
    fn prompt_user_terms_survive_a_tiny_budget() {
        let user = vec!["FlowingThoughts".to_string()];
        let prompt = build_biased_prompt(&user, 20).unwrap();
        assert!(prompt.ends_with("FlowingThoughts"));
        assert!(prompt.len() <= 20);
        // A user term longer than the whole budget still wins outright.
        let prompt = build_biased_prompt(&user, 15).unwrap();
        assert_eq!(prompt, "FlowingThoughts");
    }

    #[test]
    fn prompt_without_user_terms_is_the_dev_vocabulary() {
        let prompt = build_biased_prompt(&[], 800).unwrap();
        assert!(prompt.starts_with("API, CLI"));
        assert!(prompt.len() <= 800);
    }

    #[test]
    fn casing_terms_are_a_subset_of_prompt_terms() {
        for term in CASING_TERMS {
            assert!(
                PROMPT_TERMS.iter().any(|p| p.eq_ignore_ascii_case(term)),
                "{term} is normalized but never prompt-biased"
            );
        }
    }
}
