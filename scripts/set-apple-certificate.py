#!/usr/bin/env python3
"""Validate a local Developer ID .p12, then upload the matching GitHub secrets.

Run interactively: python3 scripts/set-apple-certificate.py /path/to/certificate.p12
The password is prompted without echo and is never stored in the repository.
"""
import base64
import getpass
import pathlib
import secrets
import subprocess
import sys
import tempfile

REPO = "mithril-studio/flowing-thoughts"
IDENTITY = '"Developer ID Application: Joost Dolstra (3P87HV73Y6)"'


def main():
    if len(sys.argv) != 2:
        sys.exit("Usage: python3 scripts/set-apple-certificate.py /path/to/certificate.p12")
    certificate = pathlib.Path(sys.argv[1]).resolve()
    data = certificate.read_bytes()
    password = getpass.getpass("Certificate export password (hidden): ")
    if not password:
        sys.exit("Use a non-empty export password.")
    with tempfile.TemporaryDirectory(prefix="ft-certificate-") as directory:
        keychain = str(pathlib.Path(directory) / "check.keychain-db")
        try:
            subprocess.run(["security", "create-keychain", "-p", secrets.token_hex(24), keychain], check=True)
            # Apple's security CLI requires the import password as an argument.
            # Never print this command or run the helper under a process tracer.
            result = subprocess.run(
                ["security", "import", str(certificate), "-k", keychain,
                 "-P", password, "-T", "/usr/bin/codesign"], capture_output=True,
            )
            if result.returncode:
                sys.exit("Certificate import failed. No GitHub secrets were changed.")
            result = subprocess.run(
                ["security", "find-identity", "-v", "-p", "codesigning", keychain],
                check=True, capture_output=True, text=True,
            )
            if IDENTITY not in result.stdout:
                sys.exit("Expected valid Developer ID identity not found. No secrets were changed.")
        finally:
            subprocess.run(["security", "delete-keychain", keychain], capture_output=True)
    for name, value in (
        ("APPLE_CERTIFICATE_BASE64", base64.b64encode(data).decode()),
        ("APPLE_CERTIFICATE_PASSWORD", password),
    ):
        subprocess.run(["gh", "secret", "set", name, "--repo", REPO], input=value, text=True, check=True)
    print("Validated and uploaded the certificate/password pair. Run the Apple credentials workflow next.")


if __name__ == "__main__":
    main()
