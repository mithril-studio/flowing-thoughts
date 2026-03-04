# Open Voice Wispr — Build Plan

**What:** macOS voice dictation app. Global hotkey → record → transcribe → type into active app.
**Stack:** Tauri 2 + React + TypeScript
**STT:** OpenAI Whisper API (user provides their own key)
**Price:** €20 one-time via LemonSqueezy. No subscription. No free tier.
**Distribution:** Direct download (.dmg), no Mac App Store, no notarization (Phase 1).

---

## Architecture Overview

```
┌─────────────────────────────────────────────┐
│                  Menu Bar UI                 │
│              (React + Tailwind)              │
│  ┌─────────┐  ┌──────────┐  ┌───────────┐  │
│  │ Status  │  │ Settings │  │  History   │  │
│  │ Widget  │  │  Panel   │  │   Panel    │  │
│  └─────────┘  └──────────┘  └───────────┘  │
└──────────────────┬──────────────────────────┘
                   │ Tauri IPC
┌──────────────────▼──────────────────────────┐
│              Rust Backend                    │
│  ┌──────────────┐  ┌─────────────────────┐  │
│  │ Audio Capture│  │  Global Hotkey       │  │
│  │ (cpal/corea) │  │  (tauri-plugin)      │  │
│  └──────┬───────┘  └─────────────────────┘  │
│         │                                    │
│  ┌──────▼───────┐  ┌─────────────────────┐  │
│  │ Whisper API  │  │  Text Injection      │  │
│  │ (reqwest)    │  │  (CGEvent/AX API)    │  │
│  └──────────────┘  └─────────────────────┘  │
│                                              │
│  ┌──────────────┐  ┌─────────────────────┐  │
│  │ License      │  │  Local Storage       │  │
│  │ (LemonSqueezy│  │  (tauri-store)       │  │
│  └──────────────┘  └─────────────────────┘  │
└──────────────────────────────────────────────┘
```

---

## Phase 1: MVP Backlog

### 1. Core Engine — Voice-to-Text Pipeline

| # | Task | Details | Priority |
|---|------|---------|----------|
| 1.1 | **Global hotkey listener** | Register system-wide shortcut (default: `Cmd+Shift+Space`). **Hold-to-record only** — press to start, release to send. Uses `tauri-plugin-global-shortcut`. | P0 |
| 1.2 | **Audio capture** | Record from default mic. Use `cpal` crate or CoreAudio bindings. Output WAV/PCM buffer. | P0 |
| 1.3 | **Whisper API integration** | POST audio to `api.openai.com/v1/audio/transcriptions`. Handle response, errors, rate limits. User provides their own API key. | P0 |
| 1.4 | **Text injection (highest-risk item)** | Type transcribed text into the currently focused app via `CGEventCreateKeyboardEvent`. Requires macOS Accessibility permission. **This is the hardest part of the build** — must be tested extensively across apps (VS Code, Chrome, Slack, Notes, Terminal, Obsidian). | P0 |
| 1.5 | **Audio → text pipeline** | Wire it all together: hotkey → record → release → send to Whisper → inject text. Visual feedback at each stage. | P0 |

**Key technical decisions:**
- **Hold-to-record only** — no toggle mode. Press hotkey, speak, release to send. Simple, no "forgot to stop" edge cases. Toggle mode is Phase 2 if users request it.
- **CGEvent over AXUIElement** — `CGEventCreateKeyboardEvent` is more reliable across apps than Accessibility text insertion. It simulates real keystrokes.
- **Accessibility permission** is required — app must prompt on first run and detect permission state on every launch.
- Audio format: send as `whisper-1` compatible (mp3/wav, <25MB).
- **Text injection test matrix** (must pass before release):
  - Safari, Chrome, Arc (browser text fields)
  - VS Code, Cursor (code editors)
  - Slack, Discord (Electron chat apps)
  - Notes, TextEdit (native macOS)
  - Terminal, iTerm2 (terminal emulators)
  - Notion, Obsidian (note-taking apps)

### 2. User Interface — Menu Bar App

| # | Task | Details | Priority |
|---|------|---------|----------|
| 2.1 | **Menu bar / tray app** | App lives in macOS menu bar, not dock. Small floating indicator during recording. Uses `tauri-plugin-positioner` for window placement. | P0 |
| 2.2 | **Recording indicator** | Visual feedback: floating pill/badge near cursor or menu bar showing "Recording..." with waveform or pulsing dot. | P0 |
| 2.3 | **Settings panel** | OpenAI API key input, hotkey configuration, language selection, model selection (whisper-1). | P0 |
| 2.4 | **Onboarding / first-run (conversion-critical)** | This is the biggest drop-off point. Must be frictionless. See onboarding flow below. | P0 |
| 2.5 | **Transcription history** | Simple list of recent transcriptions with timestamps. Copy-to-clipboard on click. Local storage only. | P1 |

**UI stack:** React 18 + Tailwind CSS. Keep it minimal — this is a utility, not a workspace.

**Onboarding flow (step by step):**

The user has already bypassed Gatekeeper before they see the app (instructions on website/README). Onboarding starts clean:

```
Step 1: "Enter your license key"
        [input field] [Activate]
        "Don't have one? Buy for €20 →" (link to LemonSqueezy)

Step 2: "Enter your OpenAI API key"
        [input field] [Save]
        "How to get an API key →" (link to platform.openai.com)

Step 3: "Grant Accessibility Permission"
        [Open System Settings]  ← button triggers the macOS prompt
        "Open Voice Wispr needs this to type text into your apps."
        [auto-detect when granted, advance to next step]

Step 4: "You're ready. Hold Cmd+Shift+Space and speak."
        [Try it now]  ← interactive test that transcribes and shows result
```

Each step validates before advancing. No skipping. No "do this later."

### 3. Licensing — LemonSqueezy (Keep It Simple)

Don't over-engineer this. It's a €20 one-time purchase. The goal is "validate once, run forever" — not a license server.

| # | Task | Details | Priority |
|---|------|---------|----------|
| 3.1 | **License key validation** | On first launch, validate key against LemonSqueezy API (`/v1/licenses/validate`). One API call. Store result locally. Done. | P0 |
| 3.2 | **Activation persistence** | Store license status + activation date in `tauri-plugin-store`. **Never phone home again after successful activation.** The app is paid, not rented. | P0 |

That's it for Phase 1. No offline grace period logic, no periodic re-validation, no device limit handling. If someone shares their key — it's €20, not worth the engineering cost to prevent.

**LemonSqueezy setup:**
- Product: Open Voice Wispr
- Price: €20 one-time
- License key: Auto-generated on purchase

### 4. Distribution & Updates

| # | Task | Details | Priority |
|---|------|---------|----------|
| 4.1 | **Build pipeline** | Tauri build → `.dmg` for macOS. GitHub Actions or local build script. Target `aarch64-apple-darwin` (Apple Silicon) + `x86_64-apple-darwin` (Intel). Universal binary preferred. | P0 |
| 4.2 | **Gatekeeper bypass — website/README only** | "Right-click → Open" instructions with screenshots/GIF on the download page. This happens *before* the app opens, so it belongs on the website, not in-app. Make it dead simple — 3 steps with visuals. | P0 |
| 4.3 | **Update checker** | On launch, check a simple JSON endpoint (or GitHub releases API) for new versions. Show "Update available" banner with download link. No auto-update — just a link. Sparkle requires code signing, skip it entirely. | P1 |
| 4.4 | **Landing page** | Simple page: what it does, demo GIF, €20 buy button (LemonSqueezy checkout), download link. | P1 |

### 5. Technical Debt / Phase 2

| # | Task | Details | Priority |
|---|------|---------|----------|
| 5.1 | Apple Developer Certificate + Notarization | Removes Gatekeeper friction. Enables Sparkle auto-updates. | Phase 2 |
| 5.2 | Local Whisper model (whisper.cpp) | Offline mode. No API key needed. Privacy-first option. | Phase 2 |
| 5.3 | Crash reporting (Sentry) | Catch issues in the wild. | Phase 2 |
| 5.4 | Multi-language prompt templates | "Translate to English", "Fix grammar", custom prompts. | Phase 2 |
| 5.5 | Clipboard mode | Paste transcription to clipboard instead of typing. | Phase 2 |

---

## Build Order (Suggested Sequence)

```
Week 1:  Project scaffold (Tauri 2 + React + Tailwind)
         Global hotkey + audio capture working in isolation
         Whisper API integration (standalone test: record → transcribe → log)

Week 2:  Text injection via CGEvent (THE hard part)
         Get it working in Notes, Chrome, VS Code, Slack, Terminal
         Wire the full pipeline: hold hotkey → record → release → transcribe → type
         Debug edge cases (special characters, newlines, Unicode, fast typing)

Week 3:  Menu bar UI + recording indicator
         Settings panel (API key, hotkey config)
         Onboarding flow (license → API key → accessibility → test)

Week 4:  LemonSqueezy license validation (simple: validate once, store, done)
         Build pipeline (.dmg, universal binary)
         Landing page + Gatekeeper instructions
         First release
```

---

## Key Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| **Text injection fails in some apps** | Core feature broken — app is useless | CGEvent keystroke simulation is the most reliable approach. Dedicated Week 2 for testing across app matrix. Ship with known-working list. Document any unsupported apps. |
| **Gatekeeper scares users away** | Users download but never open the app | Gatekeeper bypass instructions on download page with GIF walkthrough. Keep it to 3 steps. Phase 2 notarization removes this entirely. |
| **Accessibility permission not granted** | App can't type — silent failure | Onboarding blocks at this step. Detect permission state on every launch. Show persistent warning if revoked. |
| **Whisper API latency** | >3s delay feels broken | Show "Transcribing..." indicator with animation. Typical: 1-3s for short clips. |
| **€20 for unsigned app** | Trust barrier | Landing page must sell confidence: demo video, clear value prop ("no subscription, your API key, your data"), and Gatekeeper instructions upfront so users know what to expect. |

---

## Dependencies & Accounts Needed

- [ ] LemonSqueezy seller account (create product, set price, get API keys)
- [ ] OpenAI account (for testing — users bring their own)
- [ ] Domain for landing page
- [ ] GitHub repo (for releases / update checks)
- [ ] Rust toolchain + Tauri CLI installed locally
