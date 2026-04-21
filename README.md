# FlowingThoughts

Desktop voice dictation app for macOS, built with Tauri + React.

Current interaction model:
- Hold `Cmd+Shift+Space` to start a session
- Release the combo to stop and trigger transcription pipeline
- Render completed transcript in the Home timeline

## Current Status

Implemented:
- Tray app + settings window shell
- Session lifecycle state machine (`idle -> recording -> transcribing -> idle`)
- Frontend event wiring for session phases and completed transcripts
- Timeline UI for completed transcripts
- First-run onboarding flow (license, API key, accessibility check, injection test)
- Microphone capture (`audio.rs`)
- Whisper API transcription (`transcribe.rs`)
- macOS text injection into focused apps (`text_inject.rs`, clipboard + paste with clipboard preservation)
- Onboarding gates (license, OpenAI key, accessibility checks)

## Architecture

See [Speech Pipeline Architecture](./docs/ARCHITECTURE.md).
For manual cross-app verification, use [Compatibility Matrix](./docs/COMPATIBILITY.md).

## Project Structure

- `src-tauri/src/lib.rs`: backend bootstrap + session orchestrator
- `src-tauri/src/hotkey.rs`: global hotkey listener
- `src-tauri/src/audio.rs`: audio capture module
- `src-tauri/src/transcribe.rs`: transcription worker
- `src-tauri/src/text_inject.rs`: text injection module
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

Hotkey override for development:
```bash
OVW_HOTKEY=fn npm run tauri dev
```

Build checks:
```bash
npm run build
cd src-tauri && cargo check
```
