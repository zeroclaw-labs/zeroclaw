"""Materialize a portable ZIP and its Codex overlay from root plugin.json."""
import argparse
import json
from pathlib import Path
import zipfile

PACKAGE = Path(__file__).resolve().parent
COMPONENTS = (
    "plugin.json",
    ".agents/plugins/marketplace.json",
    "skills/onboard/SKILL.md",
    "assets/icon.svg",
    "README.md",
)


def compatibility_manifest():
    portable = json.loads((PACKAGE / "plugin.json").read_text())
    extension = portable["extensions"]["com.openai"]
    overlay = {key: portable[key] for key in ("name", "version", "description", "author", "repository")}
    overlay["skills"] = "./skills/"
    overlay["extensions"] = {"com.openai": {key: value for key, value in extension.items() if key != "interface"}}
    overlay["interface"] = extension["interface"]
    return overlay


def build(destination):
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    overlay = json.dumps(compatibility_manifest(), indent=2).encode() + b"\n"
    with zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED) as archive:
        for name in (*COMPONENTS, ".codex-plugin/plugin.json"):
            data = overlay if name == ".codex-plugin/plugin.json" else (PACKAGE / name).read_bytes()
            entry = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.external_attr = 0o644 << 16
            archive.writestr(entry, data)
    return destination


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    print(build(args.output))
