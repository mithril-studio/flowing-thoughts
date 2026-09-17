//! Eval tooling entry point: `cargo run --release --example ft_eval -- <command>`
//! (or `npm run eval -- <command>`). An example rather than a second binary so
//! app builds and bundles are unaffected. See docs/DUTCH_EVAL.md.

const PROMPTS: &str = include_str!("../../eval/prompts-nl.txt");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = flowing_thoughts_lib::eval::cli::main(&args, PROMPTS) {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}
