# Dutch evaluation set and eval harness

Improve local transcription **by measurement, not guesswork**. This is the
measurement layer only: nothing here changes how dictation behaves. Fixes (the
five-word floor, a better resampler, a different default model) come after a
baseline exists, and each one is judged by re-running this harness.

Everything runs offline. No audio or text leaves the machine.

## TL;DR — commands

```bash
# 1. Record: reads you the next prompt, Enter to start/stop a take (~45 min for all 312)
npm run eval -- record

# 2. Condition passes on a spread-out sample of ~40 prompts each
npm run eval -- record --condition quiet     --sample 40
npm run eval -- record --condition far       --sample 40
npm run eval -- record --condition noisy     --sample 40 --noise "café / fan / street"
npm run eval -- record --condition other_mic --sample 40      # switch mic in macOS first

# 3. What do I have?
npm run eval -- stats

# 4. Baseline (dev split). Axes are comma lists; every combination is run.
npm run eval -- run --model small,turbo --language nl,auto --vocab none,developer

# 5. Single suspects
npm run eval -- run --resampler app,afconvert          # linear resampler vs anti-aliased
npm run eval -- run --vad on,off                       # what the VAD gate costs / buys
npm run eval -- run --category short_reply             # the five-word floor

# 6. Held-out test split — only to confirm a decision already made on dev
npm run eval -- run --held-out --model turbo
```

`npm run eval -- help` lists every option. The first run compiles a release
build (a minute or two). Your terminal app needs microphone access (System
Settings → Privacy & Security → Microphone) for `record`.

## Where the data lives

Audio is personal voice data: **it never goes into git.**

| What | Where |
| --- | --- |
| Dataset (default) | `~/Library/Application Support/FlowingThoughts/eval/` |
| Override | `--data-dir <dir>` or `FT_EVAL_DIR` (`eval-data/` in the repo is gitignored for this) |
| Prompt script (committed) | `eval/prompts-nl.txt` |

The app data dir is the default because it survives worktrees, `git clean` and
branch switches, and the opt-in "keep my dictations" path writes to the same
place. Only non-personal text is committed (the prompt script). There are no
committed audio fixtures: the synthetic fixture is generated on demand.

```
eval/
  manifest.jsonl     one JSON object per clip, append-only in normal use, hand-editable
  audio/<id>.wav     the capture exactly as the app's audio path wrote it (native rate, 16-bit)
  results/           <timestamp>-<split>.json + .md per run
  test-runs.log      one line per scoring of the held-out split
  suggested-prompts.txt   output of `mine` (personal, review before use)
```

## Manifest schema

One line of `manifest.jsonl` (`src-tauri/src/eval/manifest.rs` is the source of truth):

| Field | Meaning |
| --- | --- |
| `id` | `<prompt_id>-<condition>-<8 hex>`, or `kept-<date>-<8 hex>` |
| `audio` | WAV path relative to the dataset dir |
| `reference` | Manually verified transcript of what was said. Empty for non-speech |
| `verified` | Only `true` clips are scored. Prompt takes are verified when you keep them; kept dictations start `false` |
| `expected` | `speech` \| `non_speech` |
| `split` | `dev` \| `test`, written once at creation and never recomputed |
| `categories` | Tags: `short_reply`, `everyday`, `names_places`, `compounds`, `numbers`, `mixed_tech`, `long_form`, `non_speech`, `real_world` |
| `language` | Language of the speech: `nl`, `en`, `nl-en` |
| `language_mode` | App language mode at capture time (kept dictations only) |
| `source` | `prompt_script` \| `kept_dictation` \| `synthetic` |
| `prompt_id` | Prompt this take reads, if any |
| `speaker`, `device` | Speaker id; microphone name as reported by CoreAudio |
| `sample_rate`, `channels`, `duration_ms`, `peak_amplitude` | Capture metadata, measured from the file |
| `condition`, `noise` | `normal` \| `quiet` \| `far` \| `noisy` \| `other_mic` \| `clipping` \| `real_world`; free-text noise note |
| `entities` | `{names, numbers, terms}` — lists scored separately from WER. `/` separates accepted alternates (`"12,50/12 euro 50"`) |
| `raw_transcript`, `raw_model` | What the app produced for a kept dictation. Stored apart from `reference`; never ground truth |
| `recorded_at`, `notes` | |

Entities are lists rather than character spans: they survive editing a
reference by hand, and "did this name come out right" does not need offsets.

## Dev / held-out test split

- **Deterministic:** `sha256(salt + key) mod 100 < 20` → `test`, else `dev`.
- **Stable when clips are added:** every key is hashed on its own, nothing is
  ranked, and the result is frozen in the manifest at creation.
- **Leak-free:** the key is the *prompt id*, so the normal, quiet, noisy and
  other-mic takes of one sentence all share a split. Kept dictations hash
  their clip id.
- **Stratified by category:** independent hashing gives ~20% per category in
  expectation; the salt was picked once (before any clip existed) so the
  committed script lands at 15–26% test in *every* category, and a unit test
  pins that. Prompts added later fall ~20/80. `stats` shows the actual balance.

Protection from casual tuning:

1. `run` scores `dev` only. `--held-out` is the one explicit way to score `test`.
2. A held-out run prints and stores **aggregates only** — no transcripts, no
   diffs, no worst-clip list — so there is nothing to tune against.
3. Every held-out run appends to `test-runs.log` (time, commit, configs). If
   that log grows fast, the test set is being used as a dev set.
4. The recorder never shows which split a prompt belongs to.

Rule of thumb: iterate on dev; touch test once per decision ("ship turbo as
default?", "remove the word floor?").

## What the harness measures

Each clip goes through the code dictation uses — not a copy of it:
`pipeline::is_capture_discarded` → `local_transcribe::transcribe_wav_blocking`
(resample → Silero VAD gate → Whisper → no-speech threshold → out-of-set
language retry) → `pipeline::filter_transcript` (marker/credit sanitizer →
prompt-echo filter → five-word floor) → `pipeline::finalize_transcript`
(developer dictionary, learned corrections). `lib.rs` calls the same functions.

Results are reported at two stages:

- **raw** — what the model said. Wrong here = model/decoding problem.
- **final** — what would have been typed. Right in raw but wrong in final = a
  filter threw away correct speech. A discarded speech clip counts as all
  deletions in final.

| Metric | Definition |
| --- | --- |
| WER | Corpus-level: Σ word errors / Σ reference words, over speech clips, after normalization |
| WER (lenient) | Same, but words written together or apart cost nothing. The gap to WER = compound/spacing errors |
| CER | Character Levenshtein over the normalized strings |
| names / numbers / terms | Share of tagged entities found as a whole-word sequence in the output |
| speech discarded | Speech clips that ended with no text, broken down by stage: `capture_gate`, `vad`, `model_empty`, `filtered`, `prompt_echo`, `under_word_floor` |
| non-speech typed / raw | Non-speech clips where text survived all filters / where the model produced text at all |
| latency, RTF | Per-clip transcription wall time (model load excluded, reported separately); p50/p95; RTF = Σ latency / Σ audio |
| peak MB | Peak physical footprint of the process (includes Metal buffers). Monotonic within one invocation — run one config for a clean per-model number |

Comparison axes: `--model` (base / small / turbo / any id or custom ggml stem),
`--language` (`nl` vs `auto`; auto mode also reports the detected languages and
how often the Dutch re-decode fired), `--vocab` (`none` / `personal` /
`developer` / `both`), `--vad`, `--resampler`. `personal` and `both` read the
live corrections DB **read-only**; results store only term counts and a prompt
hash, not the terms. `both` is what the app does by default; `developer` is
the default here because it is reproducible.

The linear-interpolation resampler has no in-app alternative yet, so
`--resampler afconvert` pre-converts each clip to 16 kHz with macOS
`afconvert` (anti-aliased), which bypasses the app resampler. That gives a
measured answer to "is the resampler costing accuracy?" before anyone writes
a new one. Clips are stored at native rate for exactly this reason.

Not applied: the 1-second hold-to-commit rule. It is a hotkey rule, not an
audio rule — but it does mean a real dictation is never shorter than ~1 s.

## Normalization rules (Dutch)

Conservative by design: a rule exists only when two written forms are the
**same spoken words**. Anything you would have to fix by hand after dictating
stays an error. Implemented in `src-tauri/src/eval/normalize.rs`.

| Rule | Example | Why |
| --- | --- | --- |
| Case-insensitive | `Utrecht` = `utrecht` | Casing is formatting, scored elsewhere if ever |
| Punctuation dropped; curly quotes → `'` | `Ja, dat klopt!` = `ja dat klopt` | |
| Clitics = full forms | `'t`=`het`, `'n`=`een`, `'k`=`ik`, `m'n`=`mijn`, `z'n`=`zijn` | Same word; Whisper writes either |
| In-word apostrophes kept | `auto's` ≠ `autos`, `zo'n` stays | That is spelling |
| Number words = digits, single token, < 1 000 000 | `vijftien`=`15`, `drieëntwintig`=`23`, `twaalfhonderd`=`1200` | |
| …but never bare `een` | `een` ≠ `1`; `één` = `1` | The article would wreck everything |
| Spoken thousands join | `tweeduizend zesentwintig` = `2026` | |
| Separators | `1.250`=`1250`, `12.50`=`12,50`; `15:30` kept | |
| Currency / percent | `€25`=`25 euro`, `15%`=`15 procent`, `&`=`en` | |
| Hyphen and slash → space | `e-mail` → `e mail`, `API-key` → `api key` | Strict WER still counts `email` as wrong; lenient forgives it |
| **Not** normalized | compounds (`zorg verzekering`), diacritics (`coordinatie`), loanword spelling (`gedeployed`), clock-time paraphrases (`half vier` vs `15:30`), ordinals (`3e` vs `derde`) | Visible errors, or not the same words. Use entity alternates (`3e/derde`) where two forms are both acceptable |

References are written the way you want dictation to come out. For English
loanwords that means the form you would type (`pull request`, `gedeployd`).

## Collection workflow

### Prompt script — `eval/prompts-nl.txt`

312 prompts: 40 short replies (all under five words — the word-floor suspect),
60 everyday sentences, 40 names/places, 30 compounds, 50
numbers/dates/times/EUR amounts, 60 Dutch+English tech, 20 long-form
dictations, 12 non-speech instructions (silence, breathing, cough, typing,
mouse, door, music, desk taps, paper, fan/street). The prompt *is* the
reference, so verification costs nothing unless you misread — then press `e`
and type what you said.

Format: `<id> | <text> | names: A; B | numbers: 1; 2 | terms: X; Y`. Append new
prompts at the end of a category; never renumber (the split hangs off the id).

### Recorder — `npm run eval -- record`

Shows the next prompt, records through `audio::start_recording` /
`stop_and_finalize` — the same cpal path, default device, native rate and
16-bit conversion as real dictation — then: Enter keep · `r` re-record · `p`
play back · `e` edit reference · `d` discard · `s` skip · `q` quit. It resumes
where you stopped (per speaker + condition), warns when a take is clipped or
would be dropped by the app's silence gate, and writes the WAV + manifest line.

### Plan to 450+ clips in about two hours

| Session | Command | Clips | Time |
| --- | --- | --- | --- |
| 1. Everything once, usual mic | `record` | 312 | ~45 min |
| 2. Quiet speech | `record --condition quiet --sample 40` | ~42 | ~7 min |
| 3. Far from mic (arm's length+) | `record --condition far --sample 40` | ~42 | ~7 min |
| 4. Noisy room | `record --condition noisy --sample 40 --noise "…"` | ~42 | ~7 min |
| 5. Other mic (AirPods ↔ MacBook) | `record --condition other_mic --sample 40` | ~42 | ~7 min |
| 6. Real-world | keep-dictations toggle for a few days, then `verify` | 50+ | ~10 min verifying |

`--sample 40` takes every 8th prompt of each category, so the same sentences
recur across conditions and condition effects are comparable. Clipping: a few
takes with `--condition clipping` spoken loudly into the mic. Other speakers
(optional until the ~20-user target matters): `--speaker <name>`.

### Opt-in: keep real dictations

Settings → Extras → **Keep my dictations for evaluation**. Off by default.
When on, each dictation's WAV and raw transcript are copied into the dataset
as an **unverified** `real_world` clip just before the app deletes the
capture. Unverified clips are never scored. `npm run eval -- verify` plays
each one and asks: Enter = the app heard it exactly right · `e` = type what
was said · `n` = no speech · `x` = delete. Turn the toggle off when you have
enough. Captures dropped by the silence gate are not kept.

### Mining corrections — `npm run eval -- mine`

Reads the correction history read-only and writes `suggested-prompts.txt`
(in the dataset dir, outside git): the most frequent wrong→intended pairs, each
as a prompt line built from the sentence it occurred in. **Suggestions only** —
corrections are not paired speech and not ground truth. Review the file, then
`record --prompts <file>`.

## Baseline

**No real baseline yet — it needs the owner's voice.** This environment has no
microphone access and no clips. What exists is an end-to-end proof on a
synthetic fixture: 14 sentences from the prompt script spoken by the macOS
Dutch TTS voice (22.05 kHz) plus 3 generated non-speech clips (silence,
low-level noise, clicks). TTS is not a person and 17 clips are an anecdote;
**do not make decisions from these numbers.**

```bash
npm run eval -- synth --data-dir eval-data/synth
npm run eval -- run   --data-dir eval-data/synth --language nl,auto --vocab none,developer
```

2026-09-17, commit `ef3e413`, Apple M1 Max, `whisper-large-v3-turbo-q5` (the only
multilingual model installed here):

| config | WER raw | WER final | CER final | names | numbers | terms | speech discarded | non-speech typed | p50 ms | RTF | peak MB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| nl · vocab=none | 6.4% | 9.3% | 4.5% | 4/5 | 8/8 | 12/18 | 14.3% | 0/3 | 687 | 0.16 | 746 |
| nl · vocab=developer | 5.9% | 8.8% | 4.2% | 4/5 | 8/8 | 14/18 | 14.3% | 0/3 | 714 | 0.17 | 798 |
| auto · vocab=none | 6.4% | 9.3% | 4.5% | 4/5 | 8/8 | 12/18 | 14.3% | 0/3 | 1329 | 0.28 | 802 |
| auto · vocab=developer | 5.9% | 8.8% | 4.2% | 4/5 | 8/8 | 14/18 | 14.3% | 0/3 | 1318 | 0.28 | 803 |
| nl · developer · resample=afconvert | 4.4% | 7.4% | 3.7% | 5/5 | 8/8 | 14/18 | 14.3% | 0/3 | 719 | 0.16 | 796 |

What the fixture already shows about the harness (not about quality):

- The raw→final gap is entirely the **five-word floor**: both short replies
  ("Ja, dat klopt.", "Nee, dank je.") were transcribed perfectly and then
  discarded (`under_word_floor 2`).
- `auto` costs roughly double the latency of explicit `nl` on short clips.
- The resampler axis moves numbers; whether that holds on a real 48 kHz mic is
  exactly what the real baseline must answer.

### Producing the real baseline

1. Download Whisper Small and Base in the app (Turbo is installed) so all three can be compared.
2. Record sessions 1–5 above.
3. `npm run eval -- run --model base,small,turbo --language nl,auto --vocab none,developer`
4. `npm run eval -- run --model small,turbo --resampler app,afconvert` and `--vad on,off`
5. For memory: one model per invocation, e.g. `npm run eval -- run --model small`.
6. Paste the summary tables from `results/*.md` into this section.

## Code map

| File | Role |
| --- | --- |
| `src-tauri/src/pipeline.rs` | Capture gate, vocabulary prompt, text filters, finalisation — shared by `lib.rs` and the harness |
| `src-tauri/src/local_transcribe.rs` | `transcribe_wav_blocking` (+ VAD-rejection and language diagnostics) |
| `src-tauri/src/eval/normalize.rs`, `score.rs` | Normalization, WER/CER alignment, entity accuracy, diffs |
| `src-tauri/src/eval/manifest.rs`, `prompts.rs` | Dataset schema + IO, split assignment, prompt-script parser |
| `src-tauri/src/eval/harness.rs`, `report.rs` | Run configs, stage attribution, tallies; JSON + Markdown output |
| `src-tauri/src/eval/record.rs`, `keep.rs`, `mine.rs`, `synth.rs` | Recorder + verify, opt-in keep, correction mining, synthetic fixture |
| `src-tauri/src/eval/cli.rs`, `src-tauri/examples/ft_eval.rs` | CLI. An example, not a second binary, so app builds and bundles are untouched |

Scoring, normalization, entities, split assignment, manifest IO and harness
stage attribution (with a canned transcriber) are unit-tested in plain
`cargo test`, no model required.
