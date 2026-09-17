# FlowingThoughts — Agent Guide

macOS voice dictation app (free, open source, local-first). Tauri 2 + React 19 + TypeScript + Tailwind 4; Rust backend with whisper.cpp for on-device transcription.

## Commands

```bash
npm install                # once
npm run tauri dev          # run the app
npm run build              # typecheck + bundle frontend (must pass)
npm test                   # vitest (must pass)
cd src-tauri && cargo check && cargo test   # backend (must pass)
npm run eval -- help       # offline transcription-quality eval (docs/DUTCH_EVAL.md)
```

## Rules

- Local transcription is the default path; the cloud API (Groq/OpenAI) is optional. Never make an API key a hard requirement.
- Exactly one transcription runs per dictation. Do not reintroduce multi-model fan-out on the hot path.
- The hotkey listener is native CGEventTap (`macos_hotkey.rs`) — rdev crashes on macOS 26+. Keep the tap re-enable logic intact.
- Session state lives in the backend; the frontend only renders events (`session-phase`, `recording-amplitude`, `transcription-complete`, `pipeline-error`).
- UI style: dark zinc palette, rounded-xl/2xl cards, shadcn-like. Both windows are transparent — keep `html/body` backgrounds transparent.
- Transcription changes (filters, resampling, model defaults, prompts) are judged with the eval harness, not by feel. The pure dictation stages live in `pipeline.rs` so the harness measures the real code path — keep it that way. Eval audio is personal data: never commit it.
- Test real behavior; after each meaningful change run the checks above before committing.

## Architecture

See `docs/ARCHITECTURE.md`. Key files are listed in `README.md` → Project structure.
