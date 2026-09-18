# Local Dictation & Meeting Transcription Roadmap

> **Historical planning snapshot, preserved on 2026-09-18.** The original
> assessment below predates the meetings implementation and is not a current
> status report. Strict local mode and meeting recording/transcription have
> since shipped; meetings were pulled ahead of several evaluation/refactoring
> phases. See [MEETINGS.md](MEETINGS.md) for current behavior,
> [ARCHITECTURE.md](ARCHITECTURE.md) for implementation details, and
> [RELEASING.md](RELEASING.md) for release status. The remaining proposals are
> ideas to re-evaluate, not approved implementation commitments.

## Goal

Build a macOS dictation and meeting-transcription app that is fully functional offline, with optional bring-your-own-key (BYOK) cloud processing.

**Local mode must never silently upload audio or text.** Cloud transcription and cloud summarization must each be explicitly selected.

## Current assessment

The existing Tauri/React/Rust stack is a useful dictation foundation. Keep it rather than rewrite it.

Strengths:
- Native macOS hotkey handling and backend-owned session state.
- Local Whisper inference off the async executor, with cached models.
- Verified model downloads and SQLite migrations.
- Local and cloud transcription implementations.
- Regression tests for corrections, hotkeys, and hallucination guards.

Main limitations:
- Local mode currently falls back to cloud if the local model is missing and an API key is configured (`src-tauri/src/lib.rs`).
- A five-word minimum discards legitimate short utterances. Other text filters can also reject real speech.
- Audio capture buffers the entire recording in memory and clones it on stop (`src-tauri/src/audio.rs`).
- Transcription returns flattened text rather than timestamped segments (`src-tauri/src/local_transcribe.rs`).
- API keys are stored as ordinary JSON values in SQLite (`src-tauri/src/storage.rs`).
- Some logs contain rejected transcripts and learned corrections.
- Session orchestration, routing, persistence, commands, and setup are concentrated in the approximately 2,200-line `lib.rs`.
- Dutch-capable local models currently stop at quantized Whisper Base and Small.
- The coaching feature is cloud-only; it is not an offline summarization foundation.

Verification during inspection:
- `cargo check`: passed.
- `cargo test`: 54 passed, 1 ignored (model-dependent VAD integration test).
- Frontend build/tests: not verified; build was blocked by missing dependencies (`tsc` unavailable).
- No microphone, diarization, or transcription-quality benchmark was performed.

## Principles

- Preserve the native CGEventTap listener and its tap re-enable logic.
- Keep backend session state authoritative; frontend renders events.
- Do not reintroduce multi-model fan-out into interactive dictation.
- Reuse transcription execution, not the complete dictation workflow.
- Meeting transcription must never auto-paste or inherit dictation word-count filters.
- Background processing should be unobtrusive, not hidden recording.
- Preserve original transcripts separately from corrections and generated summaries.
- Prefer measured improvements over additional blanket text-rejection heuristics.

## Phase 1 — Trustworthy local processing

### Work

- Remove automatic local-to-cloud fallback. Missing models should produce a download prompt or actionable error.
- Make audio and text cloud-processing choices explicit and separate.
- Move API keys to macOS Keychain, including migration of existing keys.
- Define retention and deletion behavior for audio, transcripts, corrections, and diagnostics.
- Redact transcript content from routine logs.
- Handle audio files left behind after crashes.

### Acceptance criteria

- Selecting local mode never sends audio or transcript content to a cloud provider, including when models are missing.
- Core dictation works offline once required models are installed.
- Secrets are no longer retained in ordinary SQLite values after migration.
- Retention and deletion behavior is documented and tested.

## Phase 2 — Dutch evaluation and lost-speech fixes

### Build an evaluation dataset first

Start with approximately 200–500 manually verified clips covering:
- Short replies and ordinary Dutch sentences.
- Names, places, compounds, numbers, dates, and times.
- Dutch mixed with English technical terms.
- Different microphones, quiet speech, clipping, and background noise.
- Silence, breathing, typing, and other non-speech.
- Multiple accents and speakers if the product targets more than personal use.

For each sample, retain audio, a manually checked reference, and relevant capture metadata. Make audio retention explicitly opt-in. Existing correction history can suggest examples, but is not reliable ground truth or a paired speech dataset; current audio is deleted after transcription.

Keep a held-out test set. Do not tune every decision against the entire dataset.

### Metrics

- Word error rate and character error rate.
- Names and numbers accuracy.
- Real speech incorrectly discarded.
- Text hallucinated on non-speech.
- End-to-end latency and memory use.

### Work

- Measure the current baseline before changing behavior.
- Replace the five-word rejection rule with an evaluated approach that preserves legitimate short speech.
- Evaluate other phrase and prompt-echo filters for false positives.
- Use explicit `nl` mode for Dutch-only dictation; evaluate mixed Dutch/English separately.
- Review auto-language behavior: it currently may perform a second decode for unsupported detected languages, but does not resolve Dutch misdetected as English.

### Acceptance criteria

- Legitimate short replies such as “Ja, dat klopt” and “Morgen om drie uur” survive.
- Silence hallucinations and dropped speech are measured independently.
- Regression fixtures cover both speech preservation and non-speech rejection.

## Phase 3 — Models, audio preprocessing, and vocabulary

### Work

- Benchmark current quantized Small against larger multilingual candidates, such as Medium and large-v3/turbo variants supported by the runtime.
- Measure on target Macs; compare accuracy, latency, memory, and sustained resource usage.
- Evaluate a production-quality anti-aliasing resampler instead of the current linear interpolation to 16 kHz.
- Compare no vocabulary prompt, personal vocabulary, and developer vocabulary.
- Make developer vocabulary appropriate to the user's context rather than assuming it improves all Dutch dictation.

### Acceptance criteria

- Model and preprocessing choices are supported by the evaluation set.
- User-facing model choices explain the quality/resource trade-off.
- No silent LLM rewriting of verbatim dictation.

### Fine-tuning decision

Do not begin with training a custom model. Consider fine-tuning only if measured, persistent errors remain after improving model choice, capture, language settings, and vocabulary handling. Recheck deployment compatibility and licensing before investing in training.

## Phase 4 — Extract reusable structured transcription

### Work

Separate focused responsibilities from `src-tauri/src/lib.rs`:
- Dictation session orchestration.
- Transcription execution and routing policy.
- Meeting recording and processing lifecycle.

Introduce a structured transcription result retaining:
- Segment start/end timestamps.
- Text.
- Source track.
- Optional speaker reference.
- Model and processing provenance.

Retain word timestamps when supported and needed for speaker alignment. Keep speaker attribution separate from raw speech recognition.

### Acceptance criteria

- Existing dictation behavior remains covered by tests.
- Meeting code can reuse inference without invoking injection or dictation-only filters.
- Timestamped results survive persistence rather than being flattened irreversibly.

## Phase 5 — Meeting recording and post-meeting transcription

### Initial scope

Ship reliable recording followed by transcription after stopping. Defer live background decoding until capture and persistence are dependable.

### Capture

- Capture microphone and system audio as separate timestamped tracks.
- Investigate ScreenCaptureKit or Core Audio process taps according to supported macOS versions.
- Preserve a shared timeline and account for source synchronization and echo.
- Write incrementally to disk with bounded memory buffers.
- Keep recording reliable even if later processing is slower than real time.

Separate tracks help distinguish local microphone speech from remote audio; they do not identify individual remote participants in a mixed stream.

### UX and privacy

- Explicit start/stop and pause controls.
- Persistent menu-bar recording indicator and duration.
- Appropriate participant notice/consent.
- Transcript window may remain closed; recording must not be covert.

### Processing

- Speech detection and speech-boundary chunking.
- Overlap and duplicate reconciliation where needed.
- Persistent jobs and completed chunks for crash recovery.
- Audio retention/deletion settings.

### Acceptance criteria

- Long recordings do not accumulate unbounded in-memory audio.
- Recovery preserves completed recording/processing work after interruption.
- Transcription produces timestamped segments without auto-pasting.
- Test permissions, device loss, disk-full conditions, and interrupted sessions.

## Phase 6 — Speaker attribution

### Distinguish the capabilities

- Transcription: what was said.
- Diarization: which speaker spoke when.
- Identification: which real person a speaker represents.

Whisper alone is not a named-speaker attribution system.

### Work

- Prototype a local diarization model that produces speaker-labelled time ranges.
- Evaluate a stack such as pyannote, checking licensing, model download requirements, macOS performance, and desktop packaging before selecting it.
- Align transcript words/segments with speaker time ranges.
- Allow users to rename “Speaker 1,” merge speakers, and correct assignments.
- Represent overlapping or uncertain speech rather than assigning confident but unsupported names.

### Acceptance criteria

- Speaker labels are editable and persist across the meeting transcript.
- Corrections can be propagated to downstream summaries.
- No promise of automatic real names from mixed system audio.
- Participant metadata integrations and voice enrollment remain separate, optional scope.

## Phase 7 — Local summaries and action items

### Work

- Introduce a local LLM path, initially through a runtime such as llama.cpp or Ollama.
- Offer optional BYOK providers behind the same summarization interface.
- For long meetings, summarize sections and consolidate decisions/actions.
- Keep the original transcript separate from generated interpretations.
- Link summary statements and action items to supporting transcript segments.

Action-item fields:
- Task.
- Owner, or unknown.
- Due date, or unknown.
- Source segment IDs.

### Acceptance criteria

- Summarization works without a cloud key when a suitable local model is installed.
- Missing owners and deadlines remain unknown rather than being invented.
- Users can navigate from generated claims to supporting transcript evidence.
- Meeting content is treated as untrusted input; extracted tasks are not automatically executed.
- Resource usage is evaluated alongside the speech models.

## Phase 8 — Transcription during recording

### Work

- Add a bounded background queue consuming persisted audio chunks during recording.
- Allow processing to lag without compromising capture.
- Prioritize interactive dictation over meeting processing.
- Limit concurrent inference and model residency according to a memory/resource budget.
- Reconcile chunk boundaries and provisional speaker assignments.

### Acceptance criteria

- Sustained long-meeting tests show bounded memory and responsive dictation.
- Pausing or failing processing does not lose captured audio.
- Users can distinguish provisional results from finalized results where relevant.

## Recommended execution order

1. Strict local mode, secure keys, and retention.
2. Dutch evaluation dataset and dropped-speech fixes.
3. Model, resampling, and vocabulary benchmarks.
4. Structured transcription and focused orchestration extraction.
5. Meeting recording and post-meeting transcription.
6. Speaker labels and manual naming.
7. Evidence-linked local summaries and action items.
8. Live background processing.

## Verification for implementation increments

Follow the repository's existing checks:

```bash
npm install
npm run build
npm test
cd src-tauri && cargo check && cargo test
```

Supplement unit tests with opted-in audio fixtures, model-dependent integration tests, and manual macOS capture/permission checks. Passing text-processing tests alone does not establish transcription accuracy or meeting-recording reliability.
