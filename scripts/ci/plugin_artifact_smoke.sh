#!/usr/bin/env bash

# Release-artifact plugin smoke.
#
# Runs one `zeroclaw` binary through the operator's own commands and checks
# that it installs and executes a WASM tool plugin, and that the decisions
# around that execution hold. Nothing runs until the operator enables the
# plugin system, turns discovery on, and approves the tool. Signature policy,
# payload digests, and the host's WIT world are enforced at install. Values
# configured for a package that does not request them are never delivered. A
# plugin that does not load is skipped rather than fatal.
#
# Usage:
#   plugin_artifact_smoke.sh --binary <zeroclaw> --expect host --packages <dir> [options]
#   plugin_artifact_smoke.sh --binary <zeroclaw> --expect no-host [options]
#
#   --expect host     The binary must carry the plugin host. <dir> is the
#                     output of build_plugin_smoke_packages.sh.
#   --expect no-host  The binary must not carry it: `plugin` is not a command.
#
# Options:
#   --label <text>     Names this run in the report, for example a target triple.
#   --archive <file>   The archive the binary came from; its SHA-256 is reported.
#   --evidence <file>  Also write the Markdown report to this file.
#   --registry-plugin <name[@version]> --registry-tool <match>
#   --registry-arguments <json> --registry-expect <text>
#                      Also install the plugin from the plugin registry and
#                      require the tool whose name contains <match> to return
#                      <text> for <json>. <match> must not name a built-in
#                      tool. Needs network access. All four go together.
#
# Exit status: 0 when every check passed, 1 when a check failed, 2 when the
# smoke could not run.
#
# Every check uses a throwaway configuration directory and the scripted local
# provider in plugin_smoke_provider.py. The binary runs detached from the
# terminal, without the caller's ZEROCLAW_* environment, and with its
# temporary files under the scratch directory, so no check can wait for a
# person or depend on the machine's own ZeroClaw setup. The optional registry
# check is the only network access the smoke itself makes. Set
# PLUGIN_SMOKE_KEEP_WORK=1 to keep the scratch directory for inspection, and
# PLUGIN_SMOKE_COMMAND_TIMEOUT (seconds, default 300) to bound each command
# the smoke runs.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
provider="$script_dir/plugin_smoke_provider.py"

fixture_tool="config-echo"
fixture_arguments='{"text":"hello world"}'
fixture_default_output="label=unset|uppercase=false|max_len=0|keys=0|text=hello world"
fixture_configured_output="label=masked|uppercase=true|max_len=5|keys=3|text=HELLO"

fatal() {
    echo "FATAL: $*" >&2
    exit 2
}

binary=""
expect=""
packages=""
label=""
archive=""
evidence=""
registry_plugin=""
registry_tool=""
registry_arguments=""
registry_expect=""

while [ "$#" -gt 0 ]; do
    option="$1"
    [ "$#" -ge 2 ] || fatal "$option needs a value"
    value="$2"
    shift 2
    case "$option" in
        --binary) binary="$value" ;;
        --expect) expect="$value" ;;
        --packages) packages="$value" ;;
        --label) label="$value" ;;
        --archive) archive="$value" ;;
        --evidence) evidence="$value" ;;
        --registry-plugin) registry_plugin="$value" ;;
        --registry-tool) registry_tool="$value" ;;
        --registry-arguments) registry_arguments="$value" ;;
        --registry-expect) registry_expect="$value" ;;
        *) fatal "unknown argument: $option" ;;
    esac
done

# The checks run from a scratch directory, so every path is made absolute here.
absolute_path() {
    printf '%s/%s' "$(cd "$(dirname "$1")" && pwd)" "$(basename "$1")"
}

[ -n "$binary" ] || fatal "--binary is required"
[ -f "$binary" ] || fatal "no binary at $binary"
binary="$(absolute_path "$binary")"
[ -x "$binary" ] || fatal "$binary is not executable"

case "$expect" in
    host)
        [ -n "$packages" ] || fatal "--expect host needs --packages"
        [ -d "$packages" ] || fatal "no packages directory at $packages"
        packages="$(cd "$packages" && pwd)"
        for required in tool-fixture tool-fixture-no-config-read tool-fixture-signed \
            tool-fixture-tampered tool-fixture-no-digest digest-mismatch \
            not-a-component wrong-world; do
            [ -f "$packages/$required/manifest.toml" ] \
                || fatal "$packages has no $required package"
        done
        for required in publisher-key.hex untrusted-key.hex; do
            [ -s "$packages/$required" ] || fatal "$packages has no $required"
        done
        ;;
    no-host) ;;
    *) fatal "--expect must be host or no-host" ;;
esac

registry_values="$registry_plugin$registry_tool$registry_arguments$registry_expect"
if [ -n "$registry_values" ]; then
    [ "$expect" = "host" ] || fatal "the registry check needs --expect host"
    if [ -z "$registry_plugin" ] || [ -z "$registry_tool" ] \
        || [ -z "$registry_arguments" ] || [ -z "$registry_expect" ]; then
        fatal "the registry check needs all four --registry-* values"
    fi
fi

if [ -n "$archive" ]; then
    [ -f "$archive" ] || fatal "no archive at $archive"
    archive="$(absolute_path "$archive")"
fi

if [ -n "$evidence" ]; then
    [ -d "$(dirname "$evidence")" ] || fatal "no directory for the evidence file $evidence"
    [ ! -d "$evidence" ] || fatal "the evidence path $evidence is a directory"
    evidence="$(absolute_path "$evidence")"
fi

python=""
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 \
        && "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 8))' 2>/dev/null; then
        python="$candidate"
        break
    fi
done
[ -n "$python" ] || fatal "python 3.8 or newer is required"

# The binary under test may be a native Windows program run from an MSYS shell.
native_path() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -m "$1"
    else
        printf '%s' "$1"
    fi
}

# Python's text output ends lines with a carriage return on Windows.
py() {
    "$python" "$@" | tr -d '\r'
}

sha256_of() {
    py -c 'import hashlib, sys
digest = hashlib.sha256()
with open(sys.argv[1], "rb") as handle:
    for block in iter(lambda: handle.read(1 << 20), b""):
        digest.update(block)
print(digest.hexdigest())' "$1"
}

now_ms() {
    py -c 'import time; print(int(time.time() * 1000))'
}

work="$(mktemp -d "${TMPDIR:-/tmp}/plugin-smoke.XXXXXX")" \
    || fatal "could not create a scratch directory"
provider_pid=""
completed=""

# Exit status 1 is reserved for a failed check. Anything that stops the smoke
# before it has judged every check is reported as 2.
cleanup() {
    local status=$?
    trap - EXIT
    if [ -n "$provider_pid" ]; then
        : >"$work/provider.stop"
        local waited=0
        while kill -0 "$provider_pid" 2>/dev/null && [ "$waited" -lt 50 ]; do
            sleep 0.1
            waited=$((waited + 1))
        done
        kill "$provider_pid" 2>/dev/null || true
        wait "$provider_pid" 2>/dev/null || true
    fi
    if [ "${PLUGIN_SMOKE_KEEP_WORK:-}" = "1" ]; then
        echo "scratch directory kept at $work" >&2
    else
        rm -rf "$work"
    fi
    if [ -z "$completed" ] && [ "$status" -ne 0 ] && [ "$status" -ne 2 ] \
        && [ "$status" -lt 128 ]; then
        echo "FATAL: the smoke stopped with status $status before it judged every check" >&2
        status=2
    fi
    exit "$status"
}
trap cleanup EXIT

mkdir -p "$work/home" "$work/cwd" "$work/tmp"
export HOME="$work/home"
export XDG_CONFIG_HOME="$work/home/.config"
export XDG_DATA_HOME="$work/home/.local/share"
export RUST_LOG=off
export NO_COLOR=1
# Nothing of the machine's own ZeroClaw setup reaches the binary, from the
# first `--version` on. The wrapper below filters the environment again.
for variable in $(env | sed -n 's/^\(ZEROCLAW_[A-Za-z0-9_]*\)=.*/\1/p'); do
    unset "$variable"
done
cd "$work/cwd"

results=()
failures=0
case_detail=""
case_index=0
config_dir=""
plugin_entry_key=""
zc_out=""
zc_rc=0

# Runs the binary under test. Sets zc_out and zc_rc; never aborts the smoke.
#
# On POSIX the binary runs in its own session and process group, detached
# from any controlling terminal: the approval prompt reads /dev/tty when the
# smoke is run from a terminal, and a check must never wait for, or be decided
# by, a person at the keyboard. It sees no ZEROCLAW_* variable except the
# configuration directory this smoke sets, and its temporary files stay under
# the scratch directory. A command that outlives the limit is killed,
# on POSIX together with anything it started, and reported as exit 124. A
# command that dies from a signal is reported as 128 plus the signal number,
# as a shell would. On Windows only the command itself is stopped.
zc() {
    set +e
    zc_out="$(PLUGIN_SMOKE_TMPDIR="$work/tmp" "$python" -c 'import os, signal, subprocess, sys
limit = float(sys.argv[1])
env = {k: v for k, v in os.environ.items()
       if not k.startswith("ZEROCLAW_") or k == "ZEROCLAW_CONFIG_DIR"}
scratch = env.pop("PLUGIN_SMOKE_TMPDIR")
env.update(TMPDIR=scratch, TMP=scratch, TEMP=scratch)
posix = hasattr(os, "killpg")
child = subprocess.Popen(sys.argv[2:], env=env, start_new_session=posix)
def signal_group(sig):
    try:
        if posix:
            os.killpg(child.pid, sig)
        else:
            child.terminate()
    except ProcessLookupError:
        pass
def stop(signum, frame):
    signal_group(signal.SIGTERM)
for name in ("SIGINT", "SIGTERM", "SIGHUP"):
    if hasattr(signal, name):
        signal.signal(getattr(signal, name), stop)
try:
    code = child.wait(timeout=limit)
except subprocess.TimeoutExpired:
    signal_group(signal.SIGKILL if posix else signal.SIGTERM)
    child.wait()
    print(f"the command ran for more than {limit:g} seconds and was killed", file=sys.stderr)
    sys.exit(124)
sys.exit(128 - code if code < 0 else code)' \
        "${PLUGIN_SMOKE_COMMAND_TIMEOUT:-300}" "$binary" "$@" 2>&1 </dev/null)"
    zc_rc=$?
    set -e
    zc_out="$(printf '%s' "$zc_out" | tr -d '\r' | sed $'s/\x1b\\[[0-9;]*m//g')"
}

last_output_line() {
    printf '%s\n' "$zc_out" | sed -e '/^[[:space:]]*$/d' | tail -n 1 | cut -c 1-300
}

# The check functions below run as `if` conditions, where `set -e` is off, so
# each step is checked by hand: `step || return 1`.
fail() {
    case_detail="$*"
    return 1
}

expect_success() {
    [ "$zc_rc" -eq 0 ] || fail "$1 exited $zc_rc: $(last_output_line)"
}

# A refusal is a decision the binary reports: exit 1 from the command, or 2
# from argument parsing. A crash, a signal death, or a timeout is not one.
expect_refusal() {
    case "$zc_rc" in
        1 | 2) return 0 ;;
        0) fail "$1 exited 0, expected a refusal" ;;
        124) fail "$1 timed out; a refusal has to be a decision" ;;
        *) fail "$1 exited $zc_rc, which is not a refusal: $(last_output_line)" ;;
    esac
}

expect_output() {
    case "$zc_out" in
        *"$1"*) return 0 ;;
    esac
    fail "output does not mention '$1': $(last_output_line)"
}

expect_installed() {
    [ -f "$config_dir/plugins/$1/manifest.toml" ] || fail "$1 is not installed"
}

expect_not_installed() {
    [ ! -e "$config_dir/plugins/$1" ] || fail "$1 was installed"
}

# A new configuration directory holding only what an agent turn needs: one
# agent, one risk profile, and the scripted provider. The locale is pinned so
# messages do not depend on the machine running the smoke.
fresh_config() {
    case_index=$((case_index + 1))
    config_dir="$work/config-$case_index"
    mkdir -p "$config_dir"
    cat >"$config_dir/config.toml" <<EOF
schema_version = 3
locale = "en"

[reliability]
provider_retries = 0
provider_backoff_ms = 0

[risk_profiles.default]

[runtime_profiles.default]

[providers.models.openai.smoke]
api_key = "plugin-smoke"
uri = "http://127.0.0.1:$provider_port"
model = "plugin-smoke"
wire_api = "chat_completions"

[agents.default]
model_provider = "openai.smoke"
risk_profile = "default"
runtime_profile = "default"
EOF
    ZEROCLAW_CONFIG_DIR="$(native_path "$config_dir")"
    export ZEROCLAW_CONFIG_DIR
}

install() {
    zc plugin install "$(native_path "$packages/$1")"
}

configure() {
    zc config set --no-interactive "$1" "$2"
    expect_success "config set $1"
}

setting() {
    zc config get "$1"
    printf '%s' "$zc_out" | tr -d '[:space:]'
}

enable_and_discover() {
    configure plugins.enabled true || return 1
    configure plugins.auto_discover true
}

approve() {
    configure risk_profiles.default.auto_approve "[\"$1\"]"
}

strict_policy_trusting() {
    configure plugins.security.signature_mode strict || return 1
    configure plugins.security.trusted_publisher_keys "[\"$(tr -d '[:space:]' <"$packages/$1")\"]"
}

entry_key() {
    zc plugin info "$1"
    expect_success "plugin info $1" || return 1
    plugin_entry_key="$(printf '%s\n' "$zc_out" | grep -Eo 'zpi1_[A-Za-z0-9_-]+' | head -n 1)"
    [ -n "$plugin_entry_key" ] || fail "plugin info $1 printed no config entry key"
}

turn_requests=""
turn_tools=""
turn_offered=""
turn_tool=""
turn_results=""
turn_result=""
turn_ms=""

summary_value() {
    printf '%s\n' "$1" | sed -n "s/^$2=//p"
}

# One agent turn in which the scripted provider asks for the tool whose name
# contains $1, passing the JSON arguments $2.
agent_turn() {
    turn_requests=""
    turn_tools=""
    turn_offered=""
    turn_tool=""
    turn_results=""
    turn_result=""
    "$python" "$provider" script --file "$work/provider.script" \
        --tool-match "$1" --tool-arguments "$2" \
        || fail "could not script the provider" || return 1
    : >"$work/provider.log"
    local started summary
    started="$(now_ms)"
    zc agent --agent default --message "plugin smoke"
    turn_ms=$(($(now_ms) - started))
    summary="$(py "$provider" summarize --log "$work/provider.log" --tool-match "$1")" \
        || fail "could not read the provider log" || return 1
    turn_requests="$(summary_value "$summary" requests)"
    turn_tools="$(summary_value "$summary" tools_offered)"
    turn_offered="$(summary_value "$summary" offered)"
    turn_tool="$(summary_value "$summary" tool_name)"
    turn_results="$(summary_value "$summary" tool_results)"
    turn_result="$(summary_value "$summary" tool_result)"
    expect_success "the agent turn" || return 1
    # A turn that never reached the provider, or that offered it nothing, says
    # nothing about which tools the host exposes.
    [ "${turn_requests:-0}" -ge 1 ] || fail "the agent turn never reached the provider" || return 1
    [ "${turn_tools:-0}" -ge 1 ] || fail "the agent turn offered the provider no tools"
}

expect_not_offered() {
    [ "$turn_offered" = "no" ] || fail "the provider was offered $turn_tool"
}

expect_offered() {
    [ "$turn_offered" = "yes" ] || fail "the provider was never offered a '$1' tool"
}

expect_tool_result() {
    [ "$turn_results" = "1" ] \
        || fail "expected one tool result, saw ${turn_results:-none}" || return 1
    [ "$turn_result" = "$1" ] || fail "tool result was '$turn_result', expected '$1'"
}

record() {
    results+=("$1|$2|$3")
    if [ "$1" = "FAIL" ]; then
        failures=$((failures + 1))
        echo "FAIL: $2: $3" >&2
    else
        echo "ok: $2"
    fi
}

run_case() {
    local name="$1"
    shift
    case_detail=""
    if "$@"; then
        record PASS "$name" "$case_detail"
    else
        record FAIL "$name" "$case_detail"
    fi
}

# ── Checks ──────────────────────────────────────────────────────────────────

command_is_listed() {
    zc --help
    expect_success "--help" || return 1
    printf '%s\n' "$zc_out" | grep -Eq '^[[:space:]]+plugin[[:space:]]'
}

binary_carries_the_plugin_command() {
    command_is_listed || fail "--help does not list a plugin command"
}

binary_has_no_plugin_command() {
    if command_is_listed; then
        fail "--help lists a plugin command"
        return 1
    fi
    [ -z "$case_detail" ] || return 1
    fresh_config
    zc plugin list
    expect_refusal "plugin list" || return 1
    [ ! -e "$config_dir/plugins" ] || fail "plugin list created a plugins directory"
}

# The next six checks share one configuration directory and follow the
# operator's path in order: install, discover without enabling, enable without
# discovery, discover, approve, remove.
install_leaves_the_plugin_system_off() {
    fresh_config
    install tool-fixture
    expect_success "plugin install" || return 1
    expect_installed tool-fixture || return 1
    [ "$(setting plugins.enabled)" = "false" ] || fail "install set plugins.enabled" || return 1
    [ "$(setting plugins.auto_discover)" = "false" ] \
        || fail "install set plugins.auto_discover" || return 1
    zc plugin info tool-fixture
    expect_success "plugin info" || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_not_offered
}

discovery_without_enabling_offers_nothing() {
    expect_installed tool-fixture || return 1
    configure plugins.auto_discover true || return 1
    [ "$(setting plugins.enabled)" = "false" ] || fail "plugins.enabled is not false" || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_not_offered || return 1
    configure plugins.auto_discover false
}

enabling_without_discovery_offers_nothing() {
    expect_installed tool-fixture || return 1
    configure plugins.enabled true || return 1
    [ "$(setting plugins.auto_discover)" = "false" ] \
        || fail "plugins.auto_discover is not false" || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_not_offered
}

a_discovered_tool_waits_for_approval() {
    expect_installed tool-fixture || return 1
    configure plugins.auto_discover true || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_offered "$fixture_tool" || return 1
    [ "$turn_results" = "1" ] \
        || fail "expected one tool result, saw ${turn_results:-none}" || return 1
    case "$turn_result" in
        *"text="*) fail "the plugin ran without approval: $turn_result" || return 1 ;;
    esac
    # The runtime words the denial by whether an operator answered or none was
    # available; both mean the call did not run.
    case "$turn_result" in
        *"the call did not run"* | *"no operator decision was available"*) ;;
        *) fail "the tool result does not report a denied call: $turn_result" ;;
    esac
}

an_approved_call_returns_the_plugin_output() {
    expect_installed tool-fixture || return 1
    approve "$fixture_tool" || return 1
    entry_key tool-fixture || return 1
    configure "plugins.entries.$plugin_entry_key.config.label" masked || return 1
    configure "plugins.entries.$plugin_entry_key.config.uppercase" true || return 1
    configure "plugins.entries.$plugin_entry_key.config.max_len" 5 || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_tool_result "$fixture_configured_output" || return 1
    case_detail="tool call turn took ${turn_ms} ms"
}

remove_uninstalls_the_package() {
    expect_installed tool-fixture || return 1
    zc plugin remove tool-fixture
    expect_success "plugin remove" || return 1
    expect_not_installed tool-fixture || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_not_offered
}

a_payload_that_is_not_webassembly_is_refused() {
    fresh_config
    install not-a-component
    expect_refusal "plugin install" || return 1
    expect_output "failed to load WASM component" || return 1
    expect_not_installed broken-fixture
}

a_component_for_another_world_is_refused() {
    fresh_config
    install wrong-world
    expect_refusal "plugin install" || return 1
    expect_output "failed to instantiate tool plugin" || return 1
    expect_not_installed wrong-world
}

a_payload_digest_mismatch_is_refused() {
    fresh_config
    install digest-mismatch
    expect_refusal "plugin install" || return 1
    expect_output "WASM payload digest mismatch" || return 1
    expect_not_installed tool-fixture
}

a_plugin_that_does_not_load_is_skipped() {
    fresh_config
    install tool-fixture
    expect_success "plugin install" || return 1
    zc plugin install --no-verify "$(native_path "$packages/not-a-component")"
    expect_success "plugin install --no-verify" || return 1
    expect_installed broken-fixture || return 1
    zc plugin info broken-fixture
    expect_refusal "plugin info broken-fixture" || return 1
    zc plugin list --verify
    expect_success "plugin list --verify" || return 1
    printf '%s\n' "$zc_out" | grep 'broken-fixture' | grep -q 'failed to load WASM component' \
        || fail "plugin list --verify does not report why broken-fixture fails" || return 1
    enable_and_discover || return 1
    approve "$fixture_tool" || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_tool_result "$fixture_default_output"
}

# The operator configures a value for a package that reads its configuration,
# then replaces that package with one of the same name that does not ask to.
# The value stays in the operator's configuration and must not reach the new
# package. The host refuses to expose the tool at all.
configured_values_are_withheld_without_the_permission() {
    fresh_config
    install tool-fixture
    expect_success "plugin install" || return 1
    entry_key tool-fixture || return 1
    configure "plugins.entries.$plugin_entry_key.config.label" masked || return 1
    zc plugin remove tool-fixture
    expect_success "plugin remove" || return 1
    install tool-fixture-no-config-read
    expect_success "plugin install" || return 1
    expect_installed tool-fixture || return 1
    if ! grep -q "$plugin_entry_key" "$config_dir/config.toml" \
        || ! grep -q '^label = ' "$config_dir/config.toml"; then
        fail "the configured value did not outlive the first package"
        return 1
    fi
    enable_and_discover || return 1
    approve "$fixture_tool" || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_not_offered
}

strict_mode_runs_a_trusted_signed_package() {
    fresh_config
    strict_policy_trusting publisher-key.hex || return 1
    install tool-fixture-signed
    expect_success "plugin install" || return 1
    zc plugin info tool-fixture
    expect_success "plugin info" || return 1
    enable_and_discover || return 1
    approve "$fixture_tool" || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_tool_result "$fixture_default_output"
}

# strict_mode_refuses <package> <trusted-key-file> <message>
strict_mode_refuses() {
    fresh_config
    strict_policy_trusting "$2" || return 1
    install "$1"
    expect_refusal "plugin install" || return 1
    expect_output "$3" || return 1
    expect_not_installed tool-fixture
}

tightening_the_policy_drops_an_unsigned_install() {
    fresh_config
    install tool-fixture
    expect_success "plugin install" || return 1
    strict_policy_trusting publisher-key.hex || return 1
    zc plugin info tool-fixture
    expect_refusal "plugin info" || return 1
    enable_and_discover || return 1
    approve "$fixture_tool" || return 1
    agent_turn "$fixture_tool" "$fixture_arguments" || return 1
    expect_not_offered
}

the_registry_plugin_installs_and_runs() {
    local name="${registry_plugin%%@*}"
    fresh_config
    zc plugin install "$registry_plugin"
    expect_success "plugin install $registry_plugin" || return 1
    expect_installed "$name" || return 1
    zc plugin info "$name"
    expect_success "plugin info $name" || return 1
    agent_turn "$registry_tool" "$registry_arguments" || return 1
    [ "$turn_offered" = "no" ] \
        || fail "'$registry_tool' also matches the built-in tool $turn_tool" || return 1
    enable_and_discover || return 1
    agent_turn "$registry_tool" "$registry_arguments" || return 1
    expect_offered "$registry_tool" || return 1
    approve "$turn_tool" || return 1
    agent_turn "$registry_tool" "$registry_arguments" || return 1
    expect_tool_result "$registry_expect" || return 1
    case_detail="tool call turn took ${turn_ms} ms"
}

# ── Run ─────────────────────────────────────────────────────────────────────

zc --version
[ "$zc_rc" -eq 0 ] || fatal "$binary --version exited $zc_rc: $zc_out"
version="$(last_output_line)"
binary_bytes="$(wc -c <"$binary" | tr -d '[:space:]')" || fatal "could not measure $binary"
binary_sha256="$(sha256_of "$binary")" || fatal "could not hash $binary"
archive_sha256=""
if [ -n "$archive" ]; then
    archive_sha256="$(sha256_of "$archive")" || fatal "could not hash $archive"
fi

"$python" "$provider" serve \
    --port-file "$work/provider.port" \
    --log "$work/provider.log" \
    --script "$work/provider.script" \
    --stop-file "$work/provider.stop" &
provider_pid=$!
provider_port=""
attempt=0
while [ "$attempt" -lt 100 ]; do
    if [ -s "$work/provider.port" ]; then
        provider_port="$(tr -d '[:space:]' <"$work/provider.port")"
        break
    fi
    kill -0 "$provider_pid" 2>/dev/null || fatal "the scripted provider exited at startup"
    sleep 0.1
    attempt=$((attempt + 1))
done
[ -n "$provider_port" ] || fatal "the scripted provider did not publish a port"

if [ "$expect" = "no-host" ]; then
    run_case "the binary has no plugin command" binary_has_no_plugin_command
else
    run_case "the binary carries the plugin command" binary_carries_the_plugin_command
    run_case "install leaves the plugin system off" install_leaves_the_plugin_system_off
    run_case "discovery without enabling offers nothing" discovery_without_enabling_offers_nothing
    run_case "enabling without discovery offers nothing" enabling_without_discovery_offers_nothing
    run_case "a discovered tool waits for operator approval" a_discovered_tool_waits_for_approval
    run_case "an approved call returns the plugin's output" an_approved_call_returns_the_plugin_output
    run_case "remove uninstalls the package" remove_uninstalls_the_package
    run_case "a payload that is not WebAssembly is refused" a_payload_that_is_not_webassembly_is_refused
    run_case "a component for another world is refused" a_component_for_another_world_is_refused
    run_case "a payload digest mismatch is refused" a_payload_digest_mismatch_is_refused
    run_case "a plugin that does not load is skipped" a_plugin_that_does_not_load_is_skipped
    run_case "configured values are withheld from a package that does not request them" \
        configured_values_are_withheld_without_the_permission
    run_case "strict mode runs a trusted signed package" strict_mode_runs_a_trusted_signed_package
    run_case "strict mode refuses an unsigned package" \
        strict_mode_refuses tool-fixture publisher-key.hex \
        "is unsigned and signature verification is required"
    run_case "strict mode refuses an untrusted publisher" \
        strict_mode_refuses tool-fixture-signed untrusted-key.hex \
        "signed by untrusted publisher key"
    run_case "strict mode refuses a manifest edited after signing" \
        strict_mode_refuses tool-fixture-tampered publisher-key.hex \
        "signature verification failed"
    run_case "strict mode refuses a signed package without a payload digest" \
        strict_mode_refuses tool-fixture-no-digest publisher-key.hex \
        "must declare signed wasm_sha256 in strict mode"
    run_case "tightening the policy drops an unsigned install" \
        tightening_the_policy_drops_an_unsigned_install
    if [ -n "$registry_plugin" ]; then
        run_case "registry plugin $registry_plugin installs and runs" \
            the_registry_plugin_installs_and_runs
    fi
fi

report() {
    local verdict="passed"
    [ "$failures" -eq 0 ] || verdict="FAILED"
    echo "### Plugin artifact smoke: ${label:-$(basename "$binary")} $verdict"
    echo
    echo "- Expectation: \`$expect\`"
    echo "- Version: \`$version\`"
    echo "- Binary: $binary_bytes bytes, SHA-256 \`$binary_sha256\`"
    if [ -n "$archive" ]; then
        echo "- Archive: \`$(basename "$archive")\`, SHA-256 \`$archive_sha256\`"
    fi
    echo
    echo "| Result | Check | Detail |"
    echo "|---|---|---|"
    local row status rest name detail
    for row in "${results[@]}"; do
        status="${row%%|*}"
        rest="${row#*|}"
        name="${rest%%|*}"
        detail="${rest#*|}"
        echo "| $status | $name | $(printf '%s' "$detail" | tr '|' '/') |"
    done
}

rendered="$(report)"
echo
echo "$rendered"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '%s\n\n' "$rendered" >>"$GITHUB_STEP_SUMMARY"
fi
if [ -n "$evidence" ]; then
    printf '%s\n' "$rendered" >"$evidence"
fi

completed=1
[ "$failures" -eq 0 ] || exit 1
