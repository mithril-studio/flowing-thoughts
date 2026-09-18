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
- Used only when the user explicitly selects the API provider. Local mode
  never falls back to it: a missing local model is a `pipeline-error`, even
  when an API key is configured (`pipeline::choose_route`)
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

### `src-tauri/src/pipeline.rs`
- The pure stages of a dictation: capture gate (tap/silence), transcription
  route (local, cloud, or "model missing"), vocabulary prompt, transcript
  filters (marker/credit sanitizer, prompt-echo, five-word
  floor) and finalisation (developer dictionary, learned corrections)
- Called by the live session in `lib.rs` *and* by the eval harness, so
  measured results describe what dictation really does

### `src-tauri/src/eval/` + `examples/ft_eval.rs`
- Offline transcription-quality evaluation: dataset manifest, Dutch scoring
  (WER/CER/entities), harness, recorder, opt-in "keep my dictations"
- Measurement only — see `docs/DUTCH_EVAL.md`. Run with `npm run eval -- help`

## Meetings

A second product on the same engine: record a call as two tracks, transcribe
it locally after stop. Off by default (`settings.meetings.enabled`). User-facing
behaviour, privacy model and the release checklist are in `docs/MEETINGS.md`.

```text
Meetings tab (start / pause / stop)
  -> session.rs (state machine, one meeting at a time)
    -> capture/ (mic via cpal, system audio via a Core Audio process tap)
      -> recording/ (ring -> resample to 16 kHz mono -> timeline -> 60 s chunk files)
  stop -> jobs.rs (transcript run + job row in SQLite)
    -> worker.rs (one `meeting-worker` thread)
      -> longform.rs (VAD windows <= 28 s, Whisper, flagging)  <- inference gate
        -> echo.rs (hide mic segments that repeat the system track)
          -> meeting `ready`; optional summary.rs (opt-in, BYOK), export.rs
```

Meetings do not inherit dictation's limits or filters: no 5-minute cap, no
whole recording in RAM, no five-word floor, no `sanitize_transcript`, no
injection guard, and never the cloud transcription route.

### Module map (`src-tauri/src/meetings/`)

| Path | What |
|---|---|
| `mod.rs` | `init` (launch recovery, then worker start), `idle_tray_title`, `shutdown` |
| `types.rs` | The contracts: string enums stored as text in the schema, DTOs, and the `AudioSource` / `AudioSourceHandler` / `SampleSink` / `ChunkLedger` / `TrackAudio` traits the packages meet at |
| `commands.rs` | Every Tauri command, as thin delegates |
| `events.rs` | `meeting-state` (every phase change), `meeting-job-progress` (throttled, per window), `meeting-updated` (refetch this meeting). The backend owns state; React only renders |
| `session.rs` | The recording state machine `idle -> starting -> recording <-> paused -> stopping -> idle`. `start` inserts the meeting, its tracks and the "Me"/"Them" speakers, builds sources and recorders, and bridges the chunk ledger to the store; no system audio means a mic-only meeting, never an error. `stop` drains and closes the chunks, then queues the transcription. Also owns the tray title (recording dot plus duration; dictation's title wins while a dictation is active), launch recovery, and deleting a meeting or only its audio |
| `capture/` | The real `AudioSource`s. `mic.rs`: cpal 0.15, same input device as dictation, runs next to a dictation capture. `system_tap.rs`: global stereo process tap on an aggregate device, rebuilt when the default output changes. `permission.rs`: System Audio Recording (a denial is silence, not an error, so: best-effort TCC preflight behind the `private-tcc` feature, plus a zeros watchdog). `device_watch.rs`: default-device listeners and "is the output the built-in speakers" (`meetings.echo_risk`). `is_supported()` is the runtime gate: macOS 14.4+, `CATapDescription` present, both tap symbols resolvable |
| `recording/` | Frames to disk. `recorder.rs` (ring, writer thread, pause/resume/stop), `resample.rs` (any format to 16 kHz mono), `timeline.rs` (runs, gaps, drift, chunk anchors; pure), `chunk_writer.rs`, `sidecar.rs` (`track.json`), `recovery.rs`, `reader.rs` (`TrackAudio` over chunk files; gaps and deleted chunks read as silence) |
| `store/` | The only place meeting SQL lives; typed access to every v3 table, usable from the UI connection and the worker's own |
| `jobs.rs` | The SQLite-backed queue: enqueue, "Re-transcribe as…", launch requeue |
| `worker.rs` | The `meeting-worker` thread |
| `longform.rs` | Window planning, per-track language, window decoding, segment flagging. Whisper only: Parakeet has no timestamps |
| `echo.rs` | Text-level echo flagging between tracks, biased towards keeping what the user said |
| `summary.rs` | Opt-in OpenRouter summary; the one place meeting text leaves the machine. Items must cite existing segments; the transcript is treated as untrusted input |
| `export.rs` | A meeting as Markdown |

The tap symbols (`AudioHardwareCreateProcessTap` / `DestroyProcessTap`) are
resolved with `dlsym`, never linked, so the binary still loads where they do
not exist. `scripts/release.sh` asserts that `nm -um` shows no `ProcessTap`
import. cpal stays on 0.15 for the same reason: 0.17+ hard-links them.

### On-disk layout

```text
~/Library/Application Support/FlowingThoughts/
  flowing_thoughts.db                      all meeting rows (schema v3)
  meetings/<meeting_id>/<mic|system>/
    <seq>.pcm                              raw s16le, 16 kHz, mono, 60 s per chunk
    track.json                             the chunk list, rewritten atomically
```

- Raw PCM has no header to finalize. The writer calls `write()` at least once
  a second and `fsync`s when a chunk closes, because the app exits through
  `_exit(0)` and may crash. About 230 MB per hour for two tracks.
- One `meeting_audio_chunks` row per chunk: `open`, then `closed`. After a
  crash an `open` chunk becomes `recovered` with `n_frames = file_len / 2`.
  `track.json` allows the same repair without the database.
- Each chunk is anchored to mach host time, the clock both tracks share.
  Pauses, ring overflow and device rebuilds are recorded as gaps, so segment
  timestamps stay on the meeting timeline. Memory is bounded by about 5 s of
  ring buffer per track; the audio callback only copies into the ring.
- Chunk rows store paths relative to the meetings root. Meeting ids are
  validated before they become directory names, because deleting a meeting's
  audio removes that directory.

Schema v3 groups: recording (`meetings`, `meeting_tracks`,
`meeting_audio_chunks`), transcript (`transcript_runs`, `transcript_windows`,
`transcript_segments`, `segment_edits`), speakers and people (`speakers`,
`speaker_turns`, `segment_speakers`, `people`, `participants`,
`speaker_assignments`), summaries (`summaries`, `summary_items`,
`summary_item_sources`) and `jobs`. Segments are immutable except for
`suppressed_reason`; displayed text is `COALESCE(edit.text, seg.text)`. v1 only
seeds the "Me" and "Them" track speakers; the people tables are there for
per-person labels later.

### Jobs and the worker

- A transcription is a `transcript_runs` row (model, language, decode
  parameters) plus a `jobs` row pointing at it, inserted together. Re-running
  never destroys results: a first run is the meeting's active run from the
  start so the transcript fills in live; a re-run replaces the old one only
  once it is complete.
- Nothing about the queue lives in memory. At launch, meetings left in
  `recording` or `paused` become `interrupted`, their chunks are repaired and
  a job is queued (`session::init`); `running` jobs go back to `queued`; then
  the worker starts.
- The worker is one thread with its own connection (`db::open_connection()`,
  WAL plus `busy_timeout`). It never takes the managed UI connection's mutex.
- Per job: plan windows for all tracks in one transaction (VAD over 5-minute
  blocks, speech packed into windows of at most 28 s, breaking at silences
  over 3 s), then decode the `pending` windows in timeline order. A window is
  the unit of resume: its segments and its `done` state commit together, so a
  restart never decodes a window twice.
- A window that errors is retried up to `MAX_WINDOW_ATTEMPTS` (3), then marked
  `failed` and the job goes on. A job can be claimed at most 8 times, which
  stops a window that takes the process down from doing so at every launch.
- When the run is settled: echo pass, run `done`, run becomes active, meeting
  `ready`. The worker also applies `auto_delete_audio_days`.
- Everything outside SQLite sits behind a `Host` trait (model, VAD, audio,
  events, settings), so tests drive whole jobs with fakes and no thread.

Meeting status: `recording`, `paused`, `interrupted`, `queued`,
`transcribing`, `ready`, `failed`.

### Inference gate (`src-tauri/src/inference_gate.rs`)

Dictation and the worker share the CPU/GPU and the loaded model, so there is
one local inference at a time, with dictation first in line.

- Dictation calls `acquire_interactive()` on its blocking thread, around the
  single transcription (`local_transcribe.rs`). That raises the preempt flag
  at once, then waits for whoever holds the gate.
- The worker calls `acquire_background()` around each model call only (one
  window, or language detection), never around reading audio, VAD or planning,
  and never for a whole job. It waits while the gate is held *or* an
  interactive caller is waiting, so dictation never queues behind a second
  window.
- Whisper's abort callback polls `should_preempt()`. A preempted decode drops
  its guard and leaves the window `pending` with no attempt counted; the
  worker then waits until dictation is done. Between model calls the worker
  also yields when the flag is up.
- Worst case for dictation is one abort-check interval, not one window. The
  rule "exactly one transcription per dictation" is unchanged. The eval
  harness stays outside the gate.

## Signing and Permissions

macOS keys privacy permissions (Microphone, Accessibility, Input Monitoring,
System Audio Recording) to the code signature.

- The release pipeline requires a Developer ID certificate
  (`APPLE_SIGNING_IDENTITY`) and notarizes/staples both the app and DMG.
  A stable bundle identifier and team ID are intended to preserve permissions
  between Developer ID releases; the first transition from an ad-hoc build may
  require re-granting them. The 0.5.1 tag failed certificate import and was not
  published. See `docs/RELEASING.md` for current status and validation.
- `bundle.macOS.hardenedRuntime` is `true`, as notarization requires. Under
  it macOS refuses the microphone without prompting unless the app carries
  the `com.apple.security.device.audio-input` entitlement, so
  `src-tauri/Entitlements.plist` must keep it.
- `scripts/release.sh` requires the Developer ID and notarization variables
  and stops before building if one is missing. Disabling Developer ID in this
  publishing path is forbidden; local builds use `npx tauri build` separately.
- `scripts/release.sh` asserts, before anything is published: strict verify
  passes, the signature identifier equals the bundle identifier, the bundle
  is signed by the configured Developer ID with the hardened runtime, both
  usage descriptions are in the bundled Info.plist, no `ProcessTap` symbol is
  linked, the hardened runtime is not on without the audio-input entitlement,
  and the app inside the updater tarball is the same signed code as
  the one in the DMG.

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
