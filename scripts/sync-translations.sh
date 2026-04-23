#!/usr/bin/env bash
# Sync per-locale .po files with the English source.
#
# Usage:
#   sync-translations.sh                    # sync all locales, AI-fill delta
#   sync-translations.sh --locale ja        # sync one locale only
#   sync-translations.sh --force            # re-translate everything (quality pass)
#   sync-translations.sh --locale ja --force
#   sync-translations.sh --help
#
# Pipeline:
#   1. mdbook-xgettext  → docs/book/po/messages.pot   (extract English msgids)
#   2. msgmerge         → per-locale .po               (mark changed→fuzzy, add new→empty)
#   3. fill-translations.py → AI-fills delta only      (requires ANTHROPIC_API_KEY)
#      Skipped if delta == 0 OR ANTHROPIC_API_KEY is unset.
#
# Idempotent — re-running against unchanged source is a no-op (zero AI calls).
# Works identically locally and in CI.
#
# Adding a new locale: see docs/book/src/developing/building-docs.md

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOOK_DIR="$REPO_ROOT/docs/book"
PO_DIR="$BOOK_DIR/po"
POT_FILE="$PO_DIR/messages.pot"

# Defaults — LOCALES env var overrides the script default (used by CI)
DEFAULT_LOCALES="${LOCALES:-ja}"
target_locale=""
force_flag=""

usage() {
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --locale)   target_locale="$2"; shift 2 ;;
        --locale=*) target_locale="${1#--locale=}"; shift ;;
        --force)    force_flag="--force"; shift ;;
        -h|--help)  usage 0 ;;
        *)          echo "unknown arg: $1" >&2; usage 1 ;;
    esac
done

# Build locale list: explicit --locale overrides env/default
if [[ -n "$target_locale" ]]; then
    locales_to_sync="$target_locale"
else
    locales_to_sync="$DEFAULT_LOCALES"
fi

mkdir -p "$PO_DIR"

# ── Step 1: extract English msgids ───────────────────────────────────────────
echo "==> Extracting English msgids → $POT_FILE"
(cd "$BOOK_DIR" && MDBOOK_OUTPUT__XGETTEXT__POT_FILE="messages.pot" mdbook build -d po-extract >/dev/null)
if [[ -f "$BOOK_DIR/po-extract/xgettext/messages.pot" ]]; then
    mv "$BOOK_DIR/po-extract/xgettext/messages.pot" "$POT_FILE"
fi
rm -rf "$BOOK_DIR/po-extract"

if [[ ! -f "$POT_FILE" ]]; then
    echo "error: messages.pot not generated — is mdbook-i18n-helpers installed?" >&2
    echo "  cargo install mdbook-i18n-helpers --locked" >&2
    exit 1
fi

# ── Step 2 + 3: per-locale merge + AI fill ───────────────────────────────────

# Count delta without bc (just sum numbers with Python, available everywhere)
count_delta() {
    local po_file="$1"
    LANG=C msgfmt --statistics "$po_file" -o /dev/null 2>&1 \
        | grep -oE '[0-9]+ (untranslated|fuzzy) message' \
        | grep -oE '^[0-9]+' \
        | python3 -c 'import sys; print(sum(int(l) for l in sys.stdin))' 2>/dev/null \
        || echo 0
}

for locale in $locales_to_sync; do
    [[ "$locale" == "en" ]] && continue   # English is the source

    po_file="$PO_DIR/$locale.po"

    if [[ ! -f "$po_file" ]]; then
        echo "==> $locale: bootstrapping new .po from template"
        msginit --no-translator --locale="$locale" --input="$POT_FILE" --output="$po_file"
    else
        echo "==> $locale: msgmerge"
        msgmerge --update --backup=none --no-fuzzy-matching "$po_file" "$POT_FILE"
    fi

    if [[ -n "$force_flag" ]]; then
        # Force mode: translate everything regardless of delta
        if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
            echo "==> $locale: --force: re-translating all entries"
            cargo run --release -q --manifest-path "$REPO_ROOT/tools/fill-translations/Cargo.toml" -- \
                --po "$po_file" --locale "$locale" --force
        else
            echo "==> $locale: --force requested but ANTHROPIC_API_KEY not set — skipping AI step"
        fi
    else
        delta=$(count_delta "$po_file")
        if [[ "$delta" -gt 0 ]]; then
            if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
                echo "==> $locale: AI-filling $delta entries"
                cargo run --release -q --manifest-path "$REPO_ROOT/tools/fill-translations/Cargo.toml" -- \
                    --po "$po_file" --locale "$locale"
            else
                echo "==> $locale: $delta entries need translation (set ANTHROPIC_API_KEY to auto-fill)"
            fi
        else
            echo "==> $locale: up to date, skipping AI step"
        fi
    fi
done

echo ""
echo "==> Translation summary:"
for locale in $locales_to_sync; do
    [[ "$locale" == "en" ]] && continue
    po_file="$PO_DIR/$locale.po"
    [[ -f "$po_file" ]] || continue
    printf "    %-8s " "$locale"
    LANG=C msgfmt --statistics "$po_file" -o /dev/null 2>&1
done
