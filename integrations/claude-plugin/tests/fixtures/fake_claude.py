"""Synthetic CLI fixture. It never reads an operator's credentials."""

import json
import os
from pathlib import Path
import sys
import time

with open(os.environ["FIXTURE_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps({"argv": sys.argv[1:],
                          "config_dir": os.environ.get("CLAUDE_CONFIG_DIR"),
                          "api_key_present": bool(os.environ.get("ANTHROPIC_API_KEY"))}) + "\n")

mode = os.environ.get("FIXTURE_MODE", "valid")
if sys.argv[1:] == ["--version"]:
    print(os.environ.get("FIXTURE_VERSION", "2.1.289 (Claude Code)") if mode != "wrong_binary" else "2.1.289 (Other CLI)")
    sys.exit(int(os.environ.get("FIXTURE_VERSION_EXIT", "0")))
elif sys.argv[1:] == ["auth", "status", "--json"]:
    if mode == "timeout":
        time.sleep(1)
    elif mode == "overflow":
        sys.stdout.write("x" * 100000)
        sys.stdout.flush()
        time.sleep(30)
    elif mode == "malformed":
        print("not JSON: fixture-private-diagnostic")
    elif mode == "nonzero":
        print("fixture-private-diagnostic sk-ant-fixture-secret", file=sys.stderr)
        print("fixture-private-diagnostic sk-ant-fixture-secret")
        sys.exit(1)
    else:
        if mode == "hostile_stderr":
            print("sk-ant-fixture-secret fixture-private-diagnostic", file=sys.stderr)
        if mode == "child_pipe":
            import subprocess
            subprocess.Popen([sys.executable, "-c", "import time; time.sleep(3)"])
        print(Path(os.environ["FIXTURE_AUTH"]).read_text(encoding="utf-8"))
        sys.exit(int(os.environ.get("FIXTURE_AUTH_EXIT", "0")))
else:
    print("unexpected command", file=sys.stderr)
    sys.exit(91)
