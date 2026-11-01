#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tool="${script_dir}/binary_size_report.sh"
repo_root="$(cd "${script_dir}/../.." && pwd -P)"
policy="${repo_root}/dev/ci/dependency-footprint.toml"
test_root="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$test_root"' EXIT
test_mode="${1:-suite}"
[[ "$test_mode" == suite || "$test_mode" == --comparison-input-regressions ]] \
    || { echo "unknown fixture mode: $test_mode" >&2; exit 2; }

# Variables that measure refuses or records would make fixture runs depend on the caller's environment.
scrubbed_variables='CARGO_PROFILE_.*|CARGO_TARGET_.*_(RUSTFLAGS|LINKER)|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS'
scrubbed_variables+='|CARGO_BUILD_RUSTFLAGS|CARGO_BUILD_TARGET|RUSTC|CARGO_BUILD_RUSTC|ZEROCLAW_BUILD_ID'
scrubbed_variables+='|CARGO_.*|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER'
while IFS= read -r name; do
    unset "$name"
done < <(compgen -e | grep -E "^(${scrubbed_variables})$" || true)
export CARGO_HOME="$test_root/cargo-home"
mkdir -p "$CARGO_HOME"

# Build input fixtures have no Cargo configuration. The real checkout does,
# and unresolved configuration must not be a positive comparability control.
fixture_repo="$test_root/fixture-repo"
mkdir -p "$fixture_repo"
cp "$repo_root/Cargo.toml" "$repo_root/Cargo.lock" "$fixture_repo/"
git init -q "$fixture_repo"
git -C "$fixture_repo" add Cargo.toml Cargo.lock
git -C "$fixture_repo" -c user.name='Binary Size Fixture' \
    -c user.email='fixture@example.invalid' -c commit.gpgsign=false \
    -c core.hooksPath=/dev/null commit -qm 'fixture source'

# A fake rustc fixes the host triple, and with it the distribution features
# resolved for an implicit target, whatever machine runs the suite.
fake_bin="$test_root/fake-bin"
mkdir -p "$fake_bin"
cat >"$fake_bin/rustc" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == "--version --verbose" ]] || exit 90
printf '%s\n' 'rustc 1.90.0 (fixture)' 'binary: rustc' "host: ${FAKE_RUSTC_HOST}" 'release: 1.90.0'
SH
chmod +x "$fake_bin/rustc"
export PATH="$fake_bin:$PATH"
export FAKE_RUSTC_HOST=x86_64-unknown-linux-gnu

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local file="$1"
    local expected="$2"
    grep -Fq -- "$expected" "$file" || fail "missing ${expected} in ${file}"
}

assert_line() {
    local file="$1"
    local expected="$2"
    grep -Fxq -- "$expected" "$file" || fail "missing line ${expected} in ${file}"
}

expect_failure() {
    local name="$1"
    local expected="$2"
    shift 2
    local status=0
    "$@" >"$test_root/${name}.out" 2>"$test_root/${name}.err" || status=$?
    [[ "$status" -ne 0 ]] || fail "${name}: command unexpectedly succeeded"
    assert_contains "$test_root/${name}.err" "$expected"
    if grep -Fq 'Traceback' "$test_root/${name}.err"; then
        fail "${name}: emitted a Python traceback"
    fi
}

edit_report() {
    python3 - "$1" "$2" "$3" <<'PY'
import json
import pathlib
import sys

source, destination, statement = sys.argv[1:]
report = json.loads(pathlib.Path(source).read_text(encoding="utf-8"))
exec(statement, {"report": report})
pathlib.Path(destination).write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
}

# The zeroclaw policy profiles the fake cargo builds, as id|mode|features|selection|bytes.
# A selection row lists the sorted features that the fake xtask resolves for it.
fake_profiles="$test_root/fake-profiles.txt"
cat >"$fake_profiles" <<'TXT'
agent-runtime|features|agent-runtime||2000
ci-all|features|ci-all||5000
foundation|no-default-features|||1000
hardware-probe|features|agent-runtime,probe,zeroclaw-tools/probe||6000
root-default|defaults|||3000
standard-distribution|selection|agent-runtime,channel-matrix|dist|4000
TXT

python3 - "$policy" "$fake_profiles" <<'PY'
import pathlib
import sys
import tomllib

policy = tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
expected = {
    profile["id"]: (profile["mode"], ",".join(sorted(profile.get("features", []))), profile.get("selection", ""))
    for profile in policy["profiles"]
    if profile["package"] == "zeroclaw"
}
known = {}
for line in pathlib.Path(sys.argv[2]).read_text(encoding="utf-8").splitlines():
    profile_id, mode, features, selection, _size = line.split("|")
    known[profile_id] = (mode, "" if mode == "selection" else features, selection)
if expected != known:
    print("FAIL: the zeroclaw policy profiles in dev/ci/dependency-footprint.toml no longer match", file=sys.stderr)
    print("the fake cargo table (fake_profiles) in scripts/ci/binary_size_report.test.sh:", file=sys.stderr)
    for profile_id in sorted(set(expected) | set(known)):
        if expected.get(profile_id) != known.get(profile_id):
            print(f"  {profile_id}: policy {expected.get(profile_id)}, fake {known.get(profile_id)}", file=sys.stderr)
    raise SystemExit(1)
PY

fake_cargo="$test_root/fake-cargo"
cat >"$fake_cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"$FAKE_CARGO_LOG"

if [[ "${1:-}" == "--version" ]]; then
    printf '%s\n' 'cargo 1.90.0 (fixture)'
    exit 0
fi

if [[ "${1:-}" == "run" ]]; then
    selection_target="${FAKE_EXPECT_TARGET:-$FAKE_RUSTC_HOST}"
    expected=(run --locked --quiet -p xtask --bin generate -- features --selection dist --target "$selection_target")
    [[ "$#" -eq "${#expected[@]}" && "$*" == "${expected[*]}" ]] || exit 91
    # Like the canonical dist_target_exclusions, 32-bit ARM targets drop a feature.
    case "$selection_target" in
        arm-unknown-linux-gnueabihf | armv7-unknown-linux-gnueabihf)
            printf '%s\n' 'agent-runtime' >"$FAKE_CARGO_LOG.dist"
            printf '%s\n' 'agent-runtime'
            ;;
        *)
            printf '%s\n' 'agent-runtime,channel-matrix' >"$FAKE_CARGO_LOG.dist"
            printf '%s\n' 'channel-matrix,agent-runtime'
            ;;
    esac
    exit 0
fi

[[ "${1:-}" == "build" ]] || exit 92
shift
release=false
locked=false
json=false
package=""
bin=""
target_dir=""
no_default=false
features=""
target=""
while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --release) release=true; shift ;;
        --locked) locked=true; shift ;;
        --message-format=json-render-diagnostics) json=true; shift ;;
        -p) package="$2"; shift 2 ;;
        --bin) bin="$2"; shift 2 ;;
        --target-dir) target_dir="$2"; shift 2 ;;
        --no-default-features) no_default=true; shift ;;
        --features) features="$2"; shift 2 ;;
        --target) target="$2"; shift 2 ;;
        *) exit 93 ;;
    esac
done
[[ "$release" == true && "$locked" == true && "$json" == true ]] || exit 94
[[ "$package" == zeroclaw && "$bin" == zeroclaw && -n "$target_dir" ]] || exit 94
[[ "$target" == "${FAKE_EXPECT_TARGET:-}" ]] || exit 95
# Each policy profile builds in its own directory, so the directory names the
# profile; its features must be the profile's, or for a selection profile the
# features the fake xtask resolved in this run.
profile="$(basename "$target_dir")"
size=""
while IFS='|' read -r row_id row_mode row_features _row_selection row_size; do
    [[ "$row_id" == "$profile" ]] || continue
    row_no_default=true
    if [[ "$row_mode" == defaults ]]; then
        row_no_default=false
    fi
    if [[ "$row_mode" == selection ]]; then
        row_features="$(cat "$FAKE_CARGO_LOG.dist")"
    fi
    if [[ "$no_default" == "$row_no_default" && "$features" == "$row_features" ]]; then
        size="$row_size"
    fi
done <"$FAKE_CARGO_PROFILES"
[[ -n "$size" ]] || exit 96
[[ "$profile" != "${FAKE_CARGO_FAIL_PROFILE:-}" ]] || exit 98
for entry in ${FAKE_CARGO_SIZES:-}; do
    if [[ "${entry%%=*}" == "$profile" ]]; then
        size="${entry#*=}"
    fi
done
if [[ -n "${FAKE_CARGO_TOUCH:-}" ]]; then
    printf '%s\n' "$profile" >"$FAKE_CARGO_TOUCH"
fi
effective_target="${target:-${CARGO_BUILD_TARGET:-}}"
out_dir="$target_dir"
if [[ -n "$effective_target" ]]; then
    out_dir="$out_dir/$effective_target"
fi
out_dir="$out_dir/release"
name=zeroclaw
if [[ "$effective_target" == *-windows-* ]]; then
    name=zeroclaw.exe
fi
if [[ "$profile" != "${FAKE_CARGO_SKIP_PROFILE:-}" ]]; then
    mkdir -p "$out_dir"
    : >"$out_dir/$name"
    if [[ "$size" -gt 0 ]]; then
        head -c "$size" /dev/zero >"$out_dir/$name"
    fi
fi
python3 - "$out_dir/$name" "$features" "$no_default" "$profile" <<'PY'
import json
import os
import pathlib
import sys

executable, requested, no_default, profile = sys.argv[1:]
enabled = requested.split(",") if requested else []
if no_default == "false":
    enabled = os.environ.get("FAKE_CARGO_DEFAULT_FEATURES", "default,fixture-default").split(",")
package = "path+" + pathlib.Path.cwd().as_uri() + "#zeroclaw@" + os.environ.get("FAKE_CARGO_WORKSPACE_VERSION", "0.8.5")
dependency_source = os.environ.get("FAKE_CARGO_DEPENDENCY_SOURCE", "registry+https://example.invalid/index")
dependency_version = os.environ.get("FAKE_CARGO_DEPENDENCY_VERSION", "1.0.0")
dependency = dependency_source + "#fixture-dependency@" + dependency_version
if os.environ.get("FAKE_CARGO_OPAQUE_PACKAGE_ID"):
    dependency = "fixture-dependency 1.0.0 (opaque source)"
dependency_features = os.environ.get("FAKE_CARGO_DEPENDENCY_FEATURES", "dep-base").split(",")

def emit(package, name, kind, features, executable):
    message = {"reason": "compiler-artifact", "package_id": package,
               "target": {"kind": [kind], "name": name}, "executable": executable, "fresh": True}
    if not os.environ.get("FAKE_CARGO_MISSING_FEATURES"):
        message["features"] = sorted(features)
    print(json.dumps(message))

emit(dependency, "fixture_dependency", "lib", dependency_features, None)
if os.environ.get("FAKE_CARGO_EXTRA_DEPENDENCY_VERSION"):
    other = dependency_source + "#fixture-dependency@" + os.environ["FAKE_CARGO_EXTRA_DEPENDENCY_VERSION"]
    # Distinct targets must not hide simultaneous versions of one package.
    emit(other, "build-script-build", "custom-build", dependency_features, None)
if os.environ.get("FAKE_CARGO_EXTRA_DEPENDENCY_SOURCE"):
    other = os.environ["FAKE_CARGO_EXTRA_DEPENDENCY_SOURCE"] + "#fixture-dependency@" + dependency_version
    emit(other, "fixture_dependency", "lib", dependency_features, None)
if os.environ.get("FAKE_CARGO_HOST_TARGET_VARIANT"):
    emit(dependency, "fixture_dependency", "lib", ["host-variant"], None)
emit(package, "zeroclaw", "lib", enabled, None)
print("a line that is not JSON")
if profile != os.environ.get("FAKE_CARGO_UNREPORTED_PROFILE"):
    emit(package, "zeroclaw", "bin", enabled, executable)
PY
if [[ "$profile" == "${FAKE_CARGO_EXTRA_BINARY_PROFILE:-}" ]]; then
    printf '{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"zeroclaw"},"executable":"%s","fresh":false}\n' \
        "$out_dir/other-$name"
fi
if [[ "$profile" == "${FAKE_CARGO_PATHLESS_PROFILE:-}" ]]; then
    printf '%s\n' '{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"zeroclaw"},"executable":null,"fresh":false}'
fi
if [[ "$profile" == "${FAKE_CARGO_DEEP_LINE_PROFILE:-}" ]]; then
    printf '%*s' 1000000 '' | tr ' ' '['
    printf '%*s\n' 1000000 '' | tr ' ' ']'
fi
printf '%s\n' '{"reason":"build-finished","success":true}'
SH
chmod +x "$fake_cargo"

run_measure() {
    local output="$1"
    local target_dir="$2"
    shift 2
    local status=0
    FAKE_CARGO_LOG="${FAKE_CARGO_LOG:-$test_root/fake-cargo.log}" \
        FAKE_CARGO_PROFILES="$fake_profiles" \
        bash "$tool" measure \
        --cargo "$fake_cargo" \
        --repo-root "$fixture_repo" \
        --target-dir "$target_dir" \
        --output "$output" \
        "$@" 2>"$test_root/measure.err" || status=$?
    if [[ "$status" -ne 0 ]]; then
        cat "$test_root/measure.err" >&2
    fi
    return "$status"
}

run_compare() {
    bash "$tool" compare "$@"
}

if [[ "$test_mode" == --comparison-input-regressions ]]; then
    FAKE_CARGO_PROFILES="$fake_profiles" python3 - \
        "$tool" "$fake_cargo" "$fixture_repo" "$test_root" <<'PY'
import json
import os
import pathlib
import shutil
import subprocess
import sys

tool, cargo, source, scratch = map(pathlib.Path, sys.argv[1:])
evidence = pathlib.Path(os.environ.get("BINARY_SIZE_REPORT_TEST_EVIDENCE_DIR", scratch / "receipts"))
evidence.mkdir(parents=True, exist_ok=True)
receipts = []

def repository(name):
    target = scratch / name
    shutil.copytree(source, target)
    return target

def measure(repo, name, profile="foundation", **overrides):
    path = evidence / (name + ".json")
    env = dict(os.environ, FAKE_CARGO_LOG=str(scratch / "regression-cargo.log"), **overrides)
    command = ["bash", str(tool), "measure", "--cargo", str(cargo), "--repo-root", str(repo),
               "--target-dir", str(scratch / "builds"), "--profile", profile, "--output", str(path)]
    result = subprocess.run(command, env=env, capture_output=True, text=True)
    if result.returncode:
        raise AssertionError(result.stderr)
    return path

def compare(name, before, after, expected):
    output = evidence / (name + "-comparison.json")
    result = subprocess.run(["bash", str(tool), "compare", str(before), str(after),
                             "--output", str(output)], capture_output=True, text=True)
    (evidence / (name + ".stdout")).write_text(result.stdout)
    (evidence / (name + ".stderr")).write_text(result.stderr)
    if result.returncode:
        raise AssertionError(result.stderr)
    [row] = json.loads(output.read_text())["measurements"]
    passed = row["comparable"] is expected and (expected or
              (row["delta_bytes"] is None and row["delta_percent"] is None))
    passed = passed and row["before_bytes"] >= 0 and row["after_bytes"] >= 0
    if expected:
        passed = passed and row["delta_bytes"] == row["after_bytes"] - row["before_bytes"]
    receipt = {"case": name, "expected_comparable": expected,
               "observed_comparable": row["comparable"], "passed": passed,
               "before_bytes": row["before_bytes"], "after_bytes": row["after_bytes"],
               "delta_bytes": row["delta_bytes"], "changed_inputs": row["changed_inputs"]}
    receipts.append(receipt)
    print(("PASS" if passed else "FAIL") + ": " + name + " " + json.dumps(receipt), flush=True)

def invalid_status(name, status):
    report = json.loads((evidence / "explicit-before.json").read_text())
    report["context"]["cargo_config"]["status"] = status
    path = evidence / (name + ".json")
    path.write_text(json.dumps(report))
    output = evidence / (name + "-comparison.json")
    result = subprocess.run(["bash", str(tool), "compare", str(path), str(path),
                             "--output", str(output)], capture_output=True, text=True)
    (evidence / (name + ".stdout")).write_text(result.stdout)
    (evidence / (name + ".stderr")).write_text(result.stderr)
    passed = result.returncode == 1 and "error: before report.context.cargo_config: malformed inspection record" in result.stderr
    passed = passed and "Traceback" not in result.stderr and not output.exists()
    receipt = {"case": name, "passed": passed, "exit": result.returncode,
               "traceback": "Traceback" in result.stderr, "stderr": result.stderr}
    receipts.append(receipt)
    print(("PASS" if passed else "FAIL") + ": " + name + " " + json.dumps(receipt), flush=True)

repo = repository("explicit-control")
a = measure(repo, "explicit-before")
b = measure(repo, "explicit-after")
compare("explicit-profile-positive-control", a, b, True)

repo = repository("defaults-control")
a = measure(repo, "defaults-before", "root-default", FAKE_CARGO_DEFAULT_FEATURES="default,old-default")
b = measure(repo, "defaults-after", "root-default", FAKE_CARGO_DEFAULT_FEATURES="default,new-default")
compare("changed-effective-defaults", a, b, False)

repo = repository("dependency-control")
a = measure(repo, "dependency-before", "agent-runtime", FAKE_CARGO_DEPENDENCY_FEATURES="old-dependency")
b = measure(repo, "dependency-after", "agent-runtime", FAKE_CARGO_DEPENDENCY_FEATURES="new-dependency")
compare("changed-dependency-feature-expansion", a, b, False)

repo = repository("config-control")
a = measure(repo, "config-before")
(repo / ".cargo").mkdir()
config = repo / ".cargo/config.toml"
config.write_text('[target.x86_64-unknown-linux-gnu]\nrustflags = ["-C", "target-feature=+crt-static"]\nlinker = "old-linker"\n')
b = measure(repo, "config-flags-after")
compare("changed-repository-rustflags", a, b, False)
config.write_text('[target.x86_64-unknown-linux-gnu]\nrustflags = ["-C", "target-feature=+crt-static"]\nlinker = "new-linker"\n')
c = measure(repo, "config-linker-after")
compare("changed-repository-linker", b, c, False)
compare("unresolved-configuration-self-comparison", c, c, False)

repo = repository("unknown-features-control")
a = measure(repo, "unknown-feature-message", FAKE_CARGO_MISSING_FEATURES="1")
compare("missing-cargo-feature-evidence", a, a, False)

known = json.loads((evidence / "explicit-before.json").read_text())
known["context"].pop("cargo_config", None)
for item in known["measurements"]:
    item.pop("effective_features", None)
missing = evidence / "missing-input-record.json"
missing.write_text(json.dumps(known))
compare("missing-comparison-input-record", missing, missing, False)
known["schema_version"] = 1
legacy = evidence / "legacy-input-record.json"
legacy.write_text(json.dumps(known))
compare("legacy-unknown-input-record", legacy, legacy, False)

# Version/source context is already recorded separately. Same named packages,
# targets and enabled features must still provide a size delta after updates.
repo = repository("dependency-version-control")
a = measure(repo, "dependency-version-before")
with (repo / "Cargo.lock").open("a") as stream:
    stream.write("\n# fixture dependency version update\n")
b = measure(repo, "dependency-version-after", FAKE_CARGO_DEPENDENCY_VERSION="2.0.0",
            FAKE_CARGO_SIZES="foundation=1100")
compare("ordinary-dependency-version-update", a, b, True)

repo = repository("dependency-source-control")
a = measure(repo, "dependency-source-before")
with (repo / "Cargo.lock").open("a") as stream:
    stream.write("\n# fixture dependency source update\n")
b = measure(repo, "dependency-source-after", FAKE_CARGO_DEPENDENCY_SOURCE="git+https://example.invalid/repository?rev=new",
            FAKE_CARGO_SIZES="foundation=1100")
compare("ordinary-dependency-source-update", a, b, True)

repo = repository("workspace-version-control")
a = measure(repo, "workspace-version-before")
with (repo / "Cargo.toml").open("a") as stream:
    stream.write("\n# fixture workspace version update\n")
b = measure(repo, "workspace-version-after", FAKE_CARGO_WORKSPACE_VERSION="0.8.6",
            FAKE_CARGO_SIZES="foundation=1100")
compare("ordinary-workspace-version-update", a, b, True)

repo = repository("simultaneous-version-control")
a = measure(repo, "simultaneous-versions", FAKE_CARGO_EXTRA_DEPENDENCY_VERSION="2.0.0")
compare("ambiguous-simultaneous-package-versions", a, a, False)

repo = repository("simultaneous-source-control")
a = measure(repo, "simultaneous-sources", FAKE_CARGO_EXTRA_DEPENDENCY_SOURCE="git+https://example.invalid/other")
compare("ambiguous-simultaneous-package-sources", a, a, False)

repo = repository("host-target-control")
a = measure(repo, "host-target-variants", FAKE_CARGO_HOST_TARGET_VARIANT="1")
compare("ambiguous-host-target-feature-variants", a, a, False)

repo = repository("opaque-package-control")
a = measure(repo, "opaque-package-id", FAKE_CARGO_OPAQUE_PACKAGE_ID="1")
compare("opaque-package-id-is-unknown", a, a, False)

invalid_status("cargo-config-status-list", [])
invalid_status("cargo-config-status-object", {})

(evidence / "receipts.json").write_text(json.dumps(receipts, indent=2) + "\n")
raise SystemExit(0 if all(receipt["passed"] for receipt in receipts) else 1)
PY
    exit "$?"
fi

expect_invalid() {
    local name="$1"
    local statement="$2"
    local expected="$3"
    local source="${4:-$test_root/report-a.json}"
    edit_report "$source" "$test_root/${name}.json" "$statement"
    expect_failure "$name" "$expected" run_compare "$test_root/${name}.json" "$source"
}

host="$(rustc --version --verbose | sed -n 's/^host: //p')"
[[ -n "$host" ]] || fail "could not read the rustc host triple"
host_key="$(printf '%s' "$host" | tr 'a-z.-' 'A-Z__')"

# A default run measures every zeroclaw policy profile with the release Cargo profile.
target_a="$test_root/target-a"
run_measure "$test_root/report-a.json" "$target_a" >"$test_root/measure-a.out"
python3 - "$test_root/report-a.json" "$repo_root" "$target_a" "$test_root/measure-a.out" "$fake_profiles" <<'PY'
import hashlib
import json
import pathlib
import sys
import tomllib

report_path, repo_root, target_dir, table_path, profiles_path = (pathlib.Path(value) for value in sys.argv[1:])
report = json.loads(report_path.read_text(encoding="utf-8"))
policy_bytes = (repo_root / "dev/ci/dependency-footprint.toml").read_bytes()
release = tomllib.loads((repo_root / "Cargo.toml").read_text(encoding="utf-8"))["profile"]["release"]
sizes = {line.split("|")[0]: int(line.split("|")[4]) for line in profiles_path.read_text(encoding="utf-8").splitlines()}


def zeros_digest(size):
    return hashlib.sha256(b"\0" * size).hexdigest()


assert report["schema_version"] == 2
assert report["kind"] == "binary_size"
assert report["evidence_units"] == {
    "binary_bytes": "measured",
    "cargo_package_name_version_pairs": "not measured",
    "runtime_memory": "not measured",
}
assert report["policy"] == {
    "digest_sha256": hashlib.sha256(policy_bytes).hexdigest(),
    "edge_kinds": ["normal", "build"],
}
assert report["cargo_profile"] == {"name": "release", "settings": release}
assert report["bin"] == "zeroclaw"
context = report["context"]
assert context["cargo_version"] == "cargo 1.90.0 (fixture)"
assert context["target"] == context["rustc_host"]
assert context["resolved_selections"] == {"dist": ["agent-runtime", "channel-matrix"]}
assert context["build_env"] == {}
assert type(context["git_dirty"]) is bool
assert len(context["git_worktree_digest_sha256"]) == 64
assert len(context["cargo_lock_sha256"]) == 64
assert [item["id"] for item in report["measurements"]] == sorted(sizes)
for item in report["measurements"]:
    size = sizes[item["id"]]
    assert item["bytes"] == size, item
    assert item["sha256"] == zeros_digest(size), item
    assert item["path"] == f"{item['id']}/release/zeroclaw", item
    assert (target_dir / item["path"]).stat().st_size == size, item
    assert item["package"] == "zeroclaw", item
    assert item["target_triple"] == context["target"], item
measurements = {item["id"]: item for item in report["measurements"]}
assert measurements["standard-distribution"]["resolved_inputs"] == {
    "mode": "selection",
    "no_default_features": True,
    "features": ["agent-runtime", "channel-matrix"],
    "selection": "dist",
}
assert measurements["root-default"]["resolved_inputs"] == {
    "mode": "defaults",
    "no_default_features": False,
    "features": [],
    "selection": None,
}
assert str(target_dir) not in json.dumps(report)
rows = {line.split()[0]: line.split() for line in table_path.read_text(encoding="utf-8").splitlines()}
assert rows["profile"] == ["profile", "bytes", "MiB", "sha256"], rows["profile"]
assert rows["foundation"] == ["foundation", "1000", "0.00", zeros_digest(1000)[:12]], rows["foundation"]
assert sorted(rows) == sorted([*sizes, "profile"]), sorted(rows)
PY
build_prefix='build --release --locked --message-format=json-render-diagnostics -p zeroclaw --bin zeroclaw'
assert_line "$test_root/fake-cargo.log" \
    "${build_prefix} --target-dir ${target_a}/standard-distribution --no-default-features --features agent-runtime,channel-matrix"
assert_line "$test_root/fake-cargo.log" "${build_prefix} --target-dir ${target_a}/root-default"
# Without --target the distribution features are resolved for the host triple.
assert_line "$test_root/fake-cargo.log" \
    "run --locked --quiet -p xtask --bin generate -- features --selection dist --target ${host}"

# Re-running is deterministic, and a bare wrapper invocation dispatches to measure.
FAKE_CARGO_LOG="$test_root/rerun.log" FAKE_CARGO_PROFILES="$fake_profiles" bash "$tool" \
    --cargo "$fake_cargo" \
    --repo-root "$fixture_repo" \
    --target-dir "$target_a" \
    --output "$test_root/report-a-rerun.json" >/dev/null 2>"$test_root/rerun.err"
cmp -s "$test_root/report-a.json" "$test_root/report-a-rerun.json" || fail "measure was not deterministic"
assert_contains "$test_root/rerun.log" "$build_prefix"

# Comparing two measurements reports per-policy-profile byte and percentage deltas.
FAKE_CARGO_SIZES='agent-runtime=2500 foundation=900 root-default=3001' \
    run_measure "$test_root/report-b.json" "$test_root/target-b" >/dev/null
run_compare "$test_root/report-a.json" "$test_root/report-b.json" \
    --output "$test_root/delta.json" >"$test_root/delta.out"
python3 - "$test_root/delta.json" "$test_root/delta.out" <<'PY'
import hashlib
import json
import pathlib
import sys

delta = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
table = {line.split()[0]: line.split() for line in pathlib.Path(sys.argv[2]).read_text(encoding="utf-8").splitlines()}


def zeros_digest(size):
    return hashlib.sha256(b"\0" * size).hexdigest()


assert delta["schema_version"] == 2
assert delta["kind"] == "binary_size_comparison"
assert delta["context"]["changed_fields"] == []
assert delta["context"]["before"] == delta["context"]["after"]
assert delta["bin"] == "zeroclaw"
assert delta["cargo_profile"]["name"] == "release"
rows = {row["id"]: row for row in delta["measurements"]}
assert [row["id"] for row in delta["measurements"]] == sorted(rows)
assert rows["agent-runtime"] == {
    "id": "agent-runtime",
    "comparable": True,
    "changed_inputs": {},
    "before_bytes": 2000,
    "after_bytes": 2500,
    "delta_bytes": 500,
    "delta_percent": 25.0,
    "before_sha256": zeros_digest(2000),
    "after_sha256": zeros_digest(2500),
}
assert rows["foundation"]["delta_bytes"] == -100
assert rows["foundation"]["delta_percent"] == -10.0
assert rows["root-default"]["delta_bytes"] == 1
assert rows["root-default"]["delta_percent"] == 0.03
assert rows["ci-all"]["delta_bytes"] == 0
assert rows["ci-all"]["delta_percent"] == 0.0
assert rows["ci-all"]["before_sha256"] == rows["ci-all"]["after_sha256"]
assert all(row["comparable"] and row["changed_inputs"] == {} for row in rows.values())
revision = delta["context"]["before"]["git_revision"][:12]
state = "dirty" if delta["context"]["before"]["git_dirty"] else "clean"
assert table["before"] == ["before", revision, state], table["before"]
assert table["after"] == ["after", revision, state], table["after"]
assert table["agent-runtime"] == ["agent-runtime", "2000", "2500", "+500", "+25.00%", "changed"], table
assert table["foundation"] == ["foundation", "1000", "900", "-100", "-10.00%", "changed"], table
assert table["root-default"] == ["root-default", "3000", "3001", "+1", "+0.03%", "changed"], table
assert table["ci-all"] == ["ci-all", "5000", "5000", "+0", "+0.00%", "same"], table
PY
run_compare "$test_root/report-a.json" "$test_root/report-b.json" \
    >"$test_root/delta-stdout.json" 2>"$test_root/delta-stdout.err"
cmp -s "$test_root/delta.json" "$test_root/delta-stdout.json" || fail "stdout comparison differs from --output"
assert_contains "$test_root/delta-stdout.err" '+25.00%'

# A zero-byte baseline has no percentage, a tiny shrink rounds to an unsigned zero,
# and a subset without selection policy profiles resolves no selection.
FAKE_CARGO_SIZES='foundation=0' \
    run_measure "$test_root/report-zero.json" "$test_root/target-zero" --profile foundation >/dev/null
FAKE_CARGO_LOG="$test_root/foundation-only.log" \
    run_measure "$test_root/report-foundation.json" "$test_root/target-zero" --profile foundation >/dev/null
if grep -q '^run ' "$test_root/foundation-only.log"; then
    fail "a measurement without selection policy profiles resolved a selection"
fi
FAKE_CARGO_SIZES='foundation=1000000' \
    run_measure "$test_root/report-large.json" "$test_root/target-zero" --profile foundation >/dev/null
FAKE_CARGO_SIZES='foundation=999999' \
    run_measure "$test_root/report-large-minus-one.json" "$test_root/target-zero" --profile foundation >/dev/null
run_compare "$test_root/report-zero.json" "$test_root/report-foundation.json" \
    --output "$test_root/zero-delta.json" >"$test_root/zero-delta.out"
run_compare "$test_root/report-large.json" "$test_root/report-large-minus-one.json" \
    --output "$test_root/tiny-delta.json" >/dev/null
python3 - "$test_root/report-foundation.json" "$test_root/zero-delta.json" "$test_root/tiny-delta.json" <<'PY'
import json
import math
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert [item["id"] for item in report["measurements"]] == ["foundation"]
assert report["context"]["resolved_selections"] == {}
[zero] = json.loads(pathlib.Path(sys.argv[2]).read_text(encoding="utf-8"))["measurements"]
assert zero["before_bytes"] == 0
assert zero["after_bytes"] == 1000
assert zero["delta_bytes"] == 1000
assert zero["delta_percent"] is None
tiny_text = pathlib.Path(sys.argv[3]).read_text(encoding="utf-8")
[tiny] = json.loads(tiny_text)["measurements"]
assert tiny["delta_bytes"] == -1
assert tiny["delta_percent"] == 0.0 and math.copysign(1.0, tiny["delta_percent"]) == 1.0
assert '"delta_percent": -0.0' not in tiny_text
PY
assert_contains "$test_root/zero-delta.out" 'n/a'

# A policy profile whose resolved features changed gets a row without a delta.
edit_report "$test_root/report-b.json" "$test_root/inputs-changed.json" '
report["context"]["resolved_selections"]["dist"] = ["agent-runtime", "channel-lark"]
item = next(item for item in report["measurements"] if item["id"] == "standard-distribution")
item["resolved_inputs"]["features"] = ["agent-runtime", "channel-lark"]
item["bytes"] = 4100
'
run_compare "$test_root/report-a.json" "$test_root/inputs-changed.json" \
    --output "$test_root/inputs-delta.json" >"$test_root/inputs-delta.out" 2>"$test_root/inputs-delta.err"
python3 - "$test_root/inputs-delta.json" <<'PY'
import json
import pathlib
import sys

delta = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert delta["context"]["changed_fields"] == ["resolved_selections"]
rows = {row["id"]: row for row in delta["measurements"]}
row = rows["standard-distribution"]
assert row["comparable"] is False
assert row["changed_inputs"] == {"features": {"added": ["channel-lark"], "removed": ["channel-matrix"]}}
assert row["before_bytes"] == 4000 and row["after_bytes"] == 4100
assert row["delta_bytes"] is None and row["delta_percent"] is None
assert all(other["comparable"] for key, other in rows.items() if key != "standard-distribution")
assert rows["agent-runtime"]["delta_bytes"] == 500
PY
assert_contains "$test_root/inputs-delta.out" 'not comparable: features +channel-lark -channel-matrix'
assert_contains "$test_root/inputs-delta.err" 'note: no delta for policy profile(s) standard-distribution'

# compare refuses whole reports that were not built the same way.
expect_invalid toolchain-mismatch 'report["context"]["rustc_version"] = "rustc 1.91.0 (fixture)"' \
    'incompatible toolchain or target context (differing: rustc_version)' "$test_root/report-b.json"
expect_invalid settings-mismatch 'report["cargo_profile"]["settings"]["lto"] = "thin"' \
    'incompatible Cargo profile (release settings differ)' "$test_root/report-b.json"
expect_failure policy-profile-sets \
    'incompatible policy profile sets (only before: agent-runtime, ci-all, hardware-probe, root-default, standard-distribution)' \
    run_compare "$test_root/report-a.json" "$test_root/report-foundation.json"

# The build environment is recorded, and compare refuses a different one.
host_linker_variable="CARGO_TARGET_${host_key}_LINKER"
export "${host_linker_variable}=cc"
RUSTFLAGS='-C opt-level=s' ZEROCLAW_BUILD_ID=fixture-build CARGO_TARGET_NOT_THE_HOST_LINKER=cc \
    run_measure "$test_root/report-env.json" "$test_root/target-zero" --profile foundation >/dev/null
unset "$host_linker_variable"
python3 - "$test_root/report-env.json" "$host_linker_variable" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert report["context"]["build_env"] == {
    sys.argv[2]: "cc",
    "RUSTFLAGS": "-C opt-level=s",
    "ZEROCLAW_BUILD_ID": "fixture-build",
}, report["context"]["build_env"]
PY
expect_failure build-env-mismatch \
    "incompatible build environment (differing: CARGO_TARGET_${host_key}_LINKER, RUSTFLAGS, ZEROCLAW_BUILD_ID)" \
    run_compare "$test_root/report-foundation.json" "$test_root/report-env.json"

# Reports are validated strictly before any comparison.
expect_invalid unknown-top-level-field 'report["extra"] = 1' 'before report: unknown field(s): extra'
expect_invalid unknown-measurement-field 'report["measurements"][0]["extra"] = 1' \
    'before report.measurements[0]: unknown field(s): extra'
expect_invalid unknown-context-field 'report["context"]["extra"] = 1' 'before report.context: unknown field(s): extra'
expect_invalid unknown-cargo-profile-field 'report["cargo_profile"]["extra"] = 1' \
    'before report.cargo_profile: unknown field(s): extra'
expect_invalid missing-context-field 'del report["context"]["build_env"]' \
    'before report.context: missing field(s): build_env'
expect_invalid schema-version-three 'report["schema_version"] = 3' 'before report: incompatible schema version'
expect_invalid schema-version-bool 'report["schema_version"] = True' 'before report: incompatible schema version'
expect_invalid wrong-kind 'report["kind"] = "dependency_footprint"' "before report.kind: expected 'binary_size'"
expect_invalid evidence-units 'report["evidence_units"]["binary_bytes"] = "not measured"' \
    'before report: malformed evidence units'
expect_invalid edge-kinds 'report["policy"]["edge_kinds"] = ["normal"]' \
    'before report.policy.edge_kinds: incompatible edge kinds'
expect_invalid policy-digest 'report["policy"]["digest_sha256"] = "0" * 64' \
    'before report.policy.digest_sha256: does not match the supplied policy'
expect_invalid other-bin 'report["bin"] = "zerocode"' "before report.bin: expected 'zeroclaw'"
expect_invalid other-cargo-profile 'report["cargo_profile"]["name"] = "release-fast"' \
    "before report.cargo_profile.name: expected 'release'"
expect_invalid settings-float 'report["cargo_profile"]["settings"]["opt-level"] = 1.5' \
    'before report.cargo_profile.settings.opt-level: unsupported value type float'
expect_invalid settings-null 'report["cargo_profile"]["settings"]["lto"] = None' \
    'before report.cargo_profile.settings.lto: unsupported value type NoneType'
expect_invalid settings-depth \
    'report["cargo_profile"]["settings"]["package"] = {"dep": {"nested": {"opt-level": 1}}}' \
    'before report.cargo_profile.settings.package.dep.nested: Cargo profile tables nest at most 3 levels'
expect_invalid settings-list-nested 'report["cargo_profile"]["settings"]["x"] = [[1]]' \
    'before report.cargo_profile.settings.x[0]: unsupported value type list'
expect_invalid settings-empty-key 'report["cargo_profile"]["settings"][""] = 1' \
    'before report.cargo_profile.settings: expected non-empty string keys'
expect_invalid git-revision 'report["context"]["git_revision"] = "not-a-revision"' \
    'before report.context.git_revision: malformed revision'
expect_invalid unsorted-selection-features 'report["context"]["resolved_selections"]["dist"].reverse()' \
    'before report.context.resolved_selections: expected sorted feature values'
expect_invalid build-env-name 'report["context"]["build_env"]["PATH"] = "/usr/bin"' \
    'before report.context.build_env: unexpected variable(s) for target'
expect_invalid build-env-value 'report["context"]["build_env"]["RUSTFLAGS"] = 1' \
    'before report.context.build_env.RUSTFLAGS: expected a string'
expect_invalid target-triple 'report["measurements"][0]["target_triple"] = "x86_64-unknown-linux-gnu/x"' \
    'before report.measurements[0].target_triple: does not match context.target'
expect_invalid measurement-package 'report["measurements"][0]["package"] = "zeroclaw-channels"' \
    'before report.measurements[0].package: does not match the supplied policy'
expect_invalid measurement-mode '
item = next(item for item in report["measurements"] if item["id"] == "foundation")
item["resolved_inputs"].update(mode="defaults", no_default_features=False)
' 'resolved_inputs.mode: does not match the supplied policy'
expect_invalid measurement-features '
item = next(item for item in report["measurements"] if item["id"] == "agent-runtime")
item["resolved_inputs"]["features"] = ["ci-all"]
' 'resolved_inputs.features: do not match the supplied policy'
expect_invalid measurement-selection '
item = next(item for item in report["measurements"] if item["id"] == "standard-distribution")
item["resolved_inputs"]["selection"] = "dist-broad"
' 'resolved_inputs.selection: does not match the supplied policy'
expect_invalid selection-features '
item = next(item for item in report["measurements"] if item["id"] == "standard-distribution")
item["resolved_inputs"]["features"] = ["agent-runtime"]
' 'resolved_inputs.features: do not match the resolved selection'
expect_invalid foreign-package-measurement '
report["measurements"].append({
    "id": "channels-minimal",
    "package": "zeroclaw-channels",
    "resolved_inputs": {"mode": "no-default-features", "no_default_features": True, "features": [], "selection": None},
    "target_triple": report["context"]["target"],
    "path": "channels-minimal/release/zeroclaw",
    "bytes": 1,
    "sha256": "0" * 64,
})
report["measurements"].sort(key=lambda item: item["id"])
' "policy profile 'channels-minimal' builds package 'zeroclaw-channels', not 'zeroclaw'"
expect_invalid boolean-bytes 'report["measurements"][0]["bytes"] = True' \
    'before report.measurements[0].bytes: expected an integer from 0 to 9223372036854775807'
expect_invalid negative-bytes 'report["measurements"][0]["bytes"] = -1' \
    'before report.measurements[0].bytes: expected an integer from 0 to 9223372036854775807'
expect_invalid oversized-bytes 'report["measurements"][0]["bytes"] = 2**63' \
    'before report.measurements[0].bytes: expected an integer from 0 to 9223372036854775807'
expect_invalid unsorted-measurements 'report["measurements"].reverse()' \
    'before report.measurements: expected measurements sorted by id'
expect_invalid duplicate-measurements 'report["measurements"].insert(0, dict(report["measurements"][0]))' \
    'before report.measurements: duplicate policy profile ids'
expect_invalid uppercase-digest \
    'report["measurements"][0]["sha256"] = report["measurements"][0]["sha256"].upper()' \
    'before report.measurements[0].sha256: malformed digest'
expect_invalid wrong-path 'report["measurements"][0]["path"] = "foundation/release/zeroclaw"' \
    'before report.measurements[0].path: does not match the policy profile, target, and zeroclaw binary'
expect_invalid unused-selection 'report["context"]["resolved_selections"]["extra"] = ["synthetic-feature"]' \
    'selection(s) not used by any measured policy profile: extra'
edit_report "$test_root/report-a.json" "$test_root/largest-bytes.json" '
for item in report["measurements"]:
    if item["id"] == "foundation":
        item["bytes"] = 2**63 - 1
'
run_compare "$test_root/largest-bytes.json" "$test_root/largest-bytes.json" >/dev/null 2>&1 \
    || fail "the largest allowed byte count was refused"
python3 - "$test_root/deep.json" <<'PY'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_text('{"a": ' + "[" * 1000000 + "]" * 1000000 + "}", encoding="utf-8")
PY
expect_failure deeply-nested-report 'nesting is too deep' run_compare "$test_root/deep.json" "$test_root/report-a.json"

# Build failures, missing binaries, and unexpected binary paths fail cleanly without a report.
FAKE_CARGO_SKIP_PROFILE=root-default expect_failure missing-binary \
    'policy profile root-default: binary not found at' \
    run_measure "$test_root/missing.json" "$test_root/target-missing"
[[ ! -e "$test_root/missing.json" ]] || fail "a measurement with a missing binary wrote a report"
FAKE_CARGO_UNREPORTED_PROFILE=root-default expect_failure unreported-binary \
    'policy profile root-default: cargo did not report a zeroclaw binary' \
    run_measure "$test_root/unreported.json" "$test_root/target-unreported"
[[ ! -e "$test_root/unreported.json" ]] || fail "a measurement without a reported binary wrote a report"
FAKE_CARGO_EXTRA_BINARY_PROFILE=root-default expect_failure several-binaries \
    'policy profile root-default: cargo reported several zeroclaw binaries' \
    run_measure "$test_root/several.json" "$test_root/target-several"
[[ ! -e "$test_root/several.json" ]] || fail "a measurement with several reported binaries wrote a report"
FAKE_CARGO_PATHLESS_PROFILE=root-default expect_failure pathless-binary \
    'policy profile root-default: cargo reported the zeroclaw binary without an executable path' \
    run_measure "$test_root/pathless.json" "$test_root/target-pathless"
[[ ! -e "$test_root/pathless.json" ]] || fail "a measurement with a pathless binary message wrote a report"
FAKE_CARGO_DEEP_LINE_PROFILE=foundation run_measure "$test_root/deep-line.json" "$test_root/target-deep-line" \
    --profile foundation >"$test_root/deep-line.out" 2>&1 \
    || fail "a deeply nested non-artifact line on cargo stdout was not skipped: $(tail -n 5 "$test_root/deep-line.out")"
FAKE_CARGO_FAIL_PROFILE=agent-runtime expect_failure build-failure \
    'policy profile agent-runtime: cargo build failed with status 98' \
    run_measure "$test_root/build-failure.json" "$test_root/target-failure"
[[ ! -e "$test_root/build-failure.json" ]] || fail "a failed build wrote a report"
configured_dir="$test_root/target-configured/foundation"
CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu expect_failure configured-build-target \
    "cargo built zeroclaw at ${configured_dir}/x86_64-unknown-linux-gnu/release/zeroclaw, expected ${configured_dir}/release/zeroclaw" \
    run_measure "$test_root/configured.json" "$test_root/target-configured" --profile foundation
[[ ! -e "$test_root/configured.json" ]] || fail "a binary at an unexpected path was measured"

# Refused arguments and environments stop before any build.
export FAKE_CARGO_LOG="$test_root/refused.log"
expect_failure output-directory "--output: ${test_root} is a directory" \
    run_measure "$test_root" "$test_root/target-refused" --profile foundation
RUSTC=rustc expect_failure rustc-override 'RUSTC set: the report records the rustc found on PATH' \
    run_measure "$test_root/refused.json" "$test_root/target-refused" --profile foundation
CARGO_BUILD_RUSTC=rustc expect_failure cargo-build-rustc-override 'CARGO_BUILD_RUSTC set' \
    run_measure "$test_root/refused.json" "$test_root/target-refused" --profile foundation
CARGO_PROFILE_RELEASE_LTO=thin expect_failure release-profile-env \
    'environment overrides the release Cargo profile (CARGO_PROFILE_RELEASE_LTO)' \
    run_measure "$test_root/refused.json" "$test_root/target-refused" --profile foundation
expect_failure foreign-policy-profile \
    'build the zeroclaw binary; refusing channels-minimal (package zeroclaw-channels)' \
    run_measure "$test_root/refused.json" "$test_root/target-refused" --profile channels-minimal
expect_failure unknown-policy-profile 'unknown policy profile id(s) for the supplied policy: missing-profile' \
    run_measure "$test_root/refused.json" "$test_root/target-refused" --profile missing-profile
expect_failure duplicate-policy-profile 'duplicate policy profile id(s): foundation' \
    run_measure "$test_root/refused.json" "$test_root/target-refused" --profile foundation --profile foundation
expect_failure removed-bin-option 'unrecognized arguments: --bin zeroclaw' \
    run_measure "$test_root/refused.json" "$test_root/target-refused" --bin zeroclaw
expect_failure removed-cargo-profile-option 'unrecognized arguments: --cargo-profile release' \
    run_measure "$test_root/refused.json" "$test_root/target-refused" --cargo-profile release
unset FAKE_CARGO_LOG
if [[ -e "$test_root/refused.log" ]] && grep -q '^build ' "$test_root/refused.log"; then
    fail "a refused measurement started a build"
fi
[[ ! -e "$test_root/refused.json" && ! -e "$test_root/target-refused" ]] || fail "refused arguments produced output"

# Variables of other Cargo profiles declared in Cargo.toml, such as release-fast, are not refused.
CARGO_PROFILE_RELEASE_FAST_CODEGEN_UNITS=4 \
    run_measure "$test_root/release-fast-env.json" "$test_root/target-zero" --profile foundation >/dev/null
cmp -s "$test_root/report-foundation.json" "$test_root/release-fast-env.json" \
    || fail "another Cargo profile's variable changed the report"

# An explicit target adds the triple directory, and Windows targets add the .exe suffix.
FAKE_EXPECT_TARGET=x86_64-pc-windows-msvc run_measure \
    "$test_root/report-windows.json" \
    "$test_root/target-windows" \
    --target x86_64-pc-windows-msvc \
    --profile foundation \
    --profile standard-distribution >/dev/null
python3 - "$test_root/report-windows.json" "$test_root/target-windows" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
target_dir = pathlib.Path(sys.argv[2])
assert report["context"]["target"] == "x86_64-pc-windows-msvc"
assert report["context"]["resolved_selections"] == {"dist": ["agent-runtime", "channel-matrix"]}
assert [item["id"] for item in report["measurements"]] == ["foundation", "standard-distribution"]
for item in report["measurements"]:
    assert item["target_triple"] == "x86_64-pc-windows-msvc", item
    assert item["path"] == f"{item['id']}/x86_64-pc-windows-msvc/release/zeroclaw.exe", item
    assert (target_dir / item["path"]).is_file(), item
PY
assert_contains "$test_root/fake-cargo.log" 'features --selection dist --target x86_64-pc-windows-msvc'
expect_failure target-mismatch 'incompatible toolchain or target context (differing: target)' \
    run_compare "$test_root/report-a.json" "$test_root/report-windows.json"
expect_invalid non-host-target-path 'report["measurements"][0]["path"] = "foundation/release/zeroclaw.exe"' \
    'before report.measurements[0].path: does not match the policy profile' "$test_root/report-windows.json"

# Without --target, distribution features are resolved for the host, so a native
# 32-bit ARM host measures the feature set of its own release artifact.
FAKE_CARGO_LOG="$test_root/arm-host.log" FAKE_RUSTC_HOST=armv7-unknown-linux-gnueabihf run_measure \
    "$test_root/report-arm-host.json" \
    "$test_root/target-arm-host" \
    --profile standard-distribution >/dev/null
python3 - "$test_root/report-arm-host.json" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert report["context"]["target"] == "armv7-unknown-linux-gnueabihf", report["context"]
assert report["context"]["resolved_selections"] == {"dist": ["agent-runtime"]}, report["context"]
[item] = report["measurements"]
assert item["resolved_inputs"]["features"] == ["agent-runtime"], item
assert item["path"] == "standard-distribution/release/zeroclaw", item
PY
assert_line "$test_root/arm-host.log" \
    'run --locked --quiet -p xtask --bin generate -- features --selection dist --target armv7-unknown-linux-gnueabihf'

# An explicit host target builds at a different path than the default, so that row has no delta.
FAKE_EXPECT_TARGET="$host" run_measure \
    "$test_root/report-host-target.json" \
    "$test_root/target-host" \
    --target "$host" \
    --profile foundation >/dev/null
run_compare "$test_root/report-foundation.json" "$test_root/report-host-target.json" \
    --output "$test_root/host-target-delta.json" >"$test_root/host-target-delta.out" 2>/dev/null
python3 - "$test_root/host-target-delta.json" "$host" <<'PY'
import json
import pathlib
import sys

[row] = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))["measurements"]
host = sys.argv[2]
assert row["comparable"] is False
assert row["changed_inputs"] == {
    "path": {"before": "foundation/release/zeroclaw", "after": f"foundation/{host}/release/zeroclaw"}
}, row["changed_inputs"]
assert row["delta_bytes"] is None and row["delta_percent"] is None
PY
assert_contains "$test_root/host-target-delta.out" \
    "not comparable: path foundation/release/zeroclaw -> foundation/${host}/release/zeroclaw"

# The source identity is re-checked after the builds, and in-repo target directories must be ignored.
source_repo="$test_root/source-repo"
git init -q "$source_repo"
printf '%s\n' '[package]' 'name = "fixture"' 'version = "0.1.0"' '' '[profile.release]' 'opt-level = 3' \
    >"$source_repo/Cargo.toml"
printf '%s\n' 'version = 4' >"$source_repo/Cargo.lock"
git -C "$source_repo" add Cargo.toml Cargo.lock
git -C "$source_repo" \
    -c user.name='Binary Size Fixture' \
    -c user.email='fixture@example.invalid' \
    -c commit.gpgsign=false \
    -c core.hooksPath=/dev/null \
    commit -qm 'fixture source'
run_measure "$source_repo/report.json" "$test_root/target-source" \
    --repo-root "$source_repo" --profile foundation >/dev/null
cp "$source_repo/report.json" "$test_root/source-first.json"
run_measure "$source_repo/report.json" "$test_root/target-source" \
    --repo-root "$source_repo" --profile foundation >/dev/null
cmp -s "$test_root/source-first.json" "$source_repo/report.json" || fail "an in-repo report changed the source identity"
if grep -q '^warning:' "$test_root/measure.err"; then
    fail "a clean source tree produced a dirty warning"
fi
python3 - "$source_repo/report.json" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert report["context"]["git_dirty"] is False
assert report["cargo_profile"] == {"name": "release", "settings": {"opt-level": 3}}
PY
FAKE_CARGO_TOUCH="$source_repo/build-side-effect.txt" expect_failure worktree-changed \
    'git source identity: worktree changed during measurement' \
    run_measure "$test_root/changed.json" "$test_root/target-source" --repo-root "$source_repo" --profile foundation
[[ ! -e "$test_root/changed.json" ]] || fail "a changed worktree still wrote a report"
run_measure "$test_root/dirty.json" "$test_root/target-source" \
    --repo-root "$source_repo" --profile foundation >/dev/null
assert_contains "$test_root/measure.err" 'has uncommitted or untracked changes; the report marks the source as dirty'
expect_failure unignored-target-dir 'is inside the repository but not ignored by Git' \
    run_measure "$test_root/unignored.json" "$source_repo/size-builds" --repo-root "$source_repo" --profile foundation
[[ ! -e "$source_repo/size-builds" ]] || fail "a refused target directory was created"

# Windows hosts are refused up front.
python3 - "$script_dir/binary_size_report.py" <<'PY'
import contextlib
import importlib.util
import io
import os
import sys

spec = importlib.util.spec_from_file_location("binary_size_report", sys.argv[1])
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)
os.name = "nt"
stderr = io.StringIO()
with contextlib.redirect_stderr(stderr):
    status = module.main(["compare", "before.json", "after.json"])
assert status == 1
assert "runs on Linux or macOS only" in stderr.getvalue(), stderr.getvalue()
PY

# CI invokes the default suite. The explicit subgroup exits above, so this
# single invocation cannot recurse back into the full suite.
bash "${script_dir}/binary_size_report.test.sh" --comparison-input-regressions

echo "binary size report fixture tests: pass"
