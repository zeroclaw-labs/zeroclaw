#!/usr/bin/env bash
# Build the ZeroClaw documentation site locally.
#
# Usage:
#   scripts/docs.sh                 # serve English on http://localhost:3000 (auto-rebuild on edit)
#   scripts/docs.sh --locale ja     # serve Japanese instead
#   scripts/docs.sh build           # static build of all locales into docs/book/book/
#   scripts/docs.sh refs            # regenerate cli.md, config.md, and rustdoc API reference
#   scripts/docs.sh --help

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOOK_DIR="$REPO_ROOT/docs/book"
REF_DIR="$BOOK_DIR/src/reference"
LOCALES=(en ja)
CARGO_FLAGS=(--no-default-features --features schema-export)

usage() {
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

require() {
    local cmd="$1"
    local install_hint="$2"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "error: '$cmd' not found on PATH" >&2
        echo "  install: $install_hint" >&2
        return 1
    fi
}

check_tools_serve() {
    local missing=0
    require mdbook            "cargo install mdbook --locked" || missing=1
    require mdbook-xgettext   "cargo install mdbook-i18n-helpers --locked" || missing=1
    require mdbook-gettext    "cargo install mdbook-i18n-helpers --locked" || missing=1
    require cargo             "https://rustup.rs" || missing=1
    [[ $missing -eq 0 ]]
}

check_tools_refs() {
    local missing=0
    require cargo "https://rustup.rs" || missing=1
    [[ $missing -eq 0 ]]
}

build_refs() {
    echo "==> Generating reference/cli.md and reference/config.md from code"
    mkdir -p "$REF_DIR"
    (cd "$REPO_ROOT" && cargo run "${CARGO_FLAGS[@]}" -- markdown-help) \
        | sed 's/^###### //' > "$REF_DIR/cli.md"
    (cd "$REPO_ROOT" && cargo run "${CARGO_FLAGS[@]}" -- markdown-schema > "$REF_DIR/config.md")
}

build_api() {
    echo "==> Generating rustdoc API reference"
    (cd "$REPO_ROOT" && cargo doc --no-deps --workspace --exclude zeroclaw-desktop)
}

build_locales() {
    echo "==> Building mdBook for locales: ${LOCALES[*]}"
    for locale in "${LOCALES[@]}"; do
        (cd "$BOOK_DIR" && MDBOOK_BOOK__LANGUAGE="$locale" mdbook build -d "book/$locale")
    done
}

assemble() {
    echo "==> Assembling site (rustdoc + locale redirect)"
    rm -rf "$BOOK_DIR/book/api"
    cp -r "$REPO_ROOT/target/doc" "$BOOK_DIR/book/api"
    cat > "$BOOK_DIR/book/index.html" <<'HTML'
<!doctype html>
<meta charset="utf-8">
<meta http-equiv="refresh" content="0; url=./en/">
<link rel="canonical" href="./en/">
<title>ZeroClaw Docs</title>
HTML
}

cmd_build() {
    check_tools_serve
    check_tools_refs
    build_refs
    build_api
    build_locales
    assemble
    echo "==> Done. Open: $BOOK_DIR/book/index.html"
}

cmd_refs() {
    check_tools_refs
    build_refs
    build_api
    mkdir -p "$BOOK_DIR/book"
    rm -rf "$BOOK_DIR/book/api"
    cp -r "$REPO_ROOT/target/doc" "$BOOK_DIR/book/api"
    echo "==> API reference: $BOOK_DIR/book/api/index.html"
}

cmd_serve() {
    local locale="$1"
    local port=3000
    check_tools_serve

    if [[ ! -f "$REF_DIR/cli.md" ]] || [[ ! -f "$REF_DIR/config.md" ]]; then
        build_refs
    fi
    if [[ ! -d "$REPO_ROOT/target/doc" ]]; then
        build_api
    fi

    local out_dir="$BOOK_DIR/book"

    # Initial build into output dir.
    (cd "$BOOK_DIR" && MDBOOK_BOOK__LANGUAGE="$locale" mdbook build -d "book")
    cp -r "$REPO_ROOT/target/doc" "$out_dir/api"

    # Watch for source changes and rebuild (no built-in HTTP server).
    (cd "$BOOK_DIR" && MDBOOK_BOOK__LANGUAGE="$locale" mdbook watch -d "book" >/dev/null 2>&1) &
    local watch_pid=$!

    # Re-copy api whenever mdbook's clean-on-rebuild removes it.
    (
        while kill -0 "$watch_pid" 2>/dev/null; do
            sleep 1
            if [[ -d "$out_dir" && ! -d "$out_dir/api" && -d "$REPO_ROOT/target/doc" ]]; then
                cp -r "$REPO_ROOT/target/doc" "$out_dir/api"
            fi
        done
    ) &
    local sync_pid=$!

    echo "==> Serving locale '$locale' at http://localhost:$port"
    echo "    API reference: http://localhost:$port/api/index.html"
    echo "    (live-reload active — edit docs/book/src/ to trigger rebuild)"

    { command -v xdg-open &>/dev/null && xdg-open "http://localhost:$port"; } || \
    { command -v open     &>/dev/null && open     "http://localhost:$port"; } || true

    # shellcheck disable=SC2064
    trap "kill $watch_pid $sync_pid 2>/dev/null; exit" INT TERM EXIT
    python3 -m http.server "$port" --directory "$out_dir"
}

# Argument parsing
mode="serve"
locale="en"
while [[ $# -gt 0 ]]; do
    case "$1" in
        build|refs|serve) mode="$1"; shift ;;
        --locale) locale="$2"; shift 2 ;;
        --locale=*) locale="${1#--locale=}"; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown arg: $1" >&2; usage 1 ;;
    esac
done

case "$mode" in
    build) cmd_build ;;
    refs)  cmd_refs ;;
    serve) cmd_serve "$locale" ;;
esac
