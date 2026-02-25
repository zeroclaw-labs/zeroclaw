"""__SKILL_NAME__ — ZeroClaw Skill (Python / WASI)

Transform text in various ways.
Protocol: read JSON from stdin, write JSON result to stdout.
Build:    pip install componentize-py
          componentize-py -d wit/ -w zeroclaw-skill componentize main -o tool.wasm
Test:     zeroclaw skill test . --args '{"text":"hello world","transform":"uppercase"}'
"""

import sys
import json


TRANSFORMS = {
    "uppercase": str.upper,
    "lowercase": str.lower,
    "reverse": lambda s: s[::-1],
    "title": str.title,
}


def run(args: dict) -> dict:
    if not isinstance(args, dict):
        raise TypeError("args must be a dict")
    text = args.get("text", "")
    transform = args.get("transform", "").lower()

    if transform not in TRANSFORMS:
        keys = ", ".join(TRANSFORMS.keys())
        return {"success": False, "output": "", "error": f"unknown transform '{transform}' — use: {keys}"}

    result = TRANSFORMS[transform](text)
    return {"success": True, "output": result, "error": None}


def main():
    raw = sys.stdin.read()
    try:
        args = json.loads(raw)
    except json.JSONDecodeError as exc:
        sys.stdout.write(json.dumps({"success": False, "output": "", "error": f"invalid JSON: {exc}"}))
        sys.stdout.flush()
        return
    try:
        result = run(args)
    except Exception as exc:
        result = {"success": False, "output": "", "error": str(exc)}

    sys.stdout.write(json.dumps(result))
    sys.stdout.flush()


if __name__ == "__main__":
    main()
