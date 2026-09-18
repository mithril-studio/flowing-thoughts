# Releasing FlowingThoughts

## Current status (2026-09-18)

The public release is **0.5.0** (Apple Silicon). The **0.5.1** tag failed Apple
certificate import and was never published. Developer ID signing/notarization
and an actual installed-app upgrade are not yet verified end-to-end.
Do not describe the failed tag as a shipped notarized release.

## Repair Apple certificate credentials

From this checkout, run interactively (do not paste passwords into chat):

```sh
python3 scripts/set-apple-certificate.py ../FlowingThoughts-DeveloperID.p12
```

The helper prompts without echo, imports the certificate into a temporary
keychain, requires our valid Developer ID identity, and only then uploads the
certificate/password pair to GitHub Actions. It removes the temporary keychain
even on failure. Apple's import command necessarily receives the password as a
process argument; run on a trusted local machine, not under a process tracer.
If import fails, re-export the Developer ID **certificate and private key** from
Keychain Access as a password-protected `.p12`, then rerun the helper.
A failure during GitHub upload can update only one secret; rerun the helper to
restore the pair. Never run releases while credentials are being replaced.

After this workflow is merged to `main`:

```sh
gh workflow run apple-credentials.yml --repo mithril-studio/flowing-thoughts
```

Check the **Apple credentials** run before tagging. It validates certificate
import, signing identity and App Store Connect notarization authentication,
without building or publishing. API key values stay in GitHub secrets.

Apple preflight also runs automatically when credential/release workflow changes
land on `main`; this does not build or publish an app. Required CI checks only
changed Rust files for formatting while retaining full Rust compilation/tests.
Repository-wide formatting cleanup remains a separate change.

## Normal release

1. Use a PR to bump `package.json`, both root versions in `package-lock.json`,
   `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and the app's entry in
   `src-tauri/Cargo.lock`. Choose a **fresh patch version**, not the failed 0.5.1 tag.
2. Run `python3 scripts/check-release-version.py vX.Y.Z` and all CI checks.
3. Merge the PR after the required `checks` status passes. Main requires PRs,
   up-to-date checks and resolved conversations, including for administrators.
4. Tag the merged commit and push the annotated tag:
   `git tag -a vX.Y.Z -m 'Release X.Y.Z' && git push origin vX.Y.Z`.
5. The release runs credential preflight, CI, then an Apple Silicon build.
   Tauri signs, notarizes and staples the app before creating updater artifacts.
   The script verifies Gatekeeper acceptance, bundle identity/entitlements,
   updater app ticket and code hash, and the update signature against the app's
   public key. The DMG is separately notarized and stapled.
6. Assets upload to a **draft** in `mithril-studio/flowing-thoughts-releases`.
   The script downloads and byte-compares all four assets before publishing.
   The stable feed changes only after verification succeeds. Release jobs are
   serialized; a newer release does not cancel one already publishing.
7. Complete the installed-app smoke test below. A green build is not that test.

Local publishing uses `bash scripts/release.sh 'Release notes'` with the env
variables listed in the script and `minisign` installed. Developer ID signing
cannot be disabled in this publishing path. For local unsigned development
builds, use `npx tauri build` separately. Never rotate the Tauri updater key as a
signing-password workaround: installed apps trust its existing public key.

## Failure/recovery

- No uploaded draft: fix the failure and rerun the same immutable tag if its code
  is correct. If code/workflow changes are needed, merge them and use a new tag.
- Failed draft: inspect its logs and assets. Delete **only that unpublished
  draft** before rerunning. Scripts do not overwrite existing releases.
- Bad published release: stop publishing; ship a higher patch version with the
  fix. Do not move source tags or silently replace signed assets. The updater
  does not automatically downgrade already-installed apps.
- CI has a 15-minute limit; provenance and Apple preflight each have a 5-minute
  limit; the build/notarization/publish job has a 25-minute limit. These are
  per-job limits, not a 25-minute limit for the entire pipeline. New CI runs
  cancel obsolete checks, but releases never cancel an active publication.
  Inspect the Apple
  submission/log before retrying. Do not bypass signing to get a green release.

## Installed-app smoke test — human interaction required

Use a disposable macOS account or a backed-up installation. Do not run a debug
build with the same bundle ID during this test.

- [ ] Install the previous public release; record version, settings/history and
      permission grants. Back up app data before upgrading.
- [ ] Launch after the new release publishes: the update banner offers the new
      version. Settings' manual update check reports it too.
- [ ] Click **Install & restart**; confirm successful relaunch and new version.
- [ ] Confirm settings, history, models and existing meetings are preserved.
- [ ] Confirm dictation, text injection and meeting recording work. The first
      transition from ad-hoc to Developer ID may require granting permissions
      again; subsequent Developer ID updates should retain them.
- [ ] Verify a fresh DMG installation passes Gatekeeper without Open Anyway.
- [ ] Disconnect the network and confirm the updated app still launches and
      performs local dictation with an installed model.
- [ ] Record app versions, macOS version and results in the release/PR notes.

The automated suite intentionally skips hardware/model-dependent Rust tests;
see `docs/MEETINGS.md` and `docs/COMPATIBILITY.md` for those release checks.
