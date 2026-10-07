"""Synthetic CLI owner; never reads real account or config files."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

mode = os.environ.get("FIXTURE_ZEROCLAW_MODE", "ready")
if sys.argv[1:] == ["native-onboard", "--help"]:
    if mode != "incompatible":
        print("claude-code --client --provider-alias --agent-alias --model --risk-preset --expected-billing "
              "--accept-yolo --accept-api-billing --native-config-dir")
    sys.exit(0)
argv = sys.argv[1:]
assert argv[2:5] == ["native-onboard", "--client", "claude-code"]
root = Path(argv[1])
if mode == "no_refresh":
    sys.exit(0)
root.mkdir(mode=0o700, exist_ok=True)
call = {"argv": argv, "stdin": sys.stdin.read(), "pid": os.getpid(), "started_monotonic": time.monotonic()}
if mode == "multi_group_sleep":
    heartbeat = Path(os.environ["HOME"]) / "native-heartbeat"
    body = ("import signal,time\nfrom pathlib import Path\n"
            "signal.signal(signal.SIGINT, signal.SIG_IGN)\n"
            "path=Path(" + repr(str(heartbeat)) + ")\n"
            "while True:\n with path.open('ab') as stream: stream.write(b'x')\n time.sleep(.02)\n")
    child = subprocess.Popen([sys.executable, "-I", "-B", "-c", body],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             preexec_fn=lambda: os.setpgid(0, 0))
    call["native_child_pid"] = child.pid
    signal.signal(signal.SIGINT, signal.SIG_IGN)
(Path(os.environ["HOME"]) / "zeroclaw-call.json").write_text(json.dumps(call))
(root / "config.toml").write_text("synthetic-canonical-config")
def value(flag, default=None):
    return argv[argv.index(flag) + 1] if flag in argv else default
request = {"client": "claude-code", "provider_alias": value("--provider-alias"),
           "agent_alias": value("--agent-alias"), "model": value("--model"),
           "risk_preset": value("--risk-preset"), "expected_billing": value("--expected-billing"),
           "accept_yolo": "--accept-yolo" in argv, "accept_api_billing": "--accept-api-billing" in argv,
           "native_config_dir": value("--native-config-dir"), "auth_profile": "subscriber"}
if mode == "wrong_request":
    request["agent_alias"] = "other"
receipt = {"schema_version": 1, "transaction_id": "synthetic", "request": request,
           "root_identity": [root.stat().st_dev, root.stat().st_ino],
           "phase": "configured" if mode in {"configured", "sleep", "multi_group_sleep"} else "ready",
           "last_validation": None if mode in {"no_validation", "configured", "sleep", "multi_group_sleep"} else {
               "at_unix_seconds": int(os.environ.get("FIXTURE_VALIDATION_TIME", str(int(time.time())))),
               "model_provider": "claude_code_native." + request["provider_alias"], "model": request["model"]},
           "failure_stage": None}
if mode == "stale_validation":
    receipt["last_validation"]["at_unix_seconds"] = 10
receipt_path = root / "native-onboard.json"
if mode != "missing_receipt":
    if mode == "receipt_symlink":
        receipt_path.symlink_to(os.environ["FIXTURE_AUTH"])
    else:
        temporary = root / ".synthetic-receipt-new"
        temporary.write_text(json.dumps(receipt))
        temporary.chmod(0o600)
        os.replace(temporary, receipt_path)
if mode == "sleep":
    def cancelled(_signum, _frame):
        sys.exit(1)
    signal.signal(signal.SIGINT, cancelled)
    time.sleep(30)
if mode == "multi_group_sleep":
    time.sleep(30)
print("sk-ant-fixture-secret fixture-private-diagnostic")
print("sk-ant-fixture-secret", file=sys.stderr)
sys.exit(1 if mode == "nonzero" else 0)
