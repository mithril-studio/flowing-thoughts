# Open Voice Wispr

Desktop voice dictation app for macOS, built with Tauri + React.

Current interaction model:
- Hold `fn` to start a session
- Release `fn` to stop and trigger transcription pipeline
- Render completed transcript in the Home timeline

The app is in active build. Audio capture, Whisper API calls, and text injection are still being finalized.

## Current Status

Implemented:
- Tray app + settings window shell
- Session lifecycle state machine (`idle -> recording -> transcribing -> idle`)
- Frontend event wiring for session phases and completed transcripts
- Timeline UI for completed transcripts

In progress:
- Real microphone capture (`audio.rs`)
- Whisper API transcription (`transcribe.rs`, currently uses `OPENAI_API_KEY`)
- macOS text injection into focused apps (`text_inject.rs`)
- Onboarding gates (license, OpenAI key, accessibility checks)

## Architecture

See [Speech Pipeline Architecture](./docs/ARCHITECTURE.md).

## Project Structure

- `src-tauri/src/lib.rs`: backend bootstrap + session orchestrator
- `src-tauri/src/hotkey.rs`: global hotkey listener
- `src-tauri/src/audio.rs`: audio capture module (WIP)
- `src-tauri/src/transcribe.rs`: transcription worker (WIP)
- `src-tauri/src/text_inject.rs`: text injection module (WIP)
- `src/pages/Home.tsx`: session status + transcript timeline

## Development

Requirements:
- Node.js + npm
- Rust toolchain
- Tauri CLI dependencies for macOS

Commands:
```bash
npm install
npm run tauri dev
```

Build checks:
```bash
npm run build
cd src-tauri && cargo check
```
