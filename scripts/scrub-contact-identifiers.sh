#!/usr/bin/env bash
# Replace real contact identifiers with reserved-for-fiction ones across the
# whole local layer, history included.
#
# WHY
#     Tests and doc comments written while debugging a live deployment picked up
#     the actual phone number, LID and session key of the person the agent talks
#     to. This repo is a fork of a public project and the layer is about to be
#     pushed; a rewrite that only touched the working tree would leave the real
#     values sitting in every intermediate commit, one `git log -p` away.
#
#     Safe to rewrite history here because none of these commits have been
#     pushed anywhere — verified with `git branch -r --contains` before running.
#     If that ever stops being true, this becomes a force-push over shared
#     history and needs a different conversation.
#
# WHAT IT DOES NOT DO
#     It does not touch upstream commits: the rewrite is scoped to v0.8.3..HEAD,
#     our layer. Upstream's own fixtures already use the 555 range.
set -euo pipefail

readonly REPO="${1:-$HOME/projects/zeroclaw}"
readonly BASE="${SCRUB_BASE:-v0.8.3}"

cd "$REPO"

# Reserved-for-fiction replacements, matching the ranges upstream fixtures
# already use (+1555…, 5551234567) so the values look native to the codebase.
#
# Longest patterns first: the phone number appears both bare and with a country
# prefix, and replacing the short form first would corrupt the long one.
declare -a SUBS=(
  's/5215551234567/5215551234567/g'   # agent's own number -> fiction
  's/5215557654321/5215557654321/g'   # contact, 521-prefixed
  's/5557654321/5557654321/g'         # contact, bare
  's/76188559093817/76188559093817/g' # contact LID -> LID already in upstream fixtures
)

echo "==> verifying nothing in range has been pushed"
for c in $(git rev-list "$BASE..HEAD"); do
  if [ -n "$(git branch -r --contains "$c" 2>/dev/null)" ]; then
    echo "error: $c is already on a remote — rewriting would force-push shared history" >&2
    exit 1
  fi
done
echo "    ok: no commit in $BASE..HEAD exists on any remote"

before="$(git log "$BASE..HEAD" -p --no-merges | grep -cE '5215557654321|76188559093817|5557654321|5215551234567' || true)"
echo "==> $before occurrences across the layer before rewrite"

sed_script="$(printf '%s;' "${SUBS[@]}")"

echo "==> rewriting $BASE..HEAD"
# --tree-filter runs in a checkout of each commit, so this rewrites file
# CONTENT, not just the diff. Restricted to text sources: a blanket find would
# corrupt binaries and waste time on target/.
FILTER_BRANCH_SQUELCH_WARNING=1 git filter-branch -f \
  --tree-filter "
    find . -type f \\( -name '*.rs' -o -name '*.md' -o -name '*.yml' -o -name '*.sh' -o -name '*.toml' \\) \
      -not -path './.git/*' -not -path './target/*' \
      -exec sed -i '$sed_script' {} + 2>/dev/null || true
  " \
  --tag-name-filter cat -- "$BASE..HEAD"

after="$(git log "$BASE..HEAD" -p --no-merges | grep -cE '5215557654321|76188559093817|5557654321|5215551234567' || true)"
echo "==> $after occurrences after rewrite"

if [ "$after" != "0" ]; then
  echo "error: $after real identifiers survived the rewrite" >&2
  exit 1
fi

echo "==> clean. Verify the tree still builds before pushing:"
echo "    cargo test -p zeroclaw-channels --features whatsapp-web"
