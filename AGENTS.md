# FlowingThoughts Execution Plan (25-30% -> 95%)

## Delivery Rules

- Work strictly step-by-step in the order below.
- After each task:
  - Run real tests/build checks (no mocked pipeline tests as acceptance).
  - Commit with a focused message.
- Do not start the next task until the current task is tested and committed.
- Permission prompts policy:
  - Only ask for elevated permissions for `push` to `main` requests.
  - Only ask for elevated permissions in plan mode.
  - Otherwise skip permission prompts and continue with default sandbox execution.

## Steps

1. Implement real audio capture pipeline in Rust.
   - Build hold-to-record buffering from default microphone.
   - Finalize WAV output on key release with per-session metadata.
   - Wire into session orchestrator.

2. Implement real Whisper transcription.
   - Replace placeholder transcription worker.
   - Send captured audio to OpenAI transcription endpoint.
   - Parse result and emit transcript events.

3. Implement real text injection.
   - Add macOS text injection into focused app.
   - Add fallback injection mode (clipboard paste).
   - Wire `Transcribing -> Injecting -> Idle`.

4. Replace raw `fn` listener with configurable production hotkey.
   - Default: `Cmd+Shift+Space`.
   - Add hold/release behavior and debounce.

5. Build onboarding gate flow.
   - License key activation.
   - OpenAI API key setup.
   - Accessibility and microphone permission checks.
   - End-to-end test action in onboarding.

6. Move persistence to Tauri store for core app state.
   - Settings, keys, history, runtime preferences.
   - Remove dependence on browser localStorage for core features.

7. Add robust pipeline reliability handling.
   - Retries/timeouts for transcription.
   - Stale-session protection.
   - Structured error propagation and user-facing states.

8. Improve UX to Wispr-like interaction quality.
   - Fast visual phase feedback.
   - Refined recording/transcribing/injecting indicators.
   - Practical error/recovery UX.

9. Cross-app compatibility and quality pass.
   - Validate text injection across major macOS targets.
   - Fix newline/special character behavior.
   - Document unsupported edge cases and fallbacks.

10. Release hardening.
    - Security/config tightening.
    - Crash-safe logging and diagnostics.
    - Distribution/update readiness checks.
