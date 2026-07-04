# Speech Pipeline Architecture

## Goal

A reliable "hold to record, release to type" dictation pipeline for macOS:

1. User presses the global hotkey — recording starts **immediately** (no arming delay)
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
- Handles hotkey start/stop events; recordings shorter than `MIN_DICTATION_MS`
  are discarded silently as accidental taps
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

### `src-tauri/src/model_manager.rs`
- Downloads GGML models from Hugging Face with SHA-256 verification
- Models live in `~/Library/Application Support/FlowingThoughts/models/`

### `src-tauri/src/transcribe.rs`
- Optional cloud path: Groq (`whisper-large-v3-turbo`) or OpenAI (`whisper-1`)
- Retries once on 429/5xx

### `src-tauri/src/corrections.rs` + `ax_snapshot.rs`
- After injection, the focused field is re-read (Accessibility API) at the
  start of the next session; single-word diffs are stored as corrections
- Corrections are applied to future transcripts (word-boundary,
  case-insensitive) and fed into the Whisper prompt

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
