#!/usr/bin/env python3
"""Exercise dependency-policy boundaries with cargo-deny, without compiling crates.

Run after the real dependency checks have populated Cargo's cache. Fixtures are
local path dependencies with generated lockfiles; their names and versions test
the reviewed exceptions, not substitute implementations of those libraries.
The ordinary CI deny/audit commands still check real sources and advisories.
"""

import argparse
from collections import Counter
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile


REVIEWED_LICENSES = (
    ("memcore", "2.0.0", "AGPL-3.0-only"),
    ("portable-kernel", "1.9.0", "AGPL-3.0-only"),
    ("vault-kit", "0.1.0", "AGPL-3.0-only"),
    ("borrow-or-share", "0.2.4", "MIT-0"),
)
AGPL_PACKAGES = {name for name, _, license_id in REVIEWED_LICENSES
                 if license_id == "AGPL-3.0-only"}
RETIRED_RNG_PACKAGES = {"rand_chacha", "ppv-lite86"}
RETIRED_RNG_SERIES = {"rand": ("0.8.", "0.9."), "rand_core": ("0.9.",)}


def run(cargo, args, repo):
    command = [cargo, *args]
    result = subprocess.run(
        command, cwd=repo, text=True, capture_output=True,
        env={**os.environ, "CARGO_TERM_COLOR": "never", "CARGO_NET_OFFLINE": "true"},
    )
    return command, result


def failure_context(message, command, result):
    return (
        f"{message}\n$ {shlex.join(command)}\nexit: {result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


def fail(message, command, result):
    raise RuntimeError(failure_context(message, command, result))


class GraphViolation(RuntimeError):
    def __init__(self, message, command, result, agpl, rng):
        super().__init__(failure_context(message, command, result))
        self.agpl = set(agpl)
        self.rng = set(rng)


def check_graph(cargo, repo, manifest=None, *, workspace=False,
                selection=("-p", "zeroclawlabs"), extra_args=(),
                expected_package="zeroclawlabs", allow_reviewed_agpl=False,
                scope_label=None):
    manifest = manifest or repo / "Cargo.toml"
    scope = scope_label or ("workspace/all-features" if workspace else "default production")
    effective_selection = ["--workspace", "--all-features"] if workspace else list(selection)
    edges = "normal,build,dev" if workspace else "normal,build"
    lock = manifest.with_name("Cargo.lock")
    before = lock.read_bytes()
    command, result = run(cargo, [
        "tree", "--locked", "--offline", "--manifest-path", str(manifest),
        *effective_selection, *extra_args, "--target", "all", "--edges", edges,
        "--prefix", "none", "--format", "{p}",
    ], repo)
    if result.returncode != 0:
        fail(f"Could not inspect the {scope} dependency graph", command, result)
    if lock.read_bytes() != before:
        fail(f"{scope} graph inspection changed Cargo.lock", command, result)
    packages = set()
    for line in result.stdout.splitlines():
        if not line.strip():
            continue
        fields = line.split()
        if len(fields) < 2 or not fields[1].startswith("v") or len(fields[1]) == 1:
            fail(f"Unrecognized {scope} dependency entry: {line!r}", command, result)
        packages.add((fields[0], fields[1][1:]))
    names = {name for name, _ in packages}
    if expected_package not in names:
        fail(f"{scope} graph did not contain the selected root package", command, result)
    violations = []
    forbidden_agpl = sorted(
        (name, version) for name, version in packages
        if not workspace and not allow_reviewed_agpl and name in AGPL_PACKAGES
    )
    forbidden_rng = sorted((name, version) for name, version in packages
                           if name in RETIRED_RNG_PACKAGES
                           or version.startswith(RETIRED_RNG_SERIES.get(name, ())))
    if forbidden_agpl:
        violations.append(f"Optional AGPL packages entered the {scope} graph: {forbidden_agpl}")
    if forbidden_rng:
        violations.append(f"Retired RNG packages entered the {scope} graph: {forbidden_rng}")
    if violations:
        raise GraphViolation("; ".join(violations), command, result,
                             forbidden_agpl, forbidden_rng)
    boundary = "retired RNG" if (workspace or allow_reviewed_agpl) else "optional AGPL and retired RNG"
    print(f"PASS {scope} graph excludes {boundary} packages on all targets")


def write_package(path, name, version, license_id, dependencies="", workspace=False,
                  table="dependencies", members=()):
    path.mkdir(parents=True)
    (path / "Cargo.toml").write_text(
        "[package]\n"
        f"name = {json.dumps(name)}\nversion = {json.dumps(version)}\n"
        f"license = {json.dumps(license_id)}\nedition = \"2021\"\n"
        "[lib]\npath = \"lib.rs\"\n"
        + ("[workspace]\nresolver = \"2\"\n" if workspace else "")
        + (f"members = {json.dumps(members)}\ndefault-members = [\".\"]\n"
           if members else "")
        + (f"[{table}]\n" + dependencies if dependencies else ""),
        encoding="utf-8",
    )
    (path / "lib.rs").write_text("// Metadata-only dependency-policy fixture.\n",
                                 encoding="utf-8")


def write_fixture(directory, name, packages, wildcard=False,
                  root_name="zeroclaw-policy-fixture", table="dependencies",
                  member=False, optional=False):
    root = directory / name / "root"
    dependencies = []
    for index, (package, version, license_id) in enumerate(packages):
        dependency = root.parent / "deps" / str(index)
        write_package(dependency, package, version, license_id)
        requirement = "" if wildcard else f", version = {json.dumps('=' + version)}"
        activation = ", optional = true" if optional else ""
        dependencies.append(
            f"dep_{index} = {{ package = {json.dumps(package)}, "
            f"path = {json.dumps(str(dependency))}{requirement}{activation} }}\n"
        )
    if member:
        write_package(root, root_name, "0.0.0", "MIT", workspace=True, members=("member",))
    write_package(root / "member" if member else root,
                  "zeroclaw-policy-member" if member else root_name, "0.0.0", "MIT",
                  "".join(dependencies), workspace=not member, table=table)
    return root


def check_graph_rejection(cargo, repo, directory, name, packages, *,
                          expected_agpl=(), expected_rng=(), workspace=False,
                          table="dependencies", optional=False):
    root = write_fixture(directory, name, packages, root_name="zeroclawlabs",
                         table=table, member=workspace, optional=optional)
    manifest = root / "Cargo.toml"
    command, result = run(cargo, [
        "generate-lockfile", "--offline", "--manifest-path", str(manifest),
    ], repo)
    if result.returncode != 0:
        fail(f"{name}: fixture resolution failed", command, result)
    if workspace:
        # The fixture dependency lives only in a second workspace member.
        # The default root graph must stay clean before the full graph refuses it.
        check_graph(cargo, repo, manifest)
    try:
        check_graph(cargo, repo, manifest, workspace=workspace)
    except GraphViolation as error:
        if error.agpl != set(expected_agpl) or error.rng != set(expected_rng):
            raise RuntimeError(f"{name}: unexpected dependency-graph refusal\n{error}") from error
    else:
        raise RuntimeError(f"{name}: dependency graph accepted forbidden dependencies")
    print(f"PASS {name}")


def check_case(cargo, config, repo, directory, name, packages, check,
               expected_errors=None, wildcard=False):
    expected = Counter(expected_errors or {})
    root = write_fixture(directory, name, packages, wildcard=wildcard)
    manifest = str(root / "Cargo.toml")
    command, result = run(cargo, [
        "generate-lockfile", "--offline", "--manifest-path", manifest,
    ], repo)
    if result.returncode != 0:
        fail(f"{name}: fixture resolution failed", command, result)
    lock = root / "Cargo.lock"
    before = lock.read_bytes()
    # The short -c belongs to cargo-deny. The local Cargo launcher reserves
    # --config for Cargo's one-line configuration overrides.
    command, result = run(cargo, [
        "deny", "--offline", "--locked", "--format", "json",
        "--manifest-path", manifest, "check", "-c", str(config),
        "--disable-fetch", check,
    ], repo)
    records = []
    for line in result.stderr.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue  # The host Cargo wrapper may print its toolchain receipt.
        if isinstance(record, dict):
            records.append(record)
    summaries = [record["fields"] for record in records
                 if record.get("type") == "summary" and "fields" in record]
    diagnostics = [record.get("fields", {}) for record in records
                   if record.get("type") == "diagnostic"]
    errors = Counter(item.get("code") for item in diagnostics
                     if item.get("severity") in ("error", "bug"))
    tool_errors = [record for record in records if record.get("type") == "log"
                   and record.get("fields", {}).get("level") == "ERROR"]
    # cargo-deny 0.19.9 uses distinct check bits: bans=2 and licenses=4.
    # A nonzero exit alone is not evidence that the intended policy refused it.
    expected_status = {"bans": 2, "licenses": 4}[check] if expected else 0
    valid_summary = (len(summaries) == 1 and set(summaries[0]) == {check}
                     and summaries[0][check].get("errors") == sum(expected.values()))
    if (result.returncode != expected_status or not valid_summary
            or errors != expected or tool_errors):
        fail(f"{name}: expected {dict(expected)} policy errors; observed {dict(errors)}",
             command, result)
    if lock.read_bytes() != before:
        fail(f"{name}: cargo-deny changed the fixture lockfile", command, result)
    print(f"PASS {name}")


def next_patch(version):
    major, minor, patch = version.split(".")
    return f"{major}.{minor}.{int(patch) + 1}"


def main():
    repo = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cargo", default="cargo",
                        help="Cargo executable, or scripts/dev/cargo-local.sh on managed hosts")
    parser.add_argument("--config", type=Path, default=repo / "deny.toml")
    args = parser.parse_args()
    config = args.config.resolve(strict=True)
    cargo = str(Path(args.cargo).resolve()) if os.sep in args.cargo else args.cargo
    # Route B enables memory-tachi by default on zeroclawlabs; keep the lean
    # core profiles (--no-default-features, slim-control, and zeroclaw-memory
    # without `tachi`) strictly AGPL-free.
    check_graph(cargo, repo, allow_reviewed_agpl=True)
    check_graph(cargo, repo, extra_args=("--no-default-features",),
                scope_label="minimal (--no-default-features)")
    check_graph(cargo, repo, extra_args=("--no-default-features", "--features", "slim-control"),
                scope_label="slim-control")
    check_graph(cargo, repo, selection=("-p", "zeroclaw-memory"),
                expected_package="zeroclaw-memory",
                scope_label="zeroclaw-memory default")
    check_graph(cargo, repo, workspace=True)
    retired_rng = (
        ("rand", "0.8.6", "MIT"), ("rand", "0.9.4", "MIT"),
        ("rand_chacha", "0.3.1", "MIT"), ("rand_chacha", "0.9.0", "MIT"),
        ("rand_core", "0.9.5", "MIT"), ("ppv-lite86", "0.2.21", "MIT"),
    )
    # Retirement covers each old series, including a later patch release.
    later_retired_rng = tuple((name, next_patch(version), license_id)
                              for name, version, license_id in retired_rng)
    reviewed_rng_skips = (("rand_core", "0.6.4"), ("getrandom", "0.2.17"),
                          ("getrandom", "0.3.4"))
    duplicates = tuple((name, version, "MIT") for name, version in reviewed_rng_skips) + (
        ("rand_core", "0.10.1", "MIT"), ("getrandom", "0.4.3", "MIT"),
    )
    with tempfile.TemporaryDirectory(prefix="zeroclaw-dependency-policy-") as temporary:
        directory = Path(temporary)
        memcore = next(package for package in REVIEWED_LICENSES if package[0] == "memcore")
        check_graph_rejection(cargo, repo, directory, "windows-only-agpl-is-denied",
                              (memcore,), expected_agpl=(memcore[:2],),
                              table="target.'cfg(windows)'.dependencies")
        check_graph_rejection(cargo, repo, directory, "default-retired-rng-is-denied",
                              retired_rng, expected_rng=tuple(p[:2] for p in retired_rng))
        check_graph_rejection(cargo, repo, directory, "workspace-optional-windows-rng-is-denied",
                              (memcore, *later_retired_rng), workspace=True, optional=True,
                              table="target.'cfg(windows)'.dependencies",
                              expected_rng=tuple(p[:2] for p in later_retired_rng))
        check_graph_rejection(cargo, repo, directory, "workspace-dev-rng-is-denied",
                              (retired_rng[-1],), workspace=True, table="dev-dependencies",
                              expected_rng=(retired_rng[-1][:2],))
        cases = (
            ("reviewed-licenses", REVIEWED_LICENSES, "licenses", {}),
            ("unreviewed-license-packages", (
                ("zeroclaw-unreviewed-agpl", "1.0.0", "AGPL-3.0-only"),
                ("zeroclaw-unreviewed-mit0", "1.0.0", "MIT-0"),
            ), "licenses", {"rejected": 2}),
            ("license-exceptions-do-not-cover-new-versions", tuple(
                (name, next_patch(version), license_id)
                for name, version, license_id in REVIEWED_LICENSES
            ), "licenses", {"rejected": 4}),
            ("license-exceptions-do-not-cover-other-licenses", tuple(
                (name, version, "GPL-3.0-only") for name, version, _ in REVIEWED_LICENSES
            ), "licenses", {"rejected": 4}),
            ("reviewed-duplicate-versions", duplicates, "bans", {}),
            ("duplicate-skips-do-not-cover-new-versions", tuple(
                (name, next_patch(version) if (name, version) in reviewed_rng_skips else version,
                 license_id) for name, version, license_id in duplicates
            ), "bans", {"duplicate": 2}),
        )
        for name, packages, check, expected in cases:
            check_case(cargo, config, repo, directory, name, packages, check, expected)
        # Separate pairs ensure that restoring any one retired skip fails its
        # own probe; a third unskipped version must not mask that regression.
        for name, retired, other in (
            ("rand", "0.8.6", "0.10.3"), ("rand", "0.9.4", "0.10.3"),
            ("rand_chacha", "0.3.1", "0.9.0"), ("rand_core", "0.9.5", "0.10.1"),
        ):
            check_case(cargo, config, repo, directory, f"retired-{name}-{retired}-skip-is-denied",
                       ((name, retired, "MIT"), (name, other, "MIT")), "bans", {"duplicate": 1})
        check_case(cargo, config, repo, directory, "unversioned-path-is-denied",
                   (("zeroclaw-unversioned-path", "1.0.0", "MIT"),),
                   "bans", {"wildcard": 1}, wildcard=True)
    print("Dependency policy: both real graphs, graph rejection probes, "
          "and cargo-deny boundary cases passed")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError) as error:
        print(f"dependency policy smoke failed: {error}", file=sys.stderr)
        sys.exit(1)
