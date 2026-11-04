#!/usr/bin/env bash

# Contract tests for plugin_artifact_smoke.sh that need no real binary.
#
# The checks that install and execute a plugin run against a real binary in
# the plugin backend job. These pin what surrounds them: argument handling,
# exit statuses, the host-free expectation, and the report.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
smoke="${script_dir}/plugin_artifact_smoke.sh"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/plugin-smoke-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# The smoke keeps its scratch directories under TMPDIR. A private one makes
# the leftover check below immune to other smoke runs on the same machine.
mkdir -p "$scratch/tmp"
export TMPDIR="$scratch/tmp"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# A stand-in for `zeroclaw`. With FAKE_PLUGIN_COMMAND=1 it lists a `plugin`
# command the way clap renders one; every `plugin` invocation is refused the
# way a binary built without the plugin host refuses it. With
# FAKE_BROKEN_VERSION=1 it cannot even report its version. Like the approval
# prompt, it reads /dev/tty whenever it can open one. With FAKE_HANG=1 a
# `plugin` invocation never returns, and with FAKE_ABORT=1 it dies by signal.
fake="$scratch/zeroclaw"
cat >"$fake" <<'EOF'
#!/usr/bin/env bash
if exec 3</dev/tty 2>/dev/null; then
    read -r _ <&3
fi
case "${1:-}" in
    --version)
        if [ "${FAKE_BROKEN_VERSION:-}" = "1" ]; then
            echo "cannot execute" >&2
            exit 126
        fi
        echo "zeroclaw 0.0.0 (fake)"
        ;;
    --help)
        echo "Commands:"
        echo "  agent         Start the AI agent loop"
        if [ "${FAKE_PLUGIN_COMMAND:-}" = "1" ]; then
            echo "  plugin        Manage WASM plugins"
        fi
        ;;
    plugin)
        if [ "${FAKE_HANG:-}" = "1" ]; then
            sleep 60 &
            echo $! >"${FAKE_HANG_PID_FILE:?}"
            wait $!
        fi
        if [ "${FAKE_ABORT:-}" = "1" ]; then
            kill -ABRT $$
        fi
        echo "error: unrecognized subcommand 'plugin'" >&2
        exit 2
        ;;
    config)
        echo "false"
        ;;
esac
EOF
chmod +x "$fake"

# A packages directory with the layout the smoke validates before it starts.
packages="$scratch/packages"
for package in tool-fixture tool-fixture-no-config-read tool-fixture-signed \
    tool-fixture-tampered tool-fixture-no-digest digest-mismatch \
    not-a-component wrong-world; do
    mkdir -p "$packages/$package"
    echo 'name = "placeholder"' >"$packages/$package/manifest.toml"
done
echo "00" >"$packages/publisher-key.hex"
echo "11" >"$packages/untrusted-key.hex"

output=""
status=0

run_smoke() {
    set +e
    output="$(bash "$smoke" "$@" 2>&1)"
    status=$?
    set -e
}

expect_status() {
    [ "$status" -eq "$1" ] || fail "$2: expected exit $1, got $status: $output"
}

expect_in_output() {
    case "$output" in
        *"$1"*) ;;
        *) fail "$2: output does not contain '$1': $output" ;;
    esac
}

expect_fatal() {
    local name="$1" message="$2"
    shift 2
    run_smoke "$@"
    expect_status 2 "$name"
    expect_in_output "FATAL: " "$name"
    expect_in_output "$message" "$name"
}

# Problems with the invocation are reported as exit 2, never as a failed check.
expect_fatal "missing binary" "--binary is required" --expect no-host
expect_fatal "binary path that does not exist" "no binary at" \
    --binary "$scratch/absent" --expect no-host
expect_fatal "unknown expectation" "--expect must be host or no-host" \
    --binary "$fake" --expect maybe
expect_fatal "missing expectation" "--expect must be host or no-host" --binary "$fake"
expect_fatal "option without a value" "--expect needs a value" --binary "$fake" --expect
expect_fatal "unknown option" "unknown argument: --verbose" \
    --binary "$fake" --verbose yes
expect_fatal "host without packages" "--expect host needs --packages" \
    --binary "$fake" --expect host
expect_fatal "packages directory that does not exist" "no packages directory at" \
    --binary "$fake" --expect host --packages "$scratch/absent"
expect_fatal "partial registry check" "the registry check needs all four --registry-* values" \
    --binary "$fake" --expect host --packages "$packages" --registry-plugin example
expect_fatal "registry check without a host" "the registry check needs --expect host" \
    --binary "$fake" --expect no-host --registry-plugin example --registry-tool example \
    --registry-arguments '{}' --registry-expect example
expect_fatal "archive that does not exist" "no archive at" \
    --binary "$fake" --expect no-host --archive "$scratch/absent.tar.gz"
expect_fatal "evidence file in a directory that does not exist" "no directory for the evidence file" \
    --binary "$fake" --expect no-host --evidence "$scratch/absent/evidence.md"
expect_fatal "evidence path that is a directory" "is a directory" \
    --binary "$fake" --expect no-host --evidence "$scratch"

rm -rf "$packages/wrong-world"
expect_fatal "incomplete packages directory" "has no wrong-world package" \
    --binary "$fake" --expect host --packages "$packages"
mkdir -p "$packages/wrong-world"
echo 'name = "placeholder"' >"$packages/wrong-world/manifest.toml"

# A binary that cannot run is a smoke that could not run, not a failed check.
FAKE_BROKEN_VERSION=1 run_smoke --binary "$fake" --expect no-host
expect_status 2 "binary that cannot report its version"
expect_in_output "--version exited 126" "binary that cannot report its version"

# A binary without the plugin command satisfies the host-free expectation.
# The archive and evidence paths are relative on purpose: the smoke works from
# a scratch directory and has to resolve them before it moves there.
caller="$scratch/caller"
mkdir -p "$caller/out"
printf 'archive bytes' >"$caller/zeroclaw.tar.gz"
archive_sha256="$(cd "$caller" && python3 -c 'import hashlib
print(hashlib.sha256(open("zeroclaw.tar.gz", "rb").read()).hexdigest())')"
summary="$scratch/summary.md"
set +e
output="$(cd "$caller" && GITHUB_STEP_SUMMARY="$summary" bash "$smoke" \
    --binary ../zeroclaw --expect no-host --label "fake target" \
    --archive zeroclaw.tar.gz --evidence out/evidence.md 2>&1)"
status=$?
set -e
expect_status 0 "host-free binary"
expect_in_output "ok: the binary has no plugin command" "host-free binary"

for report in "$caller/out/evidence.md" "$summary"; do
    [ -s "$report" ] || fail "report was not written to $report"
    for line in \
        "### Plugin artifact smoke: fake target passed" \
        "- Expectation: \`no-host\`" \
        "- Version: \`zeroclaw 0.0.0 (fake)\`" \
        "- Archive: \`zeroclaw.tar.gz\`, SHA-256 \`$archive_sha256\`" \
        "| PASS | the binary has no plugin command |"; do
        grep -Fq -- "$line" "$report" || fail "$report lacks: $line"
    done
done

# Exit status 1 is reserved for a failed check. A smoke that stops for any
# other reason, here a job summary it cannot write, reports that it could not
# run, even though its one check passed.
GITHUB_STEP_SUMMARY="$caller/out" run_smoke --binary "$fake" --expect no-host
expect_status 2 "job summary that cannot be written"
expect_in_output "ok: the binary has no plugin command" "job summary that cannot be written"
expect_in_output "before it judged every check" "job summary that cannot be written"

# Run from a terminal, the binary must not be able to reach it: the approval
# prompt would otherwise wait for a person. The fake blocks on /dev/tty when
# it can open one, so this passes only if the smoke runs it detached.
# (pty.spawn is not used: on some Python versions its copy loop never returns
# once both the terminal and standard input reach end of file.)
if python3 -c 'import pty' 2>/dev/null; then
    set +e
    output="$(python3 -c 'import os, pty, sys
pid, fd = pty.fork()
if pid == 0:
    os.execvp(sys.argv[1], sys.argv[1:])
while True:
    try:
        chunk = os.read(fd, 65536)
    except OSError:
        break
    if not chunk:
        break
    os.write(1, chunk)
_, status = os.waitpid(pid, 0)
sys.exit(os.waitstatus_to_exitcode(status) if hasattr(os, "waitstatus_to_exitcode") else status >> 8)' \
        bash "$smoke" --binary "$fake" --expect no-host 2>&1)"
    status=$?
    set -e
    expect_status 0 "host-free binary under a pseudo-terminal"
    expect_in_output "ok: the binary has no plugin command" "host-free binary under a pseudo-terminal"
fi

# A command that hangs is killed at the limit and counts as a failed check, not
# as a refusal, and nothing it started survives it.
FAKE_HANG=1 FAKE_HANG_PID_FILE="$scratch/hang.pid" PLUGIN_SMOKE_COMMAND_TIMEOUT=1 \
    run_smoke --binary "$fake" --expect no-host
expect_status 1 "hanging binary"
expect_in_output "plugin list timed out" "hanging binary"
[ -s "$scratch/hang.pid" ] || fail "hanging binary: the fake did not record its background process"
sleep 0.5
if kill -0 "$(cat "$scratch/hang.pid")" 2>/dev/null; then
    fail "hanging binary: a process the binary started outlived the time limit"
fi

# A crash is not a refusal either. The fake dies from SIGABRT, as a release
# binary does when it panics.
FAKE_ABORT=1 run_smoke --binary "$fake" --expect no-host
expect_status 1 "crashing binary"
expect_in_output "plugin list exited 134, which is not a refusal" "crashing binary"

# A binary that lists the command does not.
FAKE_PLUGIN_COMMAND=1 run_smoke --binary "$fake" --expect no-host
expect_status 1 "plugin command on a host-free target"
expect_in_output "FAIL: the binary has no plugin command: --help lists a plugin command" \
    "plugin command on a host-free target"
expect_in_output "FAILED" "plugin command on a host-free target"

# A binary without the plugin host fails the host expectation as failed
# checks, so the report still names every check that did not hold.
run_smoke --binary "$fake" --expect host --packages "$packages"
expect_status 1 "host expectation on a host-free binary"
expect_in_output "FAIL: the binary carries the plugin command" \
    "host expectation on a host-free binary"
expect_in_output "| FAIL | an approved call returns the plugin's output |" \
    "host expectation on a host-free binary"
case "$output" in
    *"| PASS |"*) fail "a host-free binary passed a host check: $output" ;;
esac

leftovers="$(find "$TMPDIR" -maxdepth 1 -name 'plugin-smoke.*' 2>/dev/null | wc -l | tr -d '[:space:]')"
[ "$leftovers" = "0" ] || fail "the smoke left $leftovers scratch directories behind"

echo "plugin artifact smoke contract tests passed"
