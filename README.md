# FlowingThoughts

Free, open source voice dictation for macOS. Hold a key, speak, release. Your words are typed into whatever app you're using.

- **Local first.** Speech recognition runs on your Mac with Whisper. No account, no subscription, no audio leaves your machine.
- **English and Dutch** out of the box, with auto detection. The recommended model is 190 MB and runs in under 0.5 GB RAM.
- **Learns from you.** Fix a word once and the correction applies to every future dictation.
- **Works everywhere.** Hold `Fn` (or `⌘⇧Space`) in any app: Slack, Mail, your editor, a browser.

## Install

1. Download the latest `.dmg` from [Releases](https://github.com/mithril-studio/flowing-thoughts-releases/releases/latest) and drag FlowingThoughts into Applications.
2. The app is not notarized yet, so right-click it and choose **Open** to get past Gatekeeper.
3. Follow the setup: download the speech model (one time), grant **Accessibility** and **Input Monitoring**, and run the injection test.
4. Hold `Fn`, speak, release.

Prefer cloud transcription? Bring your own Groq or OpenAI API key in Settings. Audio is only uploaded when you select the API provider there; local mode never falls back to the cloud, even with a key saved.

## How it works

Hold the hotkey and the app records from the microphone. On release it transcribes locally with Whisper (whisper.cpp with Metal), applies your learned corrections, and pastes the text into the focused app, restoring your clipboard afterwards.

See [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) for details and [docs/COMPATIBILITY.md](./docs/COMPATIBILITY.md) for the cross-app verification matrix.

## Development

Requirements: Node.js, Rust, and the Tauri CLI dependencies for macOS.

```bash
npm install
npm run tauri dev
```

Checks: `npm run build` and `npm test` for the frontend, `cargo check && cargo test` in `src-tauri/`.

## License

[MIT](./LICENSE)

The optional Parakeet model is [NVIDIA parakeet-tdt-0.6b-v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) (CC-BY-4.0), downloaded as the [ONNX export by istupakov](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx).
