//! Transcription-quality evaluation: dataset, scoring, harness, collection.
//!
//! Measurement infrastructure only — nothing here changes how dictation
//! behaves. Design and workflow: `docs/DUTCH_EVAL.md`. Entry point:
//! `cargo run --release --example ft_eval -- <command>` (`npm run eval -- …`).
//!
//! The one piece the app itself calls is [`keep`], behind an opt-in setting.

pub mod cli;
pub mod harness;
pub mod keep;
pub mod manifest;
pub mod mine;
pub mod normalize;
pub mod prompts;
pub mod record;
pub mod report;
pub mod score;
pub mod synth;
