#!/usr/bin/env python3
"""Real daemon + zerocode first-contact smoke test; only the external IdP is mocked.

Run from the repository root after `cargo build -p zeroclaw -p zerocode --bins`:
  python3 tests/manual/test_zerocode_oidc_bootstrap.py

All services bind loopback. Files stay below target; credentials are synthetic.
The daemon has no users, devices, paired tokens, pending pairing code, or
separate enrollment listener. The client config directory starts empty and
stdin is closed. No existing instance is contacted, restarted, or reconfigured.
"""

import argparse
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import tomllib
import urllib.request
import urllib.parse

TOKEN = "synthetic-first-contact-access-token"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class IdentityProvider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def reply(self, value):
        raw = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        if self.path != "/.well-known/openid-configuration":
            self.send_error(404)
            return
        self.reply({
            "issuer": self.server.issuer,
            "device_authorization_endpoint": self.server.issuer + "/device",
            "token_endpoint": self.server.issuer + "/token",
            "introspection_endpoint": self.server.issuer + "/introspect",
        })

    def do_POST(self):
        fields = urllib.parse.parse_qs(self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode())
        if self.path == "/device":
            if fields.get("code_challenge_method") != ["S256"] or len(fields.get("code_challenge", [""])[0]) != 43:
                self.send_error(400, "S256 PKCE is required")
                return
            self.server.challenge = fields["code_challenge"][0]
            self.reply({"device_code": "synthetic-device-code", "user_code": "ABCD-EFGH",
                        "verification_uri": self.server.issuer + "/approve",
                        "expires_in": 600, "interval": 5})
        elif self.path == "/token":
            verifier = fields.get("code_verifier", [""])[0]
            challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
            if not verifier or challenge != self.server.challenge:
                self.send_error(400, "PKCE verifier mismatch")
                return
            self.reply({"access_token": TOKEN, "token_type": "Bearer", "expires_in": 3600})
        elif self.path == "/introspect":
            self.reply({"active": True, "token_type": "Bearer", "iss": self.server.issuer,
                        "sub": "previously-unknown-user", "client_id": "desktop",
                        "aud": "zeroclaw", "exp": int(time.time()) + 3600,
                        "groups": ["desktop-users"]})
        else:
            self.send_error(404)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zeroclaw", type=Path, default=Path("target/debug/zeroclaw"))
    parser.add_argument("--zerocode", type=Path, default=Path("target/debug/zerocode"))
    args = parser.parse_args()
    daemon_binary, client_binary = args.zeroclaw.resolve(), args.zerocode.resolve()
    for binary in [daemon_binary, client_binary]:
        if not binary.is_file():
            parser.error(f"build the binary first: {binary}")
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("ZEROCLAW_", "ZEROCODE_"))}
    target = Path("target").resolve()
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="oidc-first-contact-", dir=target) as temporary:
        root = Path(temporary)
        server_config, client_config = root / "daemon", root / "client"
        server_config.mkdir()
        client_config.mkdir()
        assert not list(client_config.iterdir())
        gateway_port, wss_port = free_port(), free_port()
        with http.server.ThreadingHTTPServer(("127.0.0.1", 0), IdentityProvider) as idp:
            idp.issuer = f"http://127.0.0.1:{idp.server_port}"
            thread = threading.Thread(target=idp.serve_forever, daemon=True)
            thread.start()
            data = server_config / "data"
            config = f'''schema_version = 4
[providers.models.ollama.fixture]
model = "test-model"
[risk_profiles.fixture]
[runtime_profiles.fixture]
[agents.fixture]
model_provider = "ollama.fixture"
risk_profile = "fixture"
runtime_profile = "fixture"
[gateway]
host = "127.0.0.1"
port = {gateway_port}
# Local test only: no native pairing code is minted. The OIDC certificate
# route must STILL verify its bearer and permission grants.
require_pairing = false
[wss]
enabled = true
bind = "127.0.0.1"
port = {wss_port}
[enroll]
enabled = false
[permission_profiles.desktop]
allowed_agents = ["*"]
allowed_tools = ["*"]
grants = {{ sessions = ["create", "read"], tools = ["execute"] }}
[oidc.corp]
issuer = "{idp.issuer}"
audience = "zeroclaw"
client_id = "gateway"
client_secret = "synthetic-client-secret"
validation = "introspection"
claim_path = "groups"
interactive_clients = ["desktop"]
profile_map = {{ "desktop-users" = "desktop" }}
'''
            (server_config / "config.toml").write_text(config)
            assert "[users." not in config and "paired_tokens" not in config
            env["ZEROCLAW_DATA_DIR"] = str(data)
            gateway = f"http://127.0.0.1:{gateway_port}"
            with (root / "daemon.log").open("wb") as logs:
                daemon = subprocess.Popen(
                    [str(daemon_binary), "--config-dir", str(server_config), "daemon"],
                    env=env, stdin=subprocess.DEVNULL, stdout=logs, stderr=logs)
                try:
                    deadline = time.monotonic() + 40
                    while True:
                        if daemon.poll() is not None:
                            raise AssertionError("isolated daemon exited during startup")
                        try:
                            with urllib.request.urlopen(gateway + "/health", timeout=1) as response:
                                if response.status == 200 and (data / "tls/ca.crt").is_file():
                                    break
                        except (OSError, TimeoutError):
                            pass
                        if time.monotonic() >= deadline:
                            raise AssertionError("isolated daemon did not become ready")
                        time.sleep(0.1)
                    ledger_path = data / "tls/ledger.db"
                    if ledger_path.exists():
                        with sqlite3.connect(ledger_path) as ledger:
                            assert ledger.execute("SELECT count(*) FROM issued_certs").fetchone()[0] == 0
                    destination = f"wss://127.0.0.1:{wss_port}/"
                    out = subprocess.run(
                        [str(client_binary), "login", "corp", destination,
                         "--gateway-url", gateway, "--config-dir", str(client_config)],
                        stdin=subprocess.DEVNULL, capture_output=True, timeout=35, env=env)
                    if out.returncode:
                        # The token must never be printed even on failure.
                        detail = out.stderr.decode(errors="replace").replace(TOKEN, "[REDACTED]")
                        raise AssertionError(f"first-contact login failed: {detail}")
                    assert TOKEN.encode() not in out.stdout + out.stderr
                    assert b"pairing code" not in out.stderr and b"SAS" not in out.stderr
                    local = tomllib.loads((client_config / "zerocode-config.toml").read_text())
                    saved = local["connection"]["logins"][destination]
                    assert saved["auth_token"] == TOKEN
                    assert saved["auth_provider"] == "oidc.corp"
                    assert Path(saved["tls"]["client_cert_path"]).is_file()
                    with sqlite3.connect(ledger_path) as ledger:
                        rows = ledger.execute("SELECT actor, token_hash FROM issued_certs WHERE status='active'").fetchall()
                        assert len(rows) == 1 and rows[0][0].startswith("oidc:") and rows[0][1] == ""
                    print("PASS: empty client + unknown device + no pairing code + enroll disabled -> OIDC -> mTLS initialize -> saved login")
                finally:
                    daemon.terminate()
                    try:
                        daemon.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait()
                    idp.shutdown()
                    thread.join(timeout=5)


if __name__ == "__main__":
    main()
