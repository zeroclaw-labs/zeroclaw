#!/usr/bin/env bash
# Build the ZeroClaw documentation site locally.
#
# Usage:
#   scripts/docs.sh                 # serve English on http://localhost:3000 (auto-rebuild on edit)
#   scripts/docs.sh --locale ja     # serve Japanese instead
#   scripts/docs.sh build           # static build of all locales into docs/book/book/
#   scripts/docs.sh refs            # regenerate reference/cli.md + reference/config.md
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

check_tools() {
    local missing=0
    require mdbook            "https://rust-lang.github.io/mdBook/guide/installation.html (or 'cargo install mdbook --locked')" || missing=1
    require mdbook-xgettext   "cargo install mdbook-i18n-helpers --locked" || missing=1
    require mdbook-gettext    "cargo install mdbook-i18n-helpers --locked" || missing=1
    require generate-schema-doc "pipx install json-schema-for-humans" || missing=1
    require cargo             "https://rustup.rs" || missing=1
    [[ $missing -eq 0 ]]
}

build_refs() {
    echo "==> Generating reference/cli.md and reference/config.md from code"
    mkdir -p "$REF_DIR"
    (cd "$REPO_ROOT" && cargo run "${CARGO_FLAGS[@]}" -- markdown-help > "$REF_DIR/cli.md")
    (cd "$REPO_ROOT" && cargo run "${CARGO_FLAGS[@]}" -- markdown-schema > /tmp/zc-config.schema.json)
    generate-schema-doc --config template_name=md /tmp/zc-config.schema.json "$REF_DIR/config.md"
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
    check_tools
    build_refs
    build_api
    build_locales
    assemble
    echo "==> Done. Open: $BOOK_DIR/book/index.html"
}

cmd_refs() {
    check_tools
    build_refs
}

cmd_serve() {
    local locale="$1"
    check_tools
    if [[ ! -f "$REF_DIR/cli.md" ]] || [[ ! -f "$REF_DIR/config.md" ]]; then
        build_refs
    fi
    echo "==> Serving locale '$locale' at http://localhost:3000"
    (cd "$BOOK_DIR" && MDBOOK_BOOK__LANGUAGE="$locale" mdbook serve --open)
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
