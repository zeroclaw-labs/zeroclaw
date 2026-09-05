#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tool="${script_dir}/dependency_footprint.sh"
fixture_root="${script_dir}/fixtures/dependency-footprint"
policy="${script_dir}/../../dev/ci/dependency-footprint.toml"
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local file="$1"
    local expected="$2"
    grep -Fq -- "$expected" "$file" || fail "missing ${expected} in ${file}"
}

replace_text() {
    python3 - "$1" "$2" "$3" <<'PY'
import pathlib
import sys

path, old, new = sys.argv[1:]
value = pathlib.Path(path).read_text(encoding="utf-8")
if old not in value:
    raise SystemExit(f"missing replacement text in {path}")
pathlib.Path(path).write_text(value.replace(old, new), encoding="utf-8")
PY
}

run_normalize() {
    local raw_dir="$1"
    local output="$2"
    bash "$tool" normalize \
        --policy "$fixture_root/policy.toml" \
        --raw-dir "$raw_dir" \
        --context "$fixture_root/context.json" \
        --output "$output"
}

cmp -s "$policy" "$fixture_root/policy.toml" || fail "fixture policy drifted from production policy"
cp -R "$fixture_root/raw" "$test_root/raw-a"
cp -R "$fixture_root/raw" "$test_root/raw-b"
run_normalize "$test_root/raw-a" "$test_root/report-a.json"
run_normalize "$test_root/raw-a" "$test_root/report-a-rerun.json"
cmp -s "$test_root/report-a.json" "$test_root/report-a-rerun.json" || fail "normalization was not deterministic"

python3 - "$test_root/report-a.json" <<'PY'
import json
import sys

report = json.load(open(sys.argv[1], encoding="utf-8"))
assert report["evidence_units"]["cargo_package_name_version_pairs"] == "measured"
assert report["evidence_units"]["binary_bytes"] == "not measured"
assert report["evidence_units"]["runtime_memory"] == "not measured"
foundation = next(item for item in report["profiles"] if item["id"] == "foundation")
assert foundation["counts"]["duplicate_version_names"] == 1
dep_v2 = next(
    item
    for item in foundation["enabled_features"]
    if item["name"] == "dep-crate" and item["version"] == "2.0.0"
)
assert dep_v2["features"] == ["__rustls", "feature-a"]
assert report["contracts"]
assert type(report["context"]["git_dirty"]) is bool
assert len(report["context"]["git_worktree_digest_sha256"]) == 64
assert len(report["context"]["cargo_lock_sha256"]) == 64
assert "/fixture/checkout" not in json.dumps(report)
assert report["context"]["resolved_selections"] == {
    "dist": ["agent-runtime", "channel-matrix"]
}
PY

replace_text "$test_root/raw-b/foundation.tree" 'dep-crate v1.0.0' 'dep-crate v1.1.0'
replace_text "$test_root/raw-b/foundation.tree" 'dep-crate v2.0.0' 'dep-crate v2.1.0'
run_normalize "$test_root/raw-b" "$test_root/report-b.json"
bash "$tool" compare "$test_root/report-a.json" "$test_root/report-b.json" --output "$test_root/delta.json"
assert_contains "$test_root/delta.json" '"version": "1.1.0"'
assert_contains "$test_root/delta.json" '"version": "1.0.0"'
python3 - "$test_root/delta.json" <<'PY'
import json
import sys

delta = json.load(open(sys.argv[1], encoding="utf-8"))
foundation = next(item for item in delta["profiles"] if item["id"] == "foundation")
assert foundation["feature_deltas"] == []
PY

cp -R "$fixture_root/raw" "$test_root/raw-feature-delta"
replace_text \
    "$test_root/raw-feature-delta/foundation.tree" \
    $'dep-crate v2.0.0\tfeatures:__rustls,feature-a (*)' \
    $'dep-crate v2.0.0\tfeatures:__rustls,feature-b (*)'
run_normalize "$test_root/raw-feature-delta" "$test_root/report-feature-delta.json"
bash "$tool" compare \
    "$test_root/report-a.json" \
    "$test_root/report-feature-delta.json" \
    --output "$test_root/feature-delta.json"
python3 - "$test_root/feature-delta.json" <<'PY'
import json
import sys

delta = json.load(open(sys.argv[1], encoding="utf-8"))
foundation = next(item for item in delta["profiles"] if item["id"] == "foundation")
assert foundation["count_deltas"]["enabled_features"] == 0
assert foundation["feature_deltas"] == [
    {
        "name": "dep-crate",
        "version": "2.0.0",
        "added_features": ["feature-b"],
        "removed_features": ["feature-a"],
    }
]
PY

cp "$test_root/report-b.json" "$test_root/lockfile-change.json"
python3 - "$test_root/lockfile-change.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["context"]["cargo_lock_sha256"] = "2" * 64
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
bash "$tool" compare "$test_root/report-a.json" "$test_root/lockfile-change.json" \
    --output "$test_root/lockfile-delta.json"
python3 - "$test_root/lockfile-delta.json" <<'PY'
import json
import sys

context = json.load(open(sys.argv[1], encoding="utf-8"))["context"]
assert context["changed_fields"] == ["cargo_lock_sha256"]
assert context["before"]["cargo_lock_sha256"] == "1" * 64
assert context["after"]["cargo_lock_sha256"] == "2" * 64
PY

cp "$test_root/report-b.json" "$test_root/context-mismatch.json"
python3 - "$test_root/context-mismatch.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["context"]["target"] = "aarch64-unknown-linux-gnu"
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare "$test_root/report-a.json" "$test_root/context-mismatch.json" 2>"$test_root/context-mismatch.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "incompatible target context was accepted"
assert_contains "$test_root/context-mismatch.err" 'incompatible toolchain or target context'

cp "$test_root/report-a.json" "$test_root/malformed-report.json"
python3 - "$test_root/malformed-report.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["profiles"][0]["counts"]["package_pairs"] += 1
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare "$test_root/malformed-report.json" "$test_root/report-a.json" 2>"$test_root/malformed-report.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "malformed report counts were accepted"
assert_contains "$test_root/malformed-report.err" 'values do not match profile records'

cp "$test_root/report-a.json" "$test_root/missing-root.json"
python3 - "$test_root/missing-root.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
profile = next(item for item in report["profiles"] if item["id"] == "foundation")
profile["package_pairs"] = [pair for pair in profile["package_pairs"] if pair["name"] != profile["package"]]
profile["counts"]["package_pairs"] -= 1
profile["counts"]["unique_package_names"] -= 1
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare "$test_root/missing-root.json" "$test_root/report-a.json" 2>"$test_root/missing-root.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "report without declared root package was accepted"
assert_contains "$test_root/missing-root.err" 'declared package'

cp "$test_root/report-a.json" "$test_root/stripped-contract.json"
python3 - "$test_root/stripped-contract.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["contracts"].pop()
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare "$test_root/stripped-contract.json" "$test_root/report-a.json" 2>"$test_root/stripped-contract.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "stripped contract record was accepted"
assert_contains "$test_root/stripped-contract.err" 'records do not match supplied policy'

cp "$test_root/report-a.json" "$test_root/false-contract.json"
python3 - "$test_root/false-contract.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
profile = next(item for item in report["profiles"] if item["id"] == "hardware-probe")
profile["enabled_features"] = [
    item for item in profile["enabled_features"] if item["name"] != "zeroclaw-tools"
]
profile["counts"]["enabled_features"] -= 1
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare "$test_root/false-contract.json" "$test_root/report-a.json" 2>"$test_root/false-contract.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "false passing contract record was accepted"
assert_contains "$test_root/false-contract.err" 'contract tools-probe-enabled'

cp "$test_root/report-a.json" "$test_root/boolean-schema.json"
python3 - "$test_root/boolean-schema.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["schema_version"] = True
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare "$test_root/boolean-schema.json" "$test_root/report-a.json" 2>"$test_root/boolean-schema.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "boolean report schema was accepted"
assert_contains "$test_root/boolean-schema.err" 'incompatible schema version'

cp "$test_root/report-a.json" "$test_root/missing-resolved-selection.json"
python3 - "$test_root/missing-resolved-selection.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["context"]["resolved_selections"].pop("dist")
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare \
    "$test_root/missing-resolved-selection.json" \
    "$test_root/report-a.json" \
    2>"$test_root/missing-resolved-selection.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "report missing policy-referenced resolved selection was accepted"
assert_contains "$test_root/missing-resolved-selection.err" 'keys do not match supplied policy (missing dist)'

cp "$test_root/report-a.json" "$test_root/extra-resolved-selection.json"
python3 - "$test_root/extra-resolved-selection.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
report = json.loads(path.read_text(encoding="utf-8"))
report["context"]["resolved_selections"]["extra"] = ["synthetic-feature"]
path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare \
    "$test_root/extra-resolved-selection.json" \
    "$test_root/report-a.json" \
    2>"$test_root/extra-resolved-selection.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "report with an extra resolved selection was accepted"
assert_contains "$test_root/extra-resolved-selection.err" 'keys do not match supplied policy (extra extra)'

for side in left right; do
    cp "$test_root/report-a.json" "$test_root/selection-input-mismatch-${side}.json"
done
python3 - \
    "$test_root/selection-input-mismatch-left.json" \
    "$test_root/selection-input-mismatch-right.json" <<'PY'
import json
import pathlib
import sys

for value in sys.argv[1:]:
    path = pathlib.Path(value)
    report = json.loads(path.read_text(encoding="utf-8"))
    profile = next(item for item in report["profiles"] if item["id"] == "standard-distribution")
    profile["resolved_inputs"]["features"] = ["not-real"]
    path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
set +e
bash "$tool" compare \
    "$test_root/selection-input-mismatch-left.json" \
    "$test_root/selection-input-mismatch-right.json" \
    2>"$test_root/selection-input-mismatch.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "selection profile with mismatched resolved inputs was accepted"
assert_contains "$test_root/selection-input-mismatch.err" 'features do not match captured selection'

fake_cargo="$test_root/fake-cargo"
cat >"$fake_cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

printf '%q ' "$@" >>"$FAKE_CARGO_LOG"
printf '\n' >>"$FAKE_CARGO_LOG"

if [[ "${1:-}" == "--version" ]]; then
    printf '%s\n' 'cargo 1.90.0 (fixture)'
    exit 0
fi

if [[ "${1:-}" == "run" ]]; then
    expected=(run --locked --quiet -p xtask --bin generate -- features --selection dist)
    if [[ -n "${FAKE_EXPECT_TARGET:-}" ]]; then
        expected+=(--target "$FAKE_EXPECT_TARGET")
    fi
    [[ "$#" -eq "${#expected[@]}" && "$*" == "${expected[*]}" ]] || exit 91
    printf '%s\n' "${FAKE_CARGO_SELECTION_OUTPUT:-agent-runtime,channel-matrix}"
    exit 0
fi

[[ "${1:-}" == "tree" ]] || exit 93
shift
package=""
features=""
edges=""
prefix=""
format=""
target=""
locked=false
no_default=false
while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --locked) locked=true; shift ;;
        --no-default-features) no_default=true; shift ;;
        --edges) edges="$2"; shift 2 ;;
        --prefix) prefix="$2"; shift 2 ;;
        --format) format="$2"; shift 2 ;;
        --features) features="$2"; shift 2 ;;
        --target) target="$2"; shift 2 ;;
        -p) package="$2"; shift 2 ;;
        *) exit 94 ;;
    esac
done
[[ "$locked" == true ]] || exit 95
[[ "$edges" == "normal,build" && "$prefix" == "none" ]] || exit 96
[[ "$format" == $'{p}\tfeatures:{f}' ]] || exit 97
[[ "$target" == "${FAKE_EXPECT_TARGET:-}" ]] || exit 98
case "${package}|${no_default}|${features}" in
    'zeroclaw|true|') profile=foundation ;;
    'zeroclaw|true|agent-runtime') profile=agent-runtime ;;
    'zeroclaw|false|') profile=root-default ;;
    'zeroclaw|true|agent-runtime,channel-matrix') profile=standard-distribution ;;
    'zeroclaw|true|ci-all') profile=ci-all ;;
    'zeroclaw|true|agent-runtime,probe,zeroclaw-tools/probe') profile=hardware-probe ;;
    'zeroclaw-channels|true|') profile=channels-minimal ;;
    'zeroclaw-channels|false|') profile=channels-default ;;
    *) exit 99 ;;
esac
[[ "$profile" != "${FAKE_CARGO_FAIL_PROFILE:-}" ]] || exit 100
cat "$FAKE_CARGO_FIXTURE_ROOT/raw/${profile}.tree"
SH
chmod +x "$fake_cargo"
touch "$test_root/fake-cargo.log"

run_fake_capture() {
    local output="$1"
    local target="${2:-}"
    local selection_output="${3:-agent-runtime,channel-matrix}"
    local fail_profile="${4:-}"
    local repo_root="${5:-}"
    local -a args=(--output "$output")
    if [[ -n "$target" ]]; then
        args+=(--target "$target")
    fi
    if [[ -n "$repo_root" ]]; then
        args+=(--repo-root "$repo_root")
    fi
    (
        cd "$test_root"
        CARGO="$fake_cargo" \
            FAKE_CARGO_FIXTURE_ROOT="$fixture_root" \
            FAKE_CARGO_LOG="$test_root/fake-cargo.log" \
            FAKE_CARGO_SELECTION_OUTPUT="$selection_output" \
            FAKE_CARGO_FAIL_PROFILE="$fail_profile" \
            FAKE_EXPECT_TARGET="$target" \
            bash "$tool" "${args[@]}"
    )
}

for suffix in a b; do
    run_fake_capture "$test_root/capture-${suffix}.json"
done
cmp -s "$test_root/capture-a.json" "$test_root/capture-b.json" || fail "capture was not deterministic"
python3 - "$test_root/capture-a.json" <<'PY'
import json
import sys

report = json.load(open(sys.argv[1], encoding="utf-8"))
context = report["context"]
assert type(context["git_dirty"]) is bool
assert len(context["git_worktree_digest_sha256"]) == 64
assert context["resolved_selections"] == {
    "dist": ["agent-runtime", "channel-matrix"]
}
PY
assert_contains "$test_root/fake-cargo.log" 'run --locked --quiet -p xtask --bin generate -- features --selection dist'
run_fake_capture "$test_root/capture-target.json" 'aarch64-unknown-linux-gnu'
assert_contains "$test_root/fake-cargo.log" '--target aarch64-unknown-linux-gnu'

source_repo="$test_root/source-repo"
git init -q "$source_repo"
printf '%s\n' 'fixture source' >"$source_repo/tracked.txt"
printf '%s\n' 'version = 4' >"$source_repo/Cargo.lock"
git -C "$source_repo" add Cargo.lock tracked.txt
git -C "$source_repo" \
    -c user.name='Dependency Footprint Fixture' \
    -c user.email='fixture@example.invalid' \
    commit -qm 'fixture source'
in_repo_output="$source_repo/report.json"
run_fake_capture "$in_repo_output" '' 'agent-runtime,channel-matrix' '' "$source_repo"
cp "$in_repo_output" "$test_root/in-repo-first.json"
run_fake_capture "$in_repo_output" '' 'agent-runtime,channel-matrix' '' "$source_repo"
cmp -s "$test_root/in-repo-first.json" "$in_repo_output" || fail "in-repo output changed its source identity"
python3 - "$in_repo_output" <<'PY'
import json
import sys

context = json.load(open(sys.argv[1], encoding="utf-8"))["context"]
assert context["git_dirty"] is False
assert len(context["git_worktree_digest_sha256"]) == 64
PY
cp "$source_repo/tracked.txt" "$test_root/tracked-before.txt"
set +e
run_fake_capture \
    "$source_repo/tracked.txt" \
    '' \
    'agent-runtime,channel-matrix' \
    '' \
    "$source_repo" \
    2>"$test_root/tracked-output.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "tracked output path was accepted"
assert_contains "$test_root/tracked-output.err" 'output path is tracked by Git'
cmp -s "$source_repo/tracked.txt" "$test_root/tracked-before.txt" || fail "tracked output was modified"

source_repo_link="$test_root/source-repo-link"
ln -s "$source_repo" "$source_repo_link"
set +e
run_fake_capture \
    "$source_repo_link/tracked.txt" \
    '' \
    'agent-runtime,channel-matrix' \
    '' \
    "$source_repo" \
    2>"$test_root/symlinked-parent-output.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "tracked output through a symlinked parent was accepted"
assert_contains "$test_root/symlinked-parent-output.err" 'output path is tracked by Git'
cmp -s "$source_repo/tracked.txt" "$test_root/tracked-before.txt" || fail "tracked output through a symlinked parent was modified"

external_sentinel="$test_root/external-sentinel.json"
printf '%s\n' 'sentinel' >"$external_sentinel"
symlink_output="$source_repo/symlink-report.json"
ln -s "$external_sentinel" "$symlink_output"
git -C "$source_repo" add symlink-report.json
git -C "$source_repo" \
    -c user.name='Dependency Footprint Fixture' \
    -c user.email='fixture@example.invalid' \
    commit -qm 'fixture symlink output'
cp "$external_sentinel" "$test_root/external-sentinel-before.txt"
set +e
run_fake_capture \
    "$symlink_output" \
    '' \
    'agent-runtime,channel-matrix' \
    '' \
    "$source_repo" \
    2>"$test_root/symlink-output.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "tracked symlink output was accepted"
assert_contains "$test_root/symlink-output.err" 'output path is an existing symlink'
cmp -s "$external_sentinel" "$test_root/external-sentinel-before.txt" || fail "external symlink target was modified"

normalize_symlink_output="$test_root/normalize-symlink.json"
ln -s "$external_sentinel" "$normalize_symlink_output"
set +e
run_normalize "$test_root/raw-a" "$normalize_symlink_output" 2>"$test_root/normalize-symlink.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "normalize accepted a symlink output"
assert_contains "$test_root/normalize-symlink.err" 'output path is an existing symlink'
cmp -s "$external_sentinel" "$test_root/external-sentinel-before.txt" || fail "normalize modified an external symlink target"

compare_symlink_output="$test_root/compare-symlink.json"
ln -s "$external_sentinel" "$compare_symlink_output"
set +e
bash "$tool" compare \
    "$test_root/report-a.json" \
    "$test_root/report-b.json" \
    --output "$compare_symlink_output" \
    2>"$test_root/compare-symlink.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "compare accepted a symlink output"
assert_contains "$test_root/compare-symlink.err" 'output path is an existing symlink'
cmp -s "$external_sentinel" "$test_root/external-sentinel-before.txt" || fail "compare modified an external symlink target"

python3 - "$script_dir/dependency_footprint.py" "$test_root" <<'PY'
import importlib.util
import pathlib
import sys

module_path = pathlib.Path(sys.argv[1])
test_root = pathlib.Path(sys.argv[2])
spec = importlib.util.spec_from_file_location("dependency_footprint", module_path)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)

parent = test_root / "prepared-output-parent"
moved_parent = test_root / "prepared-output-parent-moved"
parent.mkdir()
output = module.prepare_output(str(parent / "report.json"))
parent.rename(moved_parent)
parent.mkdir()
try:
    module.atomic_write(output, {"unexpected": True})
except module.ToolError as exc:
    assert "output parent changed before write" in str(exc)
else:
    raise AssertionError("replaced output parent was accepted")
assert not (parent / "report.json").exists()
assert not (moved_parent / "report.json").exists()
PY

set +e
run_normalize \
    "$test_root/raw-a" \
    "$test_root/missing-output-parent/report.json" \
    2>"$test_root/missing-output-parent.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "normalize created a missing output parent"
assert_contains "$test_root/missing-output-parent.err" 'output parent does not exist'
[[ ! -e "$test_root/missing-output-parent" ]] || fail "failed output created its missing parent"

set +e
run_fake_capture "$test_root/bad-selection.json" '' $'agent-runtime\nchannel-matrix' 2>"$test_root/bad-selection.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "multiline selection output was accepted"
assert_contains "$test_root/bad-selection.err" 'expected one non-empty feature line'

set +e
run_fake_capture \
    "$test_root/subprocess-failure.json" \
    '' \
    'agent-runtime,channel-matrix' \
    'foundation' \
    2>"$test_root/subprocess-failure.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "failed Cargo subprocess was accepted"
assert_contains "$test_root/subprocess-failure.err" 'subprocess failed with status 100'

printf '%s\n' 'not a Cargo package line' > "$test_root/raw-a/foundation.tree"
set +e
run_normalize "$test_root/raw-a" "$test_root/should-not-exist.json" 2>"$test_root/malformed.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "malformed tree was accepted"
assert_contains "$test_root/malformed.err" 'malformed Cargo tree line'

for whitespace in leading trailing; do
    cp -R "$fixture_root/raw" "$test_root/raw-feature-${whitespace}"
    if [[ "$whitespace" == leading ]]; then
        replacement=$'dep-crate v2.0.0\tfeatures: __rustls,feature-a (*)'
    else
        replacement=$'dep-crate v2.0.0\tfeatures:__rustls,feature-a '
    fi
    replace_text \
        "$test_root/raw-feature-${whitespace}/foundation.tree" \
        $'dep-crate v2.0.0\tfeatures:__rustls,feature-a (*)' \
        "$replacement"
    set +e
    run_normalize \
        "$test_root/raw-feature-${whitespace}" \
        "$test_root/feature-${whitespace}.json" \
        2>"$test_root/feature-${whitespace}.err"
    status=$?
    set -e
    [[ "$status" -ne 0 ]] || fail "${whitespace} feature whitespace was accepted"
    assert_contains "$test_root/feature-${whitespace}.err" 'malformed value'
done

rm "$test_root/raw-a/channels-default.tree"
set +e
run_normalize "$test_root/raw-a" "$test_root/should-not-exist.json" 2>"$test_root/missing.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "missing capture was accepted"
assert_contains "$test_root/missing.err" 'missing capture'

cp -R "$fixture_root/raw" "$test_root/raw-duplicate"
cp "$fixture_root/raw/channels-default.tree" "$test_root/raw-duplicate/channels-default.txt"
set +e
run_normalize "$test_root/raw-duplicate" "$test_root/should-not-exist.json" 2>"$test_root/duplicate.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "duplicate capture files were accepted"
assert_contains "$test_root/duplicate.err" 'duplicate capture files'

cp -R "$fixture_root/raw" "$test_root/raw-truncated"
replace_text \
    "$test_root/raw-truncated/hardware-probe.tree" \
    $'zeroclaw v0.8.4\tfeatures:agent-runtime,probe' \
    $'other-root v0.8.4\tfeatures:agent-runtime,probe'
set +e
run_normalize "$test_root/raw-truncated" "$test_root/should-not-exist.json" 2>"$test_root/truncated.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "capture without selected root was accepted"
assert_contains "$test_root/truncated.err" 'selected root package'

cp -R "$fixture_root/raw" "$test_root/raw-forwarding"
replace_text \
    "$test_root/raw-forwarding/hardware-probe.tree" \
    $'zeroclaw-tools v0.8.4\tfeatures:probe' \
    $'zeroclaw-tools v0.8.4\tfeatures:'
set +e
run_normalize "$test_root/raw-forwarding" "$test_root/should-not-exist.json" 2>"$test_root/forwarding.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "hardware-to-tools forwarding failure was accepted"
assert_contains "$test_root/forwarding.err" 'tools-probe-enabled'

cp "$fixture_root/policy.toml" "$test_root/unknown-policy.toml"
printf '%s\n' 'unexpected = true' >>"$test_root/unknown-policy.toml"
set +e
bash "$tool" normalize \
    --policy "$test_root/unknown-policy.toml" \
    --raw-dir "$fixture_root/raw" \
    --context "$fixture_root/context.json" \
    --output "$test_root/should-not-exist.json" \
    2>"$test_root/unknown-policy.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "unknown policy field was accepted"
assert_contains "$test_root/unknown-policy.err" 'unknown field'

cp "$fixture_root/policy.toml" "$test_root/boolean-schema.toml"
replace_text "$test_root/boolean-schema.toml" 'schema_version = 1' 'schema_version = true'
set +e
bash "$tool" normalize \
    --policy "$test_root/boolean-schema.toml" \
    --raw-dir "$fixture_root/raw" \
    --context "$fixture_root/context.json" \
    --output "$test_root/should-not-exist.json" \
    2>"$test_root/boolean-policy.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "boolean policy schema was accepted"
assert_contains "$test_root/boolean-policy.err" 'expected 1'

cp "$fixture_root/policy.toml" "$test_root/malformed-edge-kinds.toml"
replace_text \
    "$test_root/malformed-edge-kinds.toml" \
    'edge_kinds = ["normal", "build"]' \
    'edge_kinds = [[], "build"]'
set +e
bash "$tool" normalize \
    --policy "$test_root/malformed-edge-kinds.toml" \
    --raw-dir "$fixture_root/raw" \
    --context "$fixture_root/context.json" \
    --output "$test_root/should-not-exist.json" \
    2>"$test_root/malformed-edge-kinds.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "malformed policy edge kinds were accepted"
assert_contains "$test_root/malformed-edge-kinds.err" 'error: policy.edge_kinds: expected exactly'
! grep -Fq 'Traceback' "$test_root/malformed-edge-kinds.err" || fail "malformed policy emitted a Python traceback"

printf '%s\n' '{"old": true}' > "$test_root/atomic.json"
printf '%s\n' 'not a Cargo package line' > "$test_root/raw-b/foundation.tree"
set +e
run_normalize "$test_root/raw-b" "$test_root/atomic.json" 2>"$test_root/atomic.err"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "atomic failure input was accepted"
cmp -s "$test_root/atomic.json" <(printf '%s\n' '{"old": true}') || fail "failed write replaced destination"

echo "dependency footprint fixture tests: pass"
