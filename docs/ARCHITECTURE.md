# Speech Pipeline Architecture

## Goal

Build a reliable "hold to record, release to type" dictation pipeline for macOS:

1. User presses global hotkey
2. App records audio
3. User releases hotkey
4. App transcribes speech
5. App injects text into the currently focused app

## Top-Level Flow

```text
Hotkey (global)
  -> Session Orchestrator (state machine)
    -> Audio Engine (capture/finalize)
      -> Transcription Worker (Whisper)
        -> Text Injection Engine (CGEvent/paste fallback)
          -> UI events + local history
```

## Modules and Responsibilities

### `src-tauri/src/lib.rs`
- Owns session lifecycle
- Handles hotkey start/stop events
- Emits frontend events:
  - `session-phase`
  - `transcription-complete`
  - `pipeline-error`
- Guards invalid transitions (no double-start, no stop when idle)

### `src-tauri/src/hotkey.rs`
- Captures global key events
- Emits only domain events:
  - `RecordStart`
  - `RecordStop`

### `src-tauri/src/audio.rs` (next)
- Start and stop microphone capture
- Write WAV/PCM output for STT worker
- Keep real-time callback path minimal and lock-safe

### `src-tauri/src/transcribe.rs` (current placeholder, next real Whisper)
- Accept finalized audio from audio module
- Call OpenAI audio transcription API
- Return transcript text + metadata

### `src-tauri/src/text_inject.rs` (next)
- Check/access Accessibility permissions
- Inject transcript in focused target app
- Provide robust fallback mode (clipboard paste)

## Session State Machine

Current states:
- `Idle`
- `Recording { session_id }`
- `Transcribing { session_id }`

Current transitions:
- `Idle -> Recording` on hotkey press
- `Recording -> Transcribing` on hotkey release
- `Transcribing -> Idle` on completion/error

Future states:
- `Injecting { session_id }`
- `Error { session_id, reason }`

## Frontend Event Model

`Home.tsx` listens to:
- `session-phase` to render live state in `RecordingIndicator`
- `transcription-complete` to prepend entries in timeline
- `pipeline-error` to display error status

This keeps UI fully reactive to backend lifecycle, rather than local UI assumptions.

## Immediate Implementation Plan

1. Implement `audio.rs` to produce WAV buffers per session.
2. Replace placeholder transcription with real Whisper API call.
3. Add text injection and permission checks.
4. Extend state machine with `Injecting`.
5. Persist transcript history and settings with Tauri store.

## Reliability Rules

- Every in-flight task is scoped to `session_id`.
- Late results for old sessions are ignored.
- Backend owns truth of state; frontend only renders events.
- Errors never leave app in a non-idle terminal state.
