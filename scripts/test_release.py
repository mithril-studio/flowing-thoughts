"""Release tooling tests: python3 -m unittest discover -s scripts -p 'test_*.py'."""
import base64
import importlib.util
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
