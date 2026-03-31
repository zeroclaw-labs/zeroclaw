#!/usr/bin/env bash
set -euo pipefail

# Build script for Python WASM plugins
# Compiles Python plugins to wasm32-wasip1 using extism-py and copies
# the resulting .wasm files to tests/plugins/artifacts/
#
# extism-py requires @extism.plugin_fn in the compiled source file (it does
# AST-level detection), so we generate a thin entry-point wrapper that
# imports each SDK-decorated function and re-exports it with the raw
# @extism.plugin_fn decorator.
#
# Prerequisites:
#   - extism-py   (https://github.com/extism/python-pdk/releases)
#   - wasm-merge  (from binaryen — https://github.com/WebAssembly/binaryen)
#   - wasm-opt    (from binaryen)
#
# Quick install (Linux/macOS):
#   curl -Ls https://raw.githubusercontent.com/extism/python-pdk/main/install.sh | bash

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGINS_DIR="$SCRIPT_DIR/tests/plugins"
ARTIFACTS_DIR="$PLUGINS_DIR/artifacts"
SDK_DIR="$SCRIPT_DIR/sdks/python/src"
BUILD_DIR="$SCRIPT_DIR/.build-python-plugins"

# Plugin definitions: directory | module | exported-functions (comma-separated) | output-wasm
PLUGINS=(
    "python-echo-plugin|echo_plugin|tool_echo|python_echo_plugin.wasm"
    "python-sdk-example-plugin|sdk_example_plugin|tool_greet|python_sdk_example_plugin.wasm"
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

die() { echo "ERROR: $*" >&2; exit 1; }

find_extism_py() {
    for candidate in \
        "extism-py" \
        "$HOME/.local/bin/extism-py/bin/extism-py" \
        "$HOME/.local/bin/extism-py"; do
        if [[ -f "$candidate" && -x "$candidate" ]]; then
            echo "$candidate"
            return
        elif command -v "$candidate" &>/dev/null; then
            local resolved
            resolved="$(command -v "$candidate")"
            if [[ -f "$resolved" && -x "$resolved" ]]; then
                echo "$resolved"
                return
            fi
        fi
    done
    die "extism-py not found. Install via: curl -Ls https://raw.githubusercontent.com/extism/python-pdk/main/install.sh | bash"
}

check_binaryen() {
    for tool in wasm-merge wasm-opt; do
        if ! command -v "$tool" &>/dev/null; then
            if [[ -x "$HOME/.local/bin/$tool" ]]; then
                export PATH="$HOME/.local/bin:$PATH"
            else
                die "$tool not found. Install binaryen: https://github.com/WebAssembly/binaryen/releases"
            fi
        fi
    done
}

# Generate a build entry-point that re-exports SDK-decorated functions
# with the raw @extism.plugin_fn decorator that extism-py detects.
generate_entry_point() {
    local module="$1"      # e.g. echo_plugin
    local exports="$2"     # e.g. tool_echo  (comma-separated)
    local outfile="$3"

    {
        echo "import extism"
        echo "import json"
        echo "import ${module} as _mod"
        echo ""

        IFS=',' read -ra fns <<<"$exports"
        for fn in "${fns[@]}"; do
            fn="$(echo "$fn" | xargs)"  # trim whitespace
            cat <<PYEOF
@extism.plugin_fn
def ${fn}():
    raw_input = extism.input_str()
    parsed = json.loads(raw_input) if raw_input else None
    result = _mod._orig_${fn}(parsed)
    extism.output_str(json.dumps(result))

PYEOF
        done
    } > "$outfile"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

echo "=== Building Python WASM plugins ==="

EXTISM_PY="$(find_extism_py)"
echo "Using extism-py: $EXTISM_PY"
check_binaryen

mkdir -p "$ARTIFACTS_DIR" "$BUILD_DIR"

built=0
for entry in "${PLUGINS[@]}"; do
    IFS='|' read -r dir_name module_name exports wasm_name <<<"$entry"

    plugin_dir="$PLUGINS_DIR/$dir_name"
    src="$plugin_dir/$module_name.py"
    dest="$ARTIFACTS_DIR/$wasm_name"

    [[ -d "$plugin_dir" ]] || die "Plugin directory not found: $plugin_dir"
    [[ -f "$src" ]]        || die "Plugin source not found: $src"

    echo "Building $dir_name ..."

    # Create a copy of the plugin source that exposes the raw function
    # (without the @plugin_fn decorator) so the entry-point can wrap it.
    build_plugin_dir="$BUILD_DIR/$dir_name"
    mkdir -p "$build_plugin_dir"

    # Rewrite the plugin source: rename the decorated functions to _orig_*
    # and remove the @plugin_fn decorator so extism-py doesn't see
    # conflicting imports.
    sed -E \
        -e 's/^from zeroclaw_plugin_sdk import plugin_fn$/# (build: decorator removed)/' \
        -e 's/^@plugin_fn$/# (build: decorator removed)/' \
        "$src" | \
    sed -E "s/^def ($(echo "$exports" | sed 's/,/|/g'))\(/def _orig_\1(/" \
        > "$build_plugin_dir/$module_name.py"

    # Create extism_pdk shim so SDK modules that `import extism_pdk as pdk`
    # resolve to the `extism` module provided by extism-py at build time.
    cat > "$build_plugin_dir/extism_pdk.py" <<'SHIMEOF'
from extism import *  # noqa: F401,F403
SHIMEOF

    # Generate the entry-point wrapper
    entry_point="$build_plugin_dir/_entry.py"
    generate_entry_point "$module_name" "$exports" "$entry_point"

    # Compile with extism-py
    PYTHONPATH="$build_plugin_dir:$SDK_DIR" \
        "$EXTISM_PY" "$entry_point" -o "$dest" 2>&1

    [[ -f "$dest" ]] || die "Expected artifact not found: $dest"
    echo "  $dir_name -> artifacts/$wasm_name ($(du -h "$dest" | cut -f1))"
    (( ++built ))
done

# Clean up build directory
rm -rf "$BUILD_DIR"

echo "All $built Python plugin(s) built and copied to $ARTIFACTS_DIR"
