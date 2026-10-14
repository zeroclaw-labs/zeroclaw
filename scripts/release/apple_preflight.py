#!/usr/bin/env python3
"""Check optional Apple credential groups before expensive release compilation.

The workflow's APPLE_* environment is the source of truth. This checks group
completeness and read-only notarization authentication, not certificate import,
signing identity validity, or whether Apple will accept the finished bundle.
Those remain Tauri's checks. No keychain, credential profile, temporary secret
file, or GITHUB_ENV entry is created; the workflow exports secrets at bundling.
Signing requires a certificate and identity; an empty PKCS#12 password is valid
configuration and is forwarded unchanged to Tauri by the workflow.
"""

import os
import re
import subprocess
import time


SIGNING = ("APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY")
NOTARY = ("APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID")


def authenticate(credentials):
    command = [
        "xcrun", "notarytool", "history",
        "--apple-id", credentials["APPLE_ID"],
        "--password", credentials["APPLE_PASSWORD"],
        "--team-id", credentials["APPLE_TEAM_ID"],
    ]
    for attempt in range(3):
        try:
            result = subprocess.run(
                command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT, timeout=60,
            )
        except subprocess.TimeoutExpired:
            reason = "request timed out"
            transient = True
        except OSError:
            print("::error::Cannot run notarytool. Check the runner's Xcode installation.")
            return False
        else:
            if result.returncode == 0:
                print("Apple notarization authentication passed (read-only history request).")
                return True
            # Tool output can include credentials and account history. Only
            # report a recognized HTTP status, never the output or command.
            match = re.search(rb"HTTP status code:\s*(\d{3})\b", result.stdout)
            status = int(match.group(1)) if match else None
            reason = f"HTTP {status}" if status else "unclassified notarytool failure"
            transient = status in (429, 500, 502, 503, 504)
        if not transient or attempt == 2:
            print(
                f"::error::Apple notarization preflight failed: {reason}. "
                "Check APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID and Apple's service status. "
                "Raw tool output is suppressed to protect credentials."
            )
            return False
        delay = 5 * (attempt + 1)
        print(f"Apple notarization preflight: {reason}; retrying in {delay}s ({attempt + 2}/3).")
        time.sleep(delay)
    return False


def main():
    credentials = {name: os.environ.get(name, "") for name in SIGNING + NOTARY}
    for name, value in credentials.items():
        # The later GITHUB_ENV handoff uses one line per value.
        if value and (not value.strip() or "\n" in value or "\r" in value):
            print(f"::error::{name} must be a nonblank, single-line value.")
            return 1
    for group in (SIGNING, NOTARY):
        present = [name for name in group if credentials[name]]
        # Tauri passes the PKCS#12 password verbatim to `security import -P`,
        # including an empty password. Password-only configuration still
        # activates this group and must have its certificate and identity.
        missing = [name for name in group if name != "APPLE_CERTIFICATE_PASSWORD" and not credentials[name]]
        if present and missing:
            print(f"::error::Incomplete Apple credential group; missing {', '.join(missing)}.")
            return 1
    if credentials["APPLE_ID"] and not authenticate(credentials):
        return 1
    if credentials["APPLE_CERTIFICATE"]:
        print("Signing group complete; certificate import and signing identity are not checked until Tauri bundling.")
    else:
        print("Apple signing credentials absent; retaining the unsigned (ad-hoc) build.")
    if not credentials["APPLE_ID"]:
        print("Apple notarization credentials absent; retaining the build without notarization.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
