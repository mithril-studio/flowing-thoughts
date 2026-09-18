"""Release tooling tests: python3 -m unittest discover -s scripts -p 'test_*.py'."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent


class ApplePreflightTests(unittest.TestCase):
    def test_missing_credentials_fail_before_using_keychain(self):
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/check-apple-credentials.sh")],
            env={"PATH": os.environ["PATH"]}, capture_output=True, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("APPLE_CERTIFICATE is required", result.stderr)

    def test_cleanup_on_success_and_each_credential_failure(self):
        for failure in ("none", "import", "identity", "notary"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                security = work / "security"
                security.write_text('''#!/bin/bash
case "$1" in
  create-keychain) echo "${@: -1}" > "$TRACE" ;;
  import) [ "$FAILURE" != import ] ;;
  find-identity) [ "$FAILURE" = identity ] || echo '1) HASH "Developer ID Application: Test"' ;;
  delete-keychain) echo deleted >> "$TRACE" ;;
esac
''')
                security.chmod(0o755)
                notary = work / "xcrun"
                notary.write_text('#!/bin/bash\n[ "$FAILURE" != notary ]\n')
                notary.chmod(0o755)
                env = dict(os.environ, PATH=f"{work}:{os.environ['PATH']}",
                           TRACE=str(work / "trace"), FAILURE=failure,
                           APPLE_CERTIFICATE=base64.b64encode(b"test certificate").decode(),
                           APPLE_CERTIFICATE_PASSWORD="test password",
                           APPLE_SIGNING_IDENTITY="Developer ID Application: Test",
                           APPLE_API_KEY_BASE64=base64.b64encode(b"test API key").decode(),
                           APPLE_API_KEY_ID="test", APPLE_API_ISSUER_ID="test")
                result = subprocess.run(["bash", str(ROOT / "scripts/check-apple-credentials.sh")],
                                        env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, failure == "none", result.stderr)
                trace = (work / "trace").read_text().splitlines()
                self.assertEqual(trace[-1], "deleted")
                self.assertFalse(Path(trace[0]).parent.exists())
                self.assertNotIn("test password", result.stdout + result.stderr)


class PublishTests(unittest.TestCase):
    def test_unsigned_release_is_rejected_before_build_or_publish(self):
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/release.sh")],
            env=dict(os.environ, REQUIRE_DEVELOPER_ID="0",
                     TAURI_SIGNING_PRIVATE_KEY_PATH="/nonexistent/test-key"),
            capture_output=True, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsigned publishing is forbidden", result.stderr)

    def test_only_verified_drafts_are_published(self):
        for corrupt in (False, True):
            with self.subTest(corrupt=corrupt), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                assets = work / "assets"
                assets.mkdir()
                names = ["app.tar.gz", "app.tar.gz.sig", "latest.json", "app.dmg"]
                for name in names:
                    (assets / name).write_text(name)
                gh = work / "gh"
                gh.write_text('''#!/bin/bash
printf '%s\\n' "$*" >> "$TRACE"
case "$2" in
  create) [[ " $* " == *" --draft "* ]] || exit 5 ;;
  download)
    while [ "$1" != --dir ]; do shift; done
    cp "$ASSETS"/* "$2/"
    [ "$CORRUPT" = 0 ] || echo corrupt >> "$2/app.tar.gz"
    ;;
  edit) touch "$PUBLISHED" ;;
esac
''')
                gh.chmod(0o755)
                result = subprocess.run(
                    ["bash", str(ROOT / "scripts/publish-release.sh"), "v0.5.2", "Test notes",
                     *[str(assets / name) for name in names]],
                    env=dict(os.environ, PATH=f"{work}:{os.environ['PATH']}",
                             TRACE=str(work / "trace"), ASSETS=str(assets),
                             CORRUPT=str(int(corrupt)), PUBLISHED=str(work / "published")),
                    capture_output=True, text=True,
                )
                self.assertEqual(result.returncode == 0, not corrupt, result.stderr)
                self.assertEqual((work / "published").exists(), not corrupt)
                self.assertIn("--draft", (work / "trace").read_text())


class VersionTests(unittest.TestCase):
    def test_version_drift_and_invalid_tags_are_rejected(self):
        script = ROOT / "scripts/check-release-version.py"
        expected = "v" + json.loads((ROOT / "package.json").read_text())["version"]
        for tag in (expected, "v999.999.999", "not-a-tag"):
            result = subprocess.run(["python3", str(script), tag], capture_output=True, text=True)
            self.assertEqual(result.returncode == 0, tag == expected, result.stderr)


class CertificateUploadTests(unittest.TestCase):
    def test_invalid_certificate_never_updates_secrets(self):
        spec = importlib.util.spec_from_file_location("certificate", ROOT / "scripts/set-apple-certificate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            certificate = Path(directory) / "test.p12"
            certificate.write_bytes(b"invalid")
            calls = []

            def run(args, **kwargs):
                calls.append(args)
                return subprocess.CompletedProcess(args, 1 if args[1] == "import" else 0)

            with patch.object(module.sys, "argv", ["helper", str(certificate)]), \
                 patch.object(module.getpass, "getpass", return_value="test password"), \
                 patch.object(module.subprocess, "run", side_effect=run):
                with self.assertRaisesRegex(SystemExit, "No GitHub secrets were changed"):
                    module.main()
            self.assertTrue(any(args[1] == "delete-keychain" for args in calls))
            self.assertFalse(any(args[0] == "gh" for args in calls))
