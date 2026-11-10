#!/usr/bin/env bash

# Builds the plugin packages that scripts/ci/plugin_artifact_smoke.sh installs.
#
# Usage: build_plugin_smoke_packages.sh <output-dir>
#
# Needs a Rust toolchain with the wasm32-wasip2 target. The smoke does not: it
# needs only these packages and the binary under test, so the packages are
# built once and reused for every target.
#
# Output:
#   tool-fixture/                 the in-tree tool component, unsigned, no payload digest
#   tool-fixture-no-config-read/  the same component and name, requesting no permission
#   tool-fixture-signed/          the same component, digest recorded, signed
#   tool-fixture-tampered/        the signed package with its manifest edited afterwards
#   tool-fixture-no-digest/       signed without a payload digest
#   digest-mismatch/              unsigned, with a digest of other bytes
#   not-a-component/              a payload that is not WebAssembly
#   wrong-world/                  the in-tree channel component declared as a tool
#   publisher-key.hex             public key that signed the packages above
#   untrusted-key.hex             public key that signed nothing
#
# The signing keys are generated for one run and never leave the scratch
# directory, which is removed on exit.

set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <output-dir>" >&2
    exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
mkdir -p "$1"
out="$(cd "$1" && pwd)"
cd "$repo_root"

python=""
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1; then
        python="$candidate"
        break
    fi
done
if [ -z "$python" ]; then
    echo "FATAL: python3 is required to locate the Cargo target directory." >&2
    exit 2
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/plugin-smoke-packages.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# Release-profile components: a debug component is an order of magnitude
# larger, and the host compiles the component again on every tool call.
cargo build --locked --release \
    --package zeroclaw-tool-plugin-fixture \
    --package zeroclaw-channel-plugin-fixture \
    --target wasm32-wasip2
cargo build --locked --package zeroclaw-plugins --example sign_manifest

target_dir="$(cargo metadata --locked --format-version 1 --no-deps \
    | "$python" -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])' \
    | tr -d '\r')"
tool_component="$target_dir/wasm32-wasip2/release/zeroclaw_tool_plugin_fixture.wasm"
channel_component="$target_dir/wasm32-wasip2/release/zeroclaw_channel_plugin_fixture.wasm"
signer="$target_dir/debug/examples/sign_manifest"
if [ -f "$signer.exe" ]; then
    signer="$signer.exe"
fi
manifest="$repo_root/crates/zeroclaw-plugins/tests/fixtures/tool-fixture/plugin-manifest.toml"

for required in "$tool_component" "$channel_component" "$signer" "$manifest"; do
    if [ ! -f "$required" ]; then
        echo "FATAL: expected build output is missing: $required" >&2
        exit 1
    fi
done

# package <directory> <manifest-file> <payload-file> <payload-name>
package() {
    rm -rf "${out:?}/$1"
    mkdir -p "$out/$1"
    cp "$2" "$out/$1/manifest.toml"
    cp "$3" "$out/$1/$4"
}

# The fixture manifest without its comments, for the packages that change its
# name or permissions and are no longer the fixture the comments describe.
bare_manifest() {
    grep -v '^#' "$manifest"
}

# renamed <name> prints the fixture manifest for a package called <name>.
renamed() {
    bare_manifest | sed -e "s/^name = .*/name = \"$1\"/" \
        -e "s/^wasm_path = .*/wasm_path = \"$1.wasm\"/"
}

# differs <original> <derived> <what>: a derived manifest that came out equal
# to its original would turn a refusal check into a check of nothing.
differs() {
    if cmp -s "$1" "$2"; then
        echo "FATAL: the $3 manifest is identical to the one it was derived from." >&2
        exit 1
    fi
}

# The signer's publisher-facing behavior is checked here on the way through:
# keygen creates the key's directory and refuses an existing key file, the key
# is readable by its owner only, sign creates the output directory, and sign
# warns when the manifest carries no payload digest.
"$signer" keygen "$scratch/keys/publisher.pk8" >"$out/publisher-key.hex"
"$signer" keygen "$scratch/keys/untrusted.pk8" >"$out/untrusted-key.hex"
differs "$out/publisher-key.hex" "$out/untrusted-key.hex" "untrusted key"
if "$signer" keygen "$scratch/keys/publisher.pk8" >/dev/null 2>&1; then
    echo "FATAL: keygen overwrote an existing private key." >&2
    exit 1
fi
if [ "$(uname -s)" != "Windows_NT" ] && ! uname -s | grep -qiE 'mingw|msys|cygwin'; then
    key_mode="$("$python" -c 'import os, stat, sys
print(oct(stat.S_IMODE(os.stat(sys.argv[1]).st_mode)), oct(stat.S_IMODE(os.stat(sys.argv[2]).st_mode)))' \
        "$scratch/keys/publisher.pk8" "$scratch/keys")"
    if [ "$key_mode" != "0o600 0o700" ]; then
        echo "FATAL: private key or its directory is readable by others: $key_mode" >&2
        exit 1
    fi
fi

package tool-fixture "$manifest" "$tool_component" tool-fixture.wasm

bare_manifest | awk '
    /^\[config_schema/ { exit }
    /^permissions = / { print "permissions = []"; next }
    { print }' >"$scratch/no-config-read.toml"
differs "$manifest" "$scratch/no-config-read.toml" "permission-free"
if grep -q -e 'config_read' -e 'config_schema' "$scratch/no-config-read.toml"; then
    echo "FATAL: the permission-free manifest still requests its configuration." >&2
    exit 1
fi
package tool-fixture-no-config-read "$scratch/no-config-read.toml" \
    "$tool_component" tool-fixture.wasm

signed="$scratch/signed/manifest.toml"
"$signer" sign "$manifest" "$signed" \
    --key "$scratch/keys/publisher.pk8" --payload "$tool_component" >/dev/null
package tool-fixture-signed "$signed" "$tool_component" tool-fixture.wasm

sed -e 's/^version = "0.0.0"$/version = "0.0.1"/' "$signed" >"$scratch/tampered.toml"
differs "$signed" "$scratch/tampered.toml" "tampered"
package tool-fixture-tampered "$scratch/tampered.toml" "$tool_component" tool-fixture.wasm

# Strict mode refuses this package, which is what it is for, and the signer
# has to say so.
if ! "$signer" sign "$manifest" "$scratch/no-digest.toml" \
    --key "$scratch/keys/publisher.pk8" >/dev/null 2>"$scratch/no-digest.warning"; then
    echo "FATAL: signing without a payload digest failed." >&2
    cat "$scratch/no-digest.warning" >&2
    exit 1
fi
if ! grep -q 'strict signature mode' "$scratch/no-digest.warning"; then
    echo "FATAL: the signer did not warn that strict mode refuses a digest-free manifest." >&2
    exit 1
fi
if grep -q '^wasm_sha256 = ' "$scratch/no-digest.toml"; then
    echo "FATAL: the digest-free manifest carries a payload digest." >&2
    exit 1
fi
package tool-fixture-no-digest "$scratch/no-digest.toml" "$tool_component" tool-fixture.wasm

wrong_digest="0000000000000000000000000000000000000000000000000000000000000000"
awk -v digest="$wrong_digest" \
    '{ print } /^wasm_path = / { print "wasm_sha256 = \"" digest "\"" }' \
    "$manifest" >"$scratch/digest-mismatch.toml"
differs "$manifest" "$scratch/digest-mismatch.toml" "digest-mismatch"
package digest-mismatch "$scratch/digest-mismatch.toml" "$tool_component" tool-fixture.wasm

renamed broken-fixture >"$scratch/broken.toml"
printf 'not a wasm component' >"$scratch/broken.wasm"
package not-a-component "$scratch/broken.toml" "$scratch/broken.wasm" broken-fixture.wasm

renamed wrong-world >"$scratch/wrong-world.toml"
package wrong-world "$scratch/wrong-world.toml" "$channel_component" wrong-world.wasm

echo "Plugin smoke packages written to $out"
