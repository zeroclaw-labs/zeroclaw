#!/usr/bin/env bash
#
# Compose the release announcement text from the release notes.
#
# The release notes (CHANGELOG-next.md, published verbatim as the GitHub
# release body) carry an "## In brief" section: two short paragraphs written
# by the release author that say what the release is, in the words a reader
# needs. This script turns that section into the text the announcement
# workflows publish, appending the commit and contributor counts and a link.
#
#   release_post_text.sh --notes FILE [--tag vX.Y.Z] [--prev vA.B.C]
#       [--counts "N commits. M contributors."]
#       [--link URL | --site-feed FEED_URL --fallback-link URL]
#       [--highlights N] [--limit CHARS]
#
#   --site-feed     RSS feed of the website blog; the item whose title names
#                   the tag supplies the link, else --fallback-link is used.
#   --highlights N  append up to N bullets from "## Highlights" after the
#                   In brief text (for surfaces with room, such as Discord).
#   --limit CHARS   body limit; default 280, or 255 when a link is appended,
#                   which is how X counts a post (a link is 23 characters).
#
# Counts come from the notes' preamble ("N commits", "M contributors") when
# present, else from git for the tag range. Without "## In brief" the first
# two Highlights bullets stand in, stripped of bold labels and PR references.
# Without either the script exits 2 so the caller can use its own text.
#
# A body over the limit is cut at a word boundary with a warning; the
# curated text should be written short enough that this never triggers.

set -euo pipefail

NOTES=""
TAG=""
PREV=""
COUNTS=""
LINK=""
SITE_FEED=""
FALLBACK_LINK=""
HIGHLIGHTS=0
LIMIT=""

while [ $# -gt 0 ]; do
    case "$1" in
        --notes) NOTES="$2"; shift 2 ;;
        --tag) TAG="$2"; shift 2 ;;
        --prev) PREV="$2"; shift 2 ;;
        --counts) COUNTS="$2"; shift 2 ;;
        --link) LINK="$2"; shift 2 ;;
        --site-feed) SITE_FEED="$2"; shift 2 ;;
        --fallback-link) FALLBACK_LINK="$2"; shift 2 ;;
        --highlights) HIGHLIGHTS="$2"; shift 2 ;;
        --limit) LIMIT="$2"; shift 2 ;;
        *) echo "release_post_text.sh: unknown argument: $1" >&2; exit 64 ;;
    esac
done

if [ -z "$NOTES" ] || [ ! -f "$NOTES" ]; then
    echo "release_post_text.sh: --notes FILE is required" >&2
    exit 64
fi

# Section body between "## <name>" and the next "## " heading.
section() {
    awk -v want="$1" '
        /^## / {
            if (on) exit
            on = ($0 == "## " want)
            next
        }
        on { print }
    ' "$NOTES"
}

# Markdown to plain text: bold and italic markers, inline links to their
# text, inline code to its content, and trailing PR/issue references.
strip_markdown() {
    # shellcheck disable=SC2016  # the backticks are a sed pattern, not a command
    sed -E \
        -e 's/\*\*([^*]+)\*\*/\1/g' \
        -e 's/(^|[^*])\*([^*]+)\*/\1\2/g' \
        -e 's/\[([^]]+)\]\([^)]*\)/\1/g' \
        -e 's/`([^`]*)`/\1/g' \
        -e 's/ *\(((#[0-9]+|[A-Z]+(-[A-Za-z0-9]+)+)(, *(#[0-9]+|[A-Z]+(-[A-Za-z0-9]+)+))*)\)//g' \
        -e 's/[[:space:]]+$//'
}

# Collapse runs of blank lines and trim leading/trailing blank lines.
tidy_paragraphs() {
    awk '
        NF { if (pending && out) print ""; print; out = 1; pending = 0; next }
        { if (out) pending = 1 }
    '
}

highlight_bullets() {
    section "Highlights" | { grep -E '^[-*] ' || true; } | head -"$1" | sed -E 's/^[-*] +//' | strip_markdown
}

BODY="$(section "In brief" | strip_markdown | tidy_paragraphs)"

if [ -z "$BODY" ]; then
    # Fallback: the first two Highlights bullets, one paragraph each.
    BODY="$(highlight_bullets 2 | awk 'NR > 1 { print "" } { print }')"
fi

if [ -z "$BODY" ]; then
    echo "release_post_text.sh: no \"## In brief\" or \"## Highlights\" section in $NOTES" >&2
    exit 2
fi

if [ "$HIGHLIGHTS" -gt 0 ] 2>/dev/null; then
    BULLETS="$(highlight_bullets "$HIGHLIGHTS" | sed -E 's/^/• /')"
    if [ -n "$BULLETS" ]; then
        BODY="$(printf '%s\n\n%s' "$BODY" "$BULLETS")"
    fi
fi

# Counts: given, else the release author's numbers from the preamble, else git.
if [ -z "$COUNTS" ]; then
    COMMITS="$(head -20 "$NOTES" | grep -oE '[0-9][0-9,]* commits?' | head -1 | grep -oE '^[0-9,]+' | tr -d ',' || true)"
    CONTRIBUTORS="$(head -20 "$NOTES" | grep -oE '[0-9][0-9,]* contributors?' | head -1 | grep -oE '^[0-9,]+' | tr -d ',' || true)"
    if [ -n "$COMMITS" ] && [ -n "$CONTRIBUTORS" ]; then
        COUNTS="${COMMITS} commits. ${CONTRIBUTORS} contributors."
    elif [ -n "$TAG" ] && git rev-parse --verify "$TAG^{commit}" >/dev/null 2>&1; then
        RANGE="${PREV:+${PREV}..}${TAG}"
        COMMITS="$(git rev-list --count --no-merges "$RANGE")"
        CONTRIBUTORS="$(git log --no-merges --format='%an' "$RANGE" \
            | { grep -viE '\[bot\]$|^dependabot|^github-actions|^BrewTestBot$' || true; } \
            | sort -uf | wc -l | tr -d ' ')"
        COUNTS="${COMMITS} commits. ${CONTRIBUTORS} contributors."
    fi
fi

if [ -n "$COUNTS" ]; then
    BODY="$(printf '%s\n\n%s' "$BODY" "$COUNTS")"
fi

# Link: explicit, else the website post for this tag from the blog feed,
# else the fallback (normally the GitHub release).
if [ -z "$LINK" ] && [ -n "$SITE_FEED" ] && [ -n "$TAG" ]; then
    LINK="$(curl -fsSL --max-time 20 "$SITE_FEED" 2>/dev/null \
        | tr -d '\n' \
        | grep -oE '<item>.*</item>' \
        | sed -E 's#</item>#\n#g' \
        | { grep -F "$TAG " || true; } \
        | grep -oE '<link>[^<]+</link>' \
        | head -1 \
        | sed -E 's#</?link>##g' || true)"
fi
LINK="${LINK:-$FALLBACK_LINK}"

# Length guard and word-boundary truncation, in characters not bytes, with
# paragraph breaks preserved.
if [ -z "$LIMIT" ]; then
    LIMIT=280
    [ -n "$LINK" ] && LIMIT=255
fi
BODY="$(printf '%s' "$BODY" | LIMIT="$LIMIT" python3 -c '
import os, sys
body = sys.stdin.read()
limit = int(os.environ["LIMIT"])
if len(body) > limit:
    print(f"release_post_text.sh: body is {len(body)} characters, limit {limit}; truncating at a word boundary. Shorten the \"## In brief\" section instead.", file=sys.stderr)
    cut = body[: limit - 1]
    space = max(cut.rfind(" "), cut.rfind("\n"))
    if space > 0:
        cut = cut[:space]
    body = cut.rstrip() + "…"
sys.stdout.write(body)
')"

if [ -n "$LINK" ]; then
    printf '%s\n\n%s\n' "$BODY" "$LINK"
else
    printf '%s\n' "$BODY"
fi
