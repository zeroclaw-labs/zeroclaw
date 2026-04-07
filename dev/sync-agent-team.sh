#!/bin/bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE_DIR="$ROOT_DIR/dev/agent-team/workspace"
CONFIG_SNIPPET="$ROOT_DIR/dev/agent-team/config.team.toml"
ZEROCLAW_HOME="${ZEROCLAW_HOME:-$HOME/.zeroclaw}"
WORKSPACE_DIR="$ZEROCLAW_HOME/workspace"
CONFIG_PATH="$ZEROCLAW_HOME/config.toml"

if [[ ! -f "$CONFIG_PATH" ]]; then
    echo "Config not found: $CONFIG_PATH"
    echo "Run zeroclaw once or create ~/.zeroclaw/config.toml before syncing the team."
    exit 1
fi

mkdir -p "$WORKSPACE_DIR/agents" "$WORKSPACE_DIR/skills"

copy_agent_dir() {
    local name="$1"
    rm -rf "$WORKSPACE_DIR/agents/$name"
    mkdir -p "$WORKSPACE_DIR/agents/$name"
    cp "$TEMPLATE_DIR/agents/$name/agent.toml" "$WORKSPACE_DIR/agents/$name/agent.toml"
    cp "$TEMPLATE_DIR/agents/$name/IDENTITY.md" "$WORKSPACE_DIR/agents/$name/IDENTITY.md"
    cp "$TEMPLATE_DIR/agents/$name/SOUL.md" "$WORKSPACE_DIR/agents/$name/SOUL.md"
}

copy_skill() {
    local source="$1"
    local target_dir="$2"
    mkdir -p "$target_dir"
    cp "$source" "$target_dir/SKILL.md"
}

cp "$TEMPLATE_DIR/AGENTS.md" "$WORKSPACE_DIR/AGENTS.md"
cp "$TEMPLATE_DIR/IDENTITY.md" "$WORKSPACE_DIR/IDENTITY.md"
cp "$TEMPLATE_DIR/SOUL.md" "$WORKSPACE_DIR/SOUL.md"

for agent in art design dev intel ops qa; do
    copy_agent_dir "$agent"
done

copy_skill "$ROOT_DIR/.github/skills/find-skills/SKILL.md" "$WORKSPACE_DIR/skills/find-skills"
copy_skill "$ROOT_DIR/.claude/skills/github-pr/SKILL.md" "$WORKSPACE_DIR/skills/github-pr"
copy_skill "$ROOT_DIR/.claude/skills/github-issue/SKILL.md" "$WORKSPACE_DIR/skills/github-issue"
copy_skill "$ROOT_DIR/.claude/skills/github-pr-review/SKILL.md" "$WORKSPACE_DIR/skills/github-pr-review"
copy_skill "$ROOT_DIR/.claude/skills/skill-creator/SKILL.md" "$WORKSPACE_DIR/skills/skill-creator"
copy_skill "$ROOT_DIR/.claude/skills/zeroclaw/SKILL.md" "$WORKSPACE_DIR/skills/zeroclaw"

copy_skill "$ROOT_DIR/.github/skills/find-skills/SKILL.md" "$WORKSPACE_DIR/agents/design/skills/find-skills"
copy_skill "$ROOT_DIR/.github/skills/find-skills/SKILL.md" "$WORKSPACE_DIR/agents/dev/skills/find-skills"
copy_skill "$ROOT_DIR/.claude/skills/github-pr/SKILL.md" "$WORKSPACE_DIR/agents/dev/skills/github-pr"
copy_skill "$ROOT_DIR/.claude/skills/github-issue/SKILL.md" "$WORKSPACE_DIR/agents/dev/skills/github-issue"
copy_skill "$ROOT_DIR/.github/skills/find-skills/SKILL.md" "$WORKSPACE_DIR/agents/art/skills/find-skills"
copy_skill "$ROOT_DIR/.claude/skills/github-pr-review/SKILL.md" "$WORKSPACE_DIR/agents/qa/skills/github-pr-review"

python3 - "$CONFIG_PATH" "$CONFIG_SNIPPET" <<'PY'
from pathlib import Path
import re
import sys

config_path = Path(sys.argv[1])
snippet_path = Path(sys.argv[2])
config = config_path.read_text()
snippet = snippet_path.read_text().strip()
begin = "# BEGIN repo-agent-team"
end = "# END repo-agent-team"

if begin in config and end in config:
    start = config.index(begin)
    finish = config.index(end) + len(end)
    config = (config[:start].rstrip() + "\n\n" + config[finish:].lstrip())

targets = {
    "agents.design",
    "agents.dev",
    "agents.art",
    "agents.intel",
    "agents.ops",
    "agents.qa",
    "swarms.game_team",
    "swarms.game_full",
    "swarms.auto",
}

result = []
skip = False
for line in config.splitlines():
    match = re.match(r"^\[([^\]]+)\]\s*$", line)
    if match:
        skip = match.group(1) in targets
    if not skip:
        result.append(line)

config = "\n".join(result).rstrip()

config = config.rstrip() + "\n\n" + snippet + "\n"
config_path.write_text(config)
PY

echo "Synced team template to $WORKSPACE_DIR"
echo "Merged managed agent/swarm config into $CONFIG_PATH"
echo "Restart ZeroClaw gateway to load the updated team."