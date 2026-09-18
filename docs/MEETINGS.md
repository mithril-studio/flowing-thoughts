# Meetings

Record a call, get a transcript with "Me" and "Them" labels, all on your Mac.
Meetings are off by default: turn them on in Settings → Meetings, which adds a
Meetings tab.

## What it does

- Records two tracks on one timeline: your microphone ("Me") and the Mac's
  system audio ("Them"). It works with any call app (Zoom, Teams, Meet in a
  browser) because it records what the Mac plays, not the app.
- Start, pause and stop from the Meetings tab. The menu bar shows the
  recording dot and the duration for the whole meeting. Dictation keeps
  working during a meeting.
- After you stop, the meeting is transcribed locally in the background. The
  transcript fills in while it runs. Dictation always goes first: a dictation
  interrupts meeting decoding and the meeting picks up again afterwards.
- Language is `auto`, `nl` or `en` per meeting. Auto detects once per track.
  "Re-transcribe as…" makes a new run with another model or language; the old
  transcript stays until the new one is complete.
- Segments that look wrong (no speech, outside detected speech, repeats, prompt
  echo, speaker bleed) are hidden behind a "show N hidden" toggle, never
  deleted. You can edit segment text; the original is kept.
- Optional summary (overview, decisions, actions, topics) with links back to
  the transcript lines each item came from. See the privacy model below.
- Export a meeting as Markdown.

## Privacy model

- **Local by default.** Recording and transcription happen on this Mac. No
  account, no upload. Meeting transcription never uses the cloud
  transcription provider, even when an API key is saved for dictation.
- **Audio is kept until you delete it.** Delete a meeting (transcript and
  audio) or only its audio (the transcript stays). Settings has an optional
  "delete audio N days after transcription"; it is off by default.
- **Where files live:**
  - Audio: `~/Library/Application Support/FlowingThoughts/meetings/<meeting_id>/<mic|system>/<seq>.pcm`
    (raw 16 kHz mono, 16-bit, 60 s per file, about 230 MB per hour for both
    tracks) plus a `track.json` per track.
  - Transcripts, edits, summaries and jobs: the app's SQLite database,
    `~/Library/Application Support/FlowingThoughts/flowing_thoughts.db`.
  - Nothing is encrypted beyond what FileVault gives the disk.
- **The one cloud call: summaries.** Off by default. It needs three things:
  summaries enabled in Settings, your own OpenRouter key, and your
  confirmation for that meeting ("Send the transcript to OpenRouter?"). It
  then sends the meeting's full transcript text, never audio, to OpenRouter
  with the model you chose. Nothing else in Meetings touches the network.
- **The other side.** The app records everyone on the call. Telling them, and
  getting consent where the law requires it, is up to you.

## Requirements

- **macOS 14.4 or later** for meetings. That is the first version with Core
  Audio process taps, which is how system audio is recorded. On older versions
  the Meetings tab explains this and dictation keeps working (the app itself
  runs on macOS 13.4+).
- A downloaded **Whisper** model. Parakeet cannot be used for meetings: it
  has no timestamps.

## Permissions

| Permission | Why | Without it |
|---|---|---|
| Microphone | your side | no recording |
| System Audio Recording | the other side | the meeting records your microphone only |

macOS asks for System Audio Recording the first time a meeting starts. It is
listed under System Settings → Privacy & Security → Screen & System Audio
Recording → "System Audio Recording Only".

**What denial looks like.** macOS does not report a denial as an error: the
system track simply delivers silence. So a denial never blocks a meeting. The
app shows one of:

- "System audio permission denied, recording microphone only." when macOS says
  the permission is denied.
- "No system audio detected. If the call is not silent, allow System Audio
  Recording for FlowingThoughts." after a long stretch of pure silence on the
  system track. This one can be a false alarm when nobody has spoken yet.

Both come with a button that opens the right System Settings pane. Grant the
permission there, then start a new meeting.

**Ad-hoc builds may require permissions again after an update.** The first
transition to Developer ID signing may also require new grants for Microphone,
Accessibility, Input Monitoring and System Audio Recording. Subsequent releases
with the same Developer ID are intended to retain them; verify this using the
[release smoke test](RELEASING.md). If a permission looks granted but does not
work, remove FlowingThoughts from that list in System Settings and add it again.

## Use headphones

On the built-in speakers the other side leaks into your microphone and gets
transcribed twice: once as "Them", once, badly, as "Me". The app shows "Use
headphones for best results" when it sees the output is the speakers, and
after transcription it hides "Me" segments that repeat what "Them" said at the
same moment. That pass works on text and is careful not to hide things you
really said, so some bleed gets through. There is no echo cancellation yet.

With AirPods as both microphone and output, macOS switches them to call mode
and system audio is recorded at 24 kHz. It is still fine for transcription.

## Known limits

- No per-person labels yet: everyone on the other side is "Them". The
  database is already built for speakers and attendees; diarization and
  attendee linking come after v1.
- Whisper models only, and no cloud transcription for meetings.
- Transcription starts after you stop. There is no live transcript while
  recording.
- One meeting at a time.
- English and Dutch. Auto picks one of the two per track; a meeting that
  switches language mid-way is decoded in the detected one.
- Paused stretches and device switches (for example AirPods disconnecting)
  leave gaps in the audio. Gaps are kept on the timeline, so timestamps stay
  right.
- Launch on macOS 13 is checked at build time (the binary does not hard-link
  the process tap symbols) but not tested on real hardware every release.

## Manual test checklist for a release

Run against the built `.app` opened from Finder or with `open`, never
`tauri dev`: under `tauri dev` the terminal owns the permissions.

Signing and bundle (`scripts/release.sh` asserts these and fails the release
otherwise; run by hand when checking a local build):

- [ ] `codesign --verify --deep --strict FlowingThoughts.app` passes.
- [ ] `codesign -dvvv FlowingThoughts.app` shows
      `Identifier=sh.thoughts.flowing` and `Info.plist entries=…`, not
      `Info.plist=not bound`.
- [ ] `plutil -extract NSAudioCaptureUsageDescription raw FlowingThoughts.app/Contents/Info.plist`
      and the same for `NSMicrophoneUsageDescription` print the texts.
- [ ] `nm -um FlowingThoughts.app/Contents/MacOS/flowing-thoughts | grep ProcessTap`
      prints nothing.
- [ ] The `.app` inside the updater `.app.tar.gz` has the same `CDHash` as the
      one in the DMG.

Permissions:

- [ ] Fresh install: the System Audio Recording prompt appears at the first
      meeting start. Allow: "Them" has audio. Relaunch: still granted.
- [ ] Deny it (or switch it off in System Settings): the meeting still
      records, mic only, and the denied notice with its button shows.
- [ ] Update from the previous release through the in-app updater: note which
      permissions macOS asks for again, and that dictation and meetings work
      once they are granted.

Recording:

- [ ] A real call of 60 minutes with headphones: both tracks transcribed,
      "Me" and "Them" line up, memory stays flat.
- [ ] One call each through Zoom, Teams and Meet in Chrome.
- [ ] Built-in speakers: the headphones notice shows; most duplicated "Me"
      lines end up hidden as echo.
- [ ] Pause and resume: the duration and the transcript timestamps stay right.
- [ ] Connect and disconnect AirPods mid-meeting; sleep and wake mid-meeting:
      no crash, recording continues, the gap is short.
- [ ] Tray → Quit mid-meeting, and `kill -9` mid-meeting: after relaunch the
      meeting shows as interrupted, is recovered up to the last second and
      gets transcribed.

Transcription:

- [ ] Dictate while a meeting is being transcribed: dictation latency is
      normal and the meeting finishes afterwards.
- [ ] Quit while transcribing, relaunch: the job resumes and no part is
      transcribed twice.
- [ ] Dutch, English and auto on a meeting in each language; "Re-transcribe
      as…" keeps the old transcript until the new one is done.
- [ ] Parakeet selected for dictation: meetings still ask for a Whisper model.

Summary and data:

- [ ] With summaries off, or without a key, no request leaves the Mac (check
      with Little Snitch or `nettop`).
- [ ] With summaries on: the confirmation names OpenRouter and the model;
      items link to transcript lines.
- [ ] Delete audio only: files gone, transcript stays. Delete meeting: rows
      and the meeting's directory gone.
- [ ] Markdown export opens cleanly.

Compatibility:

- [ ] Below macOS 14.4, if a tester is available: the app launches, dictation
      works, the Meetings tab explains the requirement.
