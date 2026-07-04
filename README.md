# FlowingThoughts

**Free, open-source voice dictation for macOS.** Hold a key, speak, release — your words are typed into whatever app you're using. A privacy-first alternative to Wispr Flow.

- **Local-first** — speech recognition runs entirely on your Mac (Whisper via whisper.cpp). No account, no subscription, no audio leaves your machine.
- **Bilingual** — English and Dutch out of the box, with auto-detection. The recommended model is 190 MB and runs in under 0.5 GB RAM.
- **Learns from you** — fix a word once and FlowingThoughts applies that correction to every future dictation.
- **Works everywhere** — hold `Fn` (or `⌘⇧Space`) in any app: Slack, Mail, your editor, a browser. The menu bar icon shows ● while recording and … while transcribing.

## Install

1. Download the latest `.dmg` from [Releases](https://github.com/mithril-studio/flowing-thoughts-releases/releases/latest) and drag FlowingThoughts into Applications.
2. First launch: the app is not notarized yet, so right-click the app → **Open** → **Open** to get past Gatekeeper.
3. Follow the 3-step setup: download the speech model (190 MB, one-time), grant **Accessibility** and **Input Monitoring** in System Settings → Privacy & Security (the app shows live green badges when they're set), and run the injection test.
4. Hold `Fn`, speak, release — your words are typed into whatever app you're in.

Optional: bring your own Groq or OpenAI API key (Settings → Transcription → Cloud API) for cloud transcription instead of the local model.

## How it works

```
Global hotkey (Fn, hold)
  → microphone capture (cpal, WAV)
    → Whisper transcription (local whisper.cpp, or Groq/OpenAI API)
      → learned corrections applied
        → text injected into the focused app (clipboard paste, clipboard restored)
```

See [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) for details and
[docs/COMPATIBILITY.md](./docs/COMPATIBILITY.md) for the cross-app verification matrix.

## Local models

| Model | Size | Languages | Notes |
|---|---|---|---|
| Whisper Small q5 | 190 MB | EN + NL (+97 more) | **Recommended** — best quality under 0.5 GB RAM |
| Whisper Base q5 | 60 MB | EN + NL (+97 more) | Fastest multilingual option |
| Whisper Tiny/Base/Distil `.en` | 78–336 MB | English only | Legacy English-only options |

Models are downloaded once from Hugging Face (SHA-256 verified) into
`~/Library/Application Support/FlowingThoughts/models/`.

## Development

Requirements: Node.js + npm, Rust toolchain, Tauri CLI dependencies for macOS.

```bash
npm install
npm run tauri dev
```

Checks:

```bash
npm run build          # typecheck + bundle frontend
npm test               # frontend tests (vitest)
cd src-tauri && cargo check && cargo test
```

Hotkey override for development:

```bash
OVW_HOTKEY=fn npm run tauri dev
```

## Project structure

- `src-tauri/src/lib.rs` — backend bootstrap + session orchestrator
- `src-tauri/src/macos_hotkey.rs` — native CGEventTap global hotkey listener
- `src-tauri/src/audio.rs` — microphone capture
- `src-tauri/src/local_transcribe.rs` — on-device Whisper (whisper.cpp, Metal)
- `src-tauri/src/transcribe.rs` — cloud transcription (Groq/OpenAI)
- `src-tauri/src/corrections.rs` — learn-from-edits correction engine
- `src-tauri/src/text_inject.rs` — text injection into the focused app
- `src/pages/` — React UI (Home timeline, Settings, Onboarding, learned words)

## License

[MIT](./LICENSE)
