# Speech Pipeline Architecture

## Goal

A reliable "hold to record, release to type" dictation pipeline for macOS:

1. User presses the global hotkey — audio capture starts **immediately** (no
   speech is lost), but the session only *commits* (● feedback, transcription
   eligibility) after the key has been held for 500 ms. Shorter presses are
   discarded silently, so accidental taps never paste anything.
2. App records audio while the key is held
3. User releases the hotkey
4. App transcribes speech with exactly one model (local by default)
5. App applies learned corrections, then injects text into the focused app

## Top-Level Flow

```text
Hotkey (CGEventTap, global)
  -> Session Orchestrator (state machine, lib.rs)
    -> Audio Engine (cpal capture -> WAV)
      -> Transcription (local whisper.cpp OR Groq/OpenAI API)
        -> Corrections (learned word replacements + Whisper prompt bias)
          -> Text Injection (clipboard paste, clipboard preserved)
            -> UI events + SQLite history
```

## Modules

### `src-tauri/src/lib.rs`
- Owns session lifecycle: `Idle -> Recording -> Transcribing -> Injecting -> Idle`
- Handles hotkey start/stop events; capture starts at key-down but the session
  commits only after `HOLD_TO_COMMIT_MS` (500 ms) of hold — earlier releases
  and captures shorter than `MIN_DICTATION_MS` are discarded silently
- Emits frontend events: `session-phase`, `recording-amplitude`,
  `transcription-complete`, `pipeline-error`

### `src-tauri/src/macos_hotkey.rs`
- Native CGEventTap listener (rdev crashes on macOS 26+)
- Re-enables the tap when macOS disables it by timeout — without this the
  hotkey silently dies until app restart
- Emits only domain events: `RecordStart` / `RecordStop`

### `src-tauri/src/audio.rs`
- Start/stop microphone capture, WAV output per session
- Publishes peak amplitude for the waveform UI

### `src-tauri/src/local_transcribe.rs`
- On-device Whisper via whisper.cpp (Metal-accelerated)
- Multilingual models (`whisper-small-q5`, `whisper-base-q5`) support
  English + Dutch + auto-detect; `.en` models force English
- Learned terms are passed as the Whisper initial prompt to bias decoding
- Parakeet TDT 0.6B v3 runs on ONNX Runtime (CPU, via `transcribe-rs`). It
  detects the language itself and takes no prompt, so the language setting
  and vocabulary hints do not apply to it
- The selected model is loaded and warmed in the background at launch and
  whenever the selection changes; Whisper contexts use flash attention

### `src-tauri/src/model_manager.rs`
- Downloads GGML models from Hugging Face with SHA-256 verification
- Parakeet is a directory of four ONNX files, each hash-pinned, downloaded
  as one model
- Models live in `~/Library/Application Support/FlowingThoughts/models/`
- Built-in catalog: large-v3-turbo (q5_0), small, base, plus English-only
  tiny/base/distil-small. Any other `ggml-*.bin` in the directory is listed
  as a custom model (`ModelId::Custom`), selectable and deletable like the rest
- Custom models are added by whisper.cpp catalog name (`medium-q5_0`, resolved
  against ggerganov/whisper.cpp) or a direct `.bin` URL; Hugging Face's
  `x-linked-etag` header supplies the SHA-256 when available
- Catalog entries the user doesn't want can be removed from the picker
  (`hidden_models` in the kv table) and restored later; downloaded files are
  deleted outright

### `src-tauri/src/transcribe.rs`
- Optional cloud path: Groq (`whisper-large-v3-turbo`) or OpenAI (`whisper-1`)
- Retries once on 429/5xx

### `src-tauri/src/corrections.rs` + `ax_snapshot.rs`
- After injection, the focused field is re-read (Accessibility API) at the
  start of the next session (within 10 minutes). A word-level LCS diff turns
  every substitution hunk of up to 3 words per side into a correction;
  pure insertions/deletions are skipped, and texts sharing under 50% of
  their words are rejected as unrelated. Every outcome is written to
  logs.txt ("Correction check ..." / "Learned N correction(s) ...")
- Corrections are applied to future transcripts (whole-word or whole-phrase,
  case-insensitive, longest match first) and fed into the Whisper prompt

### `src-tauri/src/text_inject.rs`
- Clipboard-paste injection with clipboard preservation

## Reliability Rules

- Every in-flight task is scoped to `session_id`; late results for old
  sessions are ignored
- Exactly one transcription runs per dictation (a 4-model fan-out used to
  saturate CPU/RAM and freeze the UI)
- Backend owns truth of state; frontend only renders events
- Errors never leave the app in a non-idle terminal state

## UI Surfaces

- `main` window — the settings/history window (transparent, rounded shell)
- Menu bar (tray) icon — its presence means the hotkey is armed; the title
  mirrors the session phase (● recording, … transcribing/typing)
