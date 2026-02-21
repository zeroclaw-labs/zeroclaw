#!/usr/bin/env python3
"""Validate preset payload JSON files for community submission safety/quality."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

ID_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-_]{1,63}$")
SECRET_KEYWORDS = (
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "private_key",
    "access_key",
    "refresh_token",
    "authorization",
)
SECRET_VALUE_PATTERNS = (
    re.compile(r"^sk-[A-Za-z0-9]{16,}"),
    re.compile(r"^ghp_[A-Za-z0-9]{16,}"),
    re.compile(r"^xox[baprs]-[A-Za-z0-9-]{10,}"),
    re.compile(r"^AKIA[A-Z0-9]{16}$"),
    re.compile(r"^AIza[0-9A-Za-z\\-_]{20,}$"),
    re.compile(r"^Bearer\\s+.+", re.IGNORECASE),
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate preset payload JSON files."
    )
    parser.add_argument(
        "paths",
        nargs="+",
        help="Preset JSON files or directories containing preset JSON files.",
    )
    parser.add_argument(
        "--allow-unknown-packs",
        action="store_true",
        help="Allow pack IDs not present in src/onboard/feature_packs.rs.",
    )
    return parser.parse_args()


def find_repo_root(start: Path) -> Path:
    cursor = start.resolve()
    for candidate in [cursor, *cursor.parents]:
        if (candidate / "Cargo.toml").exists() and (candidate / "src").exists():
            return candidate
    raise FileNotFoundError("Could not locate repository root from current directory.")


def load_known_pack_ids(repo_root: Path) -> set[str]:
    source = repo_root / "src" / "onboard" / "feature_packs.rs"
    raw = source.read_text(encoding="utf-8")
    marker = "pub const FEATURE_PACKS"
    start = raw.find(marker)
    if start < 0:
        raise ValueError(f"Failed to find {marker!r} in {source}")
    end = raw.find("];", start)
    if end < 0:
        raise ValueError(f"Failed to locate FEATURE_PACKS closing marker in {source}")
    block = raw[start:end]
    ids = set(re.findall(r'id:\s*"([^"]+)"', block))
    if not ids:
        raise ValueError(f"No pack IDs parsed from {source}")
    return ids


def collect_json_files(paths: list[str]) -> list[Path]:
    files: list[Path] = []
    for item in paths:
        path = Path(item)
        if path.is_file():
            files.append(path)
            continue
        if path.is_dir():
            files.extend(sorted(path.rglob("*.json")))
            continue
        raise FileNotFoundError(f"Path not found: {path}")
    return files


def is_suspicious_secret_value(value: str) -> bool:
    return any(pattern.search(value) for pattern in SECRET_VALUE_PATTERNS)


def scan_for_secrets(node: Any, path: str, errors: list[str]) -> None:
    if isinstance(node, dict):
        for key, value in node.items():
            key_path = f"{path}.{key}" if path else key
            key_lower = key.lower()
            if any(keyword in key_lower for keyword in SECRET_KEYWORDS):
                errors.append(f"{key_path}: secret-like key is not allowed in preset payload")
            scan_for_secrets(value, key_path, errors)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            scan_for_secrets(value, f"{path}[{index}]", errors)
    elif isinstance(node, str) and is_suspicious_secret_value(node):
        errors.append(f"{path}: secret-like value is not allowed in preset payload")


def validate_payload(
    payload: dict[str, Any],
    known_packs: set[str],
    allow_unknown_packs: bool,
) -> list[str]:
    errors: list[str] = []

    schema_version = payload.get("schema_version")
    if not isinstance(schema_version, int) or schema_version < 1:
        errors.append("schema_version must be an integer >= 1")

    preset_id = payload.get("id")
    if not isinstance(preset_id, str) or not ID_PATTERN.fullmatch(preset_id):
        errors.append("id must match ^[a-z0-9][a-z0-9-_]{1,63}$")

    title = payload.get("title")
    if title is not None and (not isinstance(title, str) or not title.strip()):
        errors.append("title must be a non-empty string when provided")

    description = payload.get("description")
    if description is not None and (not isinstance(description, str) or not description.strip()):
        errors.append("description must be a non-empty string when provided")

    packs = payload.get("packs")
    if not isinstance(packs, list) or not packs:
        errors.append("packs must be a non-empty array of pack IDs")
    else:
        invalid_items = [item for item in packs if not isinstance(item, str) or not item.strip()]
        if invalid_items:
            errors.append("packs must only contain non-empty strings")
        duplicate_ids = sorted({pack for pack in packs if packs.count(pack) > 1})
        if duplicate_ids:
            errors.append(f"packs contains duplicate IDs: {', '.join(duplicate_ids)}")
        if not allow_unknown_packs:
            unknown_ids = sorted({pack for pack in packs if pack not in known_packs})
            if unknown_ids:
                errors.append(f"packs includes unknown IDs: {', '.join(unknown_ids)}")

    config_overrides = payload.get("config_overrides", {})
    if not isinstance(config_overrides, dict):
        errors.append("config_overrides must be an object when provided")

    metadata = payload.get("metadata", {})
    if not isinstance(metadata, dict):
        errors.append("metadata must be an object when provided")

    scan_for_secrets(payload, "", errors)
    return errors


def main() -> int:
    args = parse_args()
    repo_root = find_repo_root(Path.cwd())
    known_packs = load_known_pack_ids(repo_root)
    files = collect_json_files(args.paths)

    if not files:
        print("No preset JSON files found.", file=sys.stderr)
        return 1

    failed = False
    for file_path in files:
        try:
            payload = json.loads(file_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            failed = True
            print(f"[FAIL] {file_path}: invalid JSON ({exc})")
            continue

        if not isinstance(payload, dict):
            failed = True
            print(f"[FAIL] {file_path}: root JSON value must be an object")
            continue

        errors = validate_payload(
            payload=payload,
            known_packs=known_packs,
            allow_unknown_packs=args.allow_unknown_packs,
        )
        if errors:
            failed = True
            print(f"[FAIL] {file_path}")
            for issue in errors:
                print(f"  - {issue}")
        else:
            print(f"[OK]   {file_path}")

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
