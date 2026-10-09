"""Process, terminal and wire helpers for application acceptance tests."""

import json
import os
from pathlib import Path
import re
import shlex
import select
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

from fixtures import ModelFixture, OidcFixture


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def wait_for(probe, description, timeout=60):
    deadline = time.monotonic() + timeout
    while True:
        result = probe()
        if result:
            return result
        if time.monotonic() >= deadline:
            raise AssertionError("timed out: " + description)
        time.sleep(0.1)


def http(url, method="GET", body=None, headers=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method,
                                     headers={"Content-Type": "application/json", **(headers or {})})
    # Do not use an inherited proxy for test-owned loopback endpoints.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        response = opener.open(request, timeout=5)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        raw = response.read(1024 * 1024)
        return response.code, json.loads(raw)


class RpcError(Exception):
    def __init__(self, error):
        self.error = error
        super().__init__(json.dumps(error))


class Rpc:
    def __init__(self, path, token=None, provider=None):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(10)
        self.socket.connect(str(path))
        self.stream = self.socket.makefile("rb")
        self.sequence = 0
        try:
            params = {"protocol_version": 1}
            if token is not None:
                params.update(auth_token=token, auth_provider=provider or "native")
            self.initialized = self.call("initialize", params)
        except BaseException:
            self.close()
            raise

    def close(self):
        self.stream.close()
        self.socket.close()

    def call(self, method, params=None):
        self.sequence += 1
        deadline = time.monotonic() + 10
        self.socket.sendall((json.dumps({"jsonrpc": "2.0", "id": self.sequence, "method": method,
                                        "params": params or {}}) + "\n").encode())
        while True:
            remaining = deadline - time.monotonic()
            require(remaining > 0, "RPC response deadline exceeded: " + method)
            self.socket.settimeout(remaining)
            line = self.stream.readline(8 * 1024 * 1024)
            require(bool(line), "RPC closed before response to " + method)
            response = json.loads(line)
            if response.get("id") != self.sequence:
                continue
            if "error" in response:
                raise RpcError(response["error"])
            require("result" in response, "RPC response missing result: " + method)
            return response["result"]


class Installation:
    def __init__(self, binaries, artifacts, *, oidc=False, approval=False, legacy=False, fault=None, timeout=60):
        self.temp = tempfile.TemporaryDirectory(prefix="zca-", dir="/tmp")
        self.root = Path(self.temp.name)
        self.config = self.root / "config"
        self.workspace = self.root / "workspace"
        self.config.mkdir(mode=0o700)
        self.workspace.mkdir()
        self.binaries = binaries
        self.artifacts = artifacts
        self.artifacts.mkdir(parents=True, exist_ok=True)
        self.secrets = {"acceptance-placeholder", "acceptance-provisioned-key"}
        self.model = ModelFixture()
        self.issuer = OidcFixture(self.root, self.secrets) if oidc else None
        self.daemon = None
        self.logs = []
        self.terminals = []
        self.clients = []
        self.timeout = timeout
        self.fault = fault
        self.socket = self.root / "daemon.sock"
        # The fixture configuration is the single source for allocated endpoints.
        self.ports = []
        for _ in range(2):
            reservation = socket.socket()
            reservation.bind(("127.0.0.1", 0))
            self.ports.append(reservation)
        gateway, wss = [port.getsockname()[1] for port in self.ports]
        self.gateway = f"http://127.0.0.1:{gateway}"
        self.wss = f"wss://127.0.0.1:{wss}"
        self.env = {"PATH": f"{binaries}:/usr/local/bin:/usr/bin:/bin", "HOME": str(self.root),
                    "XDG_CONFIG_HOME": str(self.root / "xdg"), "XDG_DATA_HOME": str(self.root / "xdg-data"),
                    "XDG_CACHE_HOME": str(self.root / "cache"), "TMPDIR": str(self.root),
                    "ZEROCLAW_CONFIG_DIR": str(self.config), "ZEROCLAW_SOCKET": str(self.socket),
                    "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "TERM": "xterm-256color", "RUST_LOG": "info"}
        source = "schema_version = 3\n"
        source += f'''
[providers.models.custom.acceptance]
api_key = "acceptance-placeholder"
uri = "{self.model.url}"
model = "acceptance-model"
wire_api = "chat_completions"
native_tools = true
[agents.acceptance]
model_provider = "custom.acceptance"
risk_profile = "acceptance"
runtime_profile = "acceptance"
[agents.acceptance.workspace]
path = "{self.workspace}"
[risk_profiles.acceptance]
level = "supervised"
auto_approve = {json.dumps([] if approval else ["file_write", "file_read"])}
always_ask = {json.dumps(["file_write"] if approval else [])}
[runtime_profiles.acceptance]
max_tool_iterations = 8
[reliability]
provider_retries = 0
provider_backoff_ms = 0
[gateway]
host = "127.0.0.1"
port = {gateway}
require_pairing = true
[memory]
backend = "sqlite"
embedding_provider = "none"
[sop]
sops_dir = "shared/sops"
execution_mode = "deterministic"
persist_runs = true
'''
        if fault == "initialize":
            source += "\n[security]\ntrust_daemon_uid = false\n"
        if oidc:
            source += f'''
[wss]
enabled = true
bind = "127.0.0.1"
port = {wss}
[permission_profiles.acceptance]
admin = true
grants = {{ memory = ["read"] }}
[oidc.acceptance]
issuer = "{self.issuer.url}"
audience = "zeroclaw"
client_id = "acceptance-service"
client_secret = "acceptance-placeholder"
claim_path = "groups"
interactive_clients = ["acceptance-client"]
service_clients = ["acceptance-service"]
profile_map = {{ acceptance-users = "acceptance" }}
service_profile_map = {{ acceptance-service = "acceptance" }}
'''
        if legacy:
            source = (Path(__file__).parent / "fixtures" / "local-before-oidc.toml").read_text()
            source = source.replace("@MODEL_URL@", self.model.url).replace("@WORKSPACE@", str(self.workspace))
            source = source.replace("@GATEWAY_PORT@", str(gateway))
        (self.config / "config.toml").write_text(source)
        (self.config / "config.toml").chmod(0o600)

    def __enter__(self):
        return self

    def __exit__(self, _kind, error, _traceback):
        errors = []

        def cleanup(operation):
            try:
                operation()
            except Exception as failure:
                errors.append(str(failure))

        if self.daemon is not None and self.daemon.poll() is not None:
            errors.append(f"daemon exited unexpectedly (status {self.daemon.returncode})")
        if error is None:
            for terminal in self.terminals:
                if not terminal.closed:
                    cleanup(terminal.screen)
        owned = self.owned_processes()
        for terminal in reversed(self.terminals):
            cleanup(terminal.close)
        for client in self.clients:
            cleanup(client.close)
        cleanup(self.stop)
        cleanup(lambda: self.finish_processes(owned))
        for port in self.ports:
            cleanup(port.close)
        cleanup(self.model.close)
        if self.issuer:
            cleanup(self.issuer.close)
        cleanup(self.export_evidence)
        if error is None:
            cleanup(self.model.verify)
            if self.issuer and self.issuer.errors:
                errors.extend(self.issuer.errors)
        cleanup(self.temp.cleanup)
        if error or errors:
            messages = ([str(error)] if error else []) + ["cleanup/fixture: " + message for message in errors]
            raise AssertionError(self.redact("; ".join(messages))) from None

    def owned_processes(self):
        # Match the unique installation environment and exact executable before
        # acquiring pidfds. These remain attached to the same process even if
        # its numeric PID is reused. Includes ZeroCode's auto-started daemon.
        owned = []
        marker = ("ZEROCLAW_CONFIG_DIR=" + str(self.config)).encode()
        for process in Path("/proc").glob("[0-9]*"):
            try:
                if (marker in (process / "environ").read_bytes().split(b"\0")
                        and (process / "exe").resolve() in (self.binaries / "zeroclaw", self.binaries / "zerocode")):
                    owned.append(os.pidfd_open(int(process.name)))
            except (OSError, PermissionError):
                continue
        return owned

    @staticmethod
    def finish_processes(owned):
        for handle in owned:
            try:
                signal.pidfd_send_signal(handle, signal.SIGTERM)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 3
        for handle in owned:
            try:
                if not select.select([handle], [], [], max(0, deadline - time.monotonic()))[0]:
                    try:
                        signal.pidfd_send_signal(handle, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                require(bool(select.select([handle], [], [], 3)[0]), "owned application did not exit during cleanup")
            finally:
                os.close(handle)

    def export_evidence(self):
        evidence = {"model_requests": len(self.model.requests), "model_warmups": self.model.warmups,
                    "stream_requests": sum(row.get("stream") is True for row in self.model.requests),
                    "unused_responses": len(self.model.steps), "errors": self.model.errors}
        (self.artifacts / "fixture.json").write_text(self.redact(json.dumps(evidence, indent=2)))
        if self.issuer:
            (self.artifacts / "issuer.json").write_text(self.redact(json.dumps(
                {"calls": dict(self.issuer.calls), "errors": self.issuer.errors}, indent=2)))
        for log in self.logs:
            log.close()
            path = Path(log.name)
            (self.artifacts / path.name).write_text(self.redact(path.read_text(errors="replace")))
        traces = list(self.root.rglob("runtime-trace.jsonl"))
        for index, trace in enumerate(traces):
            (self.artifacts / f"runtime-{index}.log").write_text(self.redact(trace.read_text(errors="replace")))

    def redact(self, text):
        self.secrets.update(re.findall(r"X-Pairing-Code:\s*([A-Za-z0-9_-]+)", text))
        for secret in sorted(self.secrets, key=len, reverse=True):
            text = text.replace(secret, "[redacted]")
        text = re.sub(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+", "[jwt]", text)
        text = re.sub(r"-----BEGIN [^-]*PRIVATE KEY-----.*?-----END [^-]*PRIVATE KEY-----", "[private key]", text, flags=re.S)
        text = re.sub(r"(?i)Bearer\s+[A-Za-z0-9._~-]+", "Bearer [redacted]", text)
        text = re.sub(r"(?im)^.*(?:pairing code|pairing_code|otpauth|admin.token|zc_)[^\n]*$", "[credential line]", text)
        return text.replace(str(self.root), "<installation>")

    def cli(self, *args):
        result = subprocess.run([str(self.binaries / "zeroclaw"), "--config-dir", str(self.config), *args],
                                env=self.env, cwd=self.workspace, text=True, capture_output=True, timeout=self.timeout)
        require(result.returncode == 0, "CLI failed: " + self.redact(result.stderr + result.stdout))
        return result.stdout

    def release_ports(self):
        for port in self.ports:
            port.close()

    def start(self):
        self.release_ports()
        log = open(self.root / f"daemon-{len(self.logs)}.log", "w+")
        self.logs.append(log)
        self.daemon = subprocess.Popen([str(self.binaries / "zeroclaw"), "--config-dir", str(self.config), "daemon"],
                                       env=self.env, cwd=self.workspace, stdout=log, stderr=log, start_new_session=True)
        self.wait_ready()

    def wait_ready(self):
        def probe():
            if self.daemon:
                require(self.daemon.poll() is None, "daemon exited during startup")
            try:
                return self.socket.exists() and http(self.gateway + "/health")[0] == 200
            except (OSError, urllib.error.URLError):
                return False
        wait_for(probe, "daemon HTTP and IPC readiness", self.timeout)

    def stop(self):
        if self.daemon and self.daemon.poll() is None:
            os.killpg(self.daemon.pid, signal.SIGTERM)
            try:
                self.daemon.wait(timeout=8)
            except subprocess.TimeoutExpired:
                os.killpg(self.daemon.pid, signal.SIGKILL)
                self.daemon.wait(timeout=5)
        self.daemon = None

    def rpc(self, token=None, provider=None):
        client = Rpc(self.socket, token, provider)
        self.clients.append(client)
        return client

    def pair(self):
        path = self.config / "data" / "gateway-admin.token"
        wait_for(path.exists, "gateway admin token", self.timeout)
        admin = path.read_text().strip()
        self.secrets.add(admin)
        status, data = http(self.gateway + "/admin/paircode", headers={"x-zeroclaw-admin-token": admin})
        require(status == 200, "pairing code request failed")
        code = data["pairing_code"]
        require(bool(code), "no pairing code available")
        self.secrets.add(code)
        status, data = http(self.gateway + "/pair", "POST", headers={"X-Pairing-Code": code})
        require(status == 200 and bool(data.get("token")), "pairing failed")
        self.secrets.add(data["token"])
        return data["token"]

    def tui(self, *, token=None, provider=None, remote=False):
        self.release_ports()
        terminal = Terminal(self, token=token, provider=provider, remote=remote)
        self.terminals.append(terminal)
        return terminal


class Terminal:
    def __init__(self, install, token=None, provider=None, remote=False):
        self.install = install
        self.name = f"tui-{len(install.terminals)}"
        self.tmux_socket = install.root / (self.name + ".tmux")
        self.closed = False
        args = [str(install.binaries / "zerocode"), "--config-dir", str(install.config), "--agent", "acceptance"]
        env = dict(install.env)
        if token:
            env["ZEROCLAW_AUTH_TOKEN"] = token
        if remote:
            certs = install.root / "client"
            if not certs.exists():
                install.cli("security", "issue-client-cert", "--name", "acceptance-client", "--out-dir", str(certs))
            args += ["--connect", install.wss, "--tls-ca-cert", str(certs / "ca.crt"),
                     "--tls-client-cert", str(certs / "client.crt"), "--tls-client-key", str(certs / "client.key")]
            # ZeroCode's independent client configuration supplies provider selection.
            (install.config / "zerocode-config.toml").write_text(f'[connection.wss]\nauth_provider = "{provider or "native"}"\n')
        self.log = install.root / (self.name + ".log")
        # env is passed to tmux itself; its server is unique to this test.
        self.env = env
        command = shlex.join(args)
        tmux_config = install.root / "tmux.conf"
        tmux_config.write_text("set-option -g remain-on-exit on\nset-option -g default-shell /bin/sh\n")
        ready = install.root / (self.name + ".ready")
        os.mkfifo(ready, mode=0o600)
        self.raw = install.root / (self.name + ".raw")
        self.run("-f", str(tmux_config), "new-session", "-d", "-x", "120", "-y", "45", "-s", "acceptance",
                 "-c", str(install.workspace), "read -r start < " + shlex.quote(str(ready)) + "; exec " + command)
        self.run("pipe-pane", "-o", "-t", "acceptance:0.0", "cat > " + shlex.quote(str(self.raw)))
        self.pid = int(self.run("display-message", "-p", "-t", "acceptance:0.0", "#{pane_pid}"))
        def release():
            try:
                descriptor = os.open(ready, os.O_WRONLY | os.O_NONBLOCK)
            except OSError as error:
                if error.errno == 6:  # ENXIO: shell has not opened the FIFO yet.
                    return False
                raise
            with os.fdopen(descriptor, "w") as stream:
                stream.write("start\n")
            return True
        wait_for(release, "terminal launch handshake", 10)

    def run(self, *args, check=True):
        return subprocess.run(["tmux", "-S", str(self.tmux_socket), *args], env=self.env,
                              capture_output=True, text=True, check=check, timeout=10).stdout

    def screen(self):
        screen = self.run("capture-pane", "-p", "-t", "acceptance:0.0")
        self.log.write_text(screen)
        dead = self.run("display-message", "-p", "-t", "acceptance:0.0", "#{pane_dead}").strip()
        details = self.raw.read_text(errors="replace") if dead != "0" and self.raw.exists() else screen
        require(dead == "0", "ZeroCode exited: " + self.install.redact(details)[-3000:])
        return screen

    def expect(self, text):
        wait_for(lambda: text in self.screen(), "ZeroCode displays " + text, self.install.timeout)
        self.save()

    def send(self, text):
        self.run("send-keys", "-t", "acceptance:0.0", "-l", "--", text)
        self.key("Enter")

    def chat(self):
        self.expect("Connected")
        for _ in range(3):
            self.key("M-f")
        self.expect("Type to chat")

    def state(self, marker):
        wait_for(lambda: marker in self.run("display-message", "-p", "-t", "acceptance:0.0", "#{pane_title}"),
                 "ZeroCode terminal state " + marker, self.install.timeout)
        self.save()

    def key(self, key):
        self.run("send-keys", "-t", "acceptance:0.0", key)

    def save(self):
        screen = self.run("capture-pane", "-p", "-t", "acceptance:0.0", check=False)
        target = self.install.artifacts / (self.name + ".txt")
        with target.open("a") as output:
            title = self.run("display-message", "-p", "-t", "acceptance:0.0", "#{pane_title}", check=False)
            output.write("Terminal title: " + self.install.redact(title) + self.install.redact(screen) + "\n")

    def close(self):
        if self.closed:
            return
        self.closed = True
        self.save()
        if self.run("display-message", "-p", "-t", "acceptance:0.0", "#{pane_dead}", check=False).strip() == "0":
            os.kill(self.pid, signal.SIGTERM)
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            dead = self.run("display-message", "-p", "-t", "acceptance:0.0", "#{pane_dead}", check=False).strip()
            if dead != "0":
                break
            time.sleep(0.1)
        self.run("kill-server", check=False)
        if self.raw.exists():
            (self.install.artifacts / (self.name + "-wire.txt")).write_text(
                self.install.redact(self.raw.read_text(errors="replace")))
