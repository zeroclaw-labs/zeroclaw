#!/usr/bin/env python3
"""Validate release archive evidence; never stage or package product contents.

Python 3.8+ standard library only. Inventory validation precedes extraction;
regular payloads are streamed into caller-owned empty scratch, without using
version-dependent tar extraction APIs or trusting archived links.
"""

import argparse
import hashlib
import json
import pathlib
import stat
import sys
import tarfile
import zipfile


FORMAT = "zeroclaw-release-archive-evidence-v1"


def member_path(name):
    if not isinstance(name, str) or not name or name.startswith("/"):
        raise ValueError("unsafe member path")
    if "\\" in name or ":" in name or "\x00" in name:
        raise ValueError("unsafe member path")
    normalized = name.rstrip("/")
    if any(part in ("", ".", "..") for part in normalized.split("/")):
        raise ValueError("unsafe member path")
    return normalized


def digest_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def input_record(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError("input file is not regular: " + str(path))
    return {"bytes": path.stat().st_size, "sha256": digest_file(path)}


def manifest(args):
    files = {}
    for path in [args.binary] + args.companion:
        name = member_path(path.name)
        if name in files:
            raise ValueError("duplicate input filename")
        files[name] = input_record(path)
    web = args.web_dist
    if web.is_symlink() or not web.is_dir():
        raise ValueError("web input directory is missing: " + str(web))
    directories = {"web", "web/dist"}
    for path in sorted(web.rglob("*")):
        name = member_path("web/dist/" + path.relative_to(web).as_posix())
        if path.is_symlink():
            raise ValueError("web input is not regular: " + name)
        if path.is_dir():
            directories.add(name)
        else:
            files[name] = input_record(path)
    value = {
        "format": FORMAT,
        "source": args.source,
        "target": args.target,
        "run": args.run,
        "attempt": args.attempt,
        "features": args.features,
        "main": args.binary.name,
        "files": files,
        "directories": sorted(directories),
    }
    write_json(args.output, value)


def read_manifest(path, expected_source, expected_target, expected_run, expected_attempt):
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or value.get("format") != FORMAT:
        raise ValueError("unsupported evidence manifest")
    if value.get("source") != expected_source:
        raise ValueError("source identity differs")
    if value.get("target") != expected_target:
        raise ValueError("target identity differs")
    if type(value.get("run")) is not int or value["run"] != expected_run:
        raise ValueError("run identity differs")
    if type(value.get("attempt")) is not int or value["attempt"] != expected_attempt:
        raise ValueError("attempt identity differs")
    files = value.get("files")
    directories = value.get("directories")
    if not isinstance(files, dict) or not files or not isinstance(directories, list):
        raise ValueError("invalid evidence inventory")
    for name, record in files.items():
        if member_path(name) != name or not isinstance(record, dict):
            raise ValueError("invalid evidence file")
        size, digest = record.get("bytes"), record.get("sha256")
        if type(size) is not int or size < 0 or not isinstance(digest, str):
            raise ValueError("invalid evidence payload record")
        if len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
            raise ValueError("invalid evidence payload digest")
    if value.get("main") not in files:
        raise ValueError("main is absent from evidence inventory")
    allowed_directories = {member_path(name) for name in directories}
    for name in files:
        for parent in pathlib.PurePosixPath(name).parents:
            if parent.as_posix() != ".":
                allowed_directories.add(parent.as_posix())
    if set(files) & allowed_directories:
        raise ValueError("evidence file/directory collision")
    return value, allowed_directories


def archive_members(source, zipped):
    seen = set()
    members = []
    for item in source.infolist() if zipped else source.getmembers():
        name = member_path(item.filename if zipped else item.name)
        if name in seen:
            raise ValueError("duplicate archive member: " + name)
        seen.add(name)
        directory = item.is_dir() if zipped else item.isdir()
        if zipped:
            kind = stat.S_IFMT(item.external_attr >> 16)
            valid = kind in (0, stat.S_IFDIR) if directory else kind in (0, stat.S_IFREG)
            size = item.file_size
        else:
            valid = directory or item.isfile()
            size = item.size
        if not valid:
            raise ValueError("nonregular archive member: " + name)
        members.append((name, directory, size, item))
    return members


def verify(args):
    value, allowed_directories = read_manifest(
        args.manifest, args.expected_source, args.expected_target, args.expected_run, args.expected_attempt
    )
    zipped = args.archive.name.endswith(".zip")
    if not zipped and not args.archive.name.endswith(".tar.gz"):
        raise ValueError("unsupported archive format")
    archive = zipfile.ZipFile(str(args.archive)) if zipped else tarfile.open(str(args.archive), "r:gz")
    with archive as source:
        members = archive_members(source, zipped)
        actual_files = {name for name, directory, _, _ in members if not directory}
        expected_files = set(value["files"])
        if expected_files - actual_files:
            raise ValueError("missing archive files")
        if actual_files - expected_files:
            raise ValueError("unexpected archive files")
        actual_directories = {name for name, directory, _, _ in members if directory}
        for name, _, _, _ in members:
            for parent in pathlib.PurePosixPath(name).parents:
                if parent.as_posix() != ".":
                    actual_directories.add(parent.as_posix())
        if allowed_directories - actual_directories:
            raise ValueError("missing archive directories")
        for name, directory, size, _ in members:
            if directory and name not in allowed_directories:
                raise ValueError("unexpected archive directory: " + name)
            if not directory and size != value["files"][name]["bytes"]:
                raise ValueError("payload size differs: " + name)
        # All member paths/types/inventory/sizes have been checked before any
        # extraction. The caller owns this scratch; existing contents are never
        # adopted, deleted, or overwritten.
        destination = args.extract_dir
        if destination.is_symlink() or (destination.exists() and not destination.is_dir()):
            raise ValueError("extraction directory is not empty")
        if destination.exists() and any(destination.iterdir()):
            raise ValueError("extraction directory is not empty")
        destination.mkdir(parents=True, exist_ok=True)
        for name, directory, _, item in members:
            output = destination.joinpath(*name.split("/"))
            if directory:
                output.mkdir(parents=True, exist_ok=True)
                continue
            output.parent.mkdir(parents=True, exist_ok=True)
            reader = source.open(item) if zipped else source.extractfile(item)
            if reader is None:
                raise ValueError("regular payload could not be read: " + name)
            digest, written = hashlib.sha256(), 0
            expected = value["files"][name]
            with reader, output.open("wb") as target:
                for chunk in iter(lambda: reader.read(1024 * 1024), b""):
                    written += len(chunk)
                    if written > expected["bytes"]:
                        raise ValueError("payload size differs: " + name)
                    digest.update(chunk)
                    target.write(chunk)
            if written != expected["bytes"]:
                raise ValueError("payload size differs: " + name)
            if digest.hexdigest() != expected["sha256"]:
                raise ValueError("payload digest differs: " + name)
    evidence = {
        "verdict": "PASS",
        "source": value["source"],
        "target": value["target"],
        "run": value["run"],
        "attempt": value["attempt"],
        "features": value["features"],
        "main": value["main"],
        "files": value["files"],
        "archive_sha256": digest_file(args.archive),
        "manifest_sha256": digest_file(args.manifest),
    }
    if args.evidence is not None:
        write_json(args.evidence, evidence)
    print("PASS archive verification: " + value["target"] + " / " + str(len(expected_files)) + " payloads")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("manifest", help="hash actual completed build inputs")
    build.add_argument("--binary", type=pathlib.Path, required=True)
    build.add_argument("--companion", type=pathlib.Path, action="append", default=[])
    build.add_argument("--web-dist", type=pathlib.Path, required=True)
    build.add_argument("--source", required=True)
    build.add_argument("--target", required=True)
    build.add_argument("--run", type=int, required=True)
    build.add_argument("--attempt", type=int, required=True)
    build.add_argument("--features", required=True)
    build.add_argument("--output", type=pathlib.Path, required=True)
    check = commands.add_parser("verify", help="check inventory then extract and hash regular payloads")
    check.add_argument("--manifest", type=pathlib.Path, required=True)
    check.add_argument("--archive", type=pathlib.Path, required=True)
    check.add_argument("--extract-dir", type=pathlib.Path, required=True)
    check.add_argument("--expected-source", required=True)
    check.add_argument("--expected-target", required=True)
    check.add_argument("--expected-run", type=int, required=True)
    check.add_argument("--expected-attempt", type=int, required=True)
    check.add_argument("--evidence", type=pathlib.Path)
    args = parser.parse_args()
    try:
        manifest(args) if args.command == "manifest" else verify(args)
    except (OSError, ValueError, KeyError, tarfile.TarError, zipfile.BadZipFile, EOFError) as error:
        print("archive verification: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
