#!/usr/bin/env python3
"""
OpenClaw → ZeroClaw Configuration Converter

Reads an OpenClaw config (openclaw.json) and produces a ZeroClaw config (config.toml).
Handles provider mapping, channel config, memory settings, and gateway settings.

Usage:
    python convert-openclaw-config.py ~/.openclaw/openclaw.json
    python convert-openclaw-config.py ~/.openclaw/openclaw.json -o /path/to/output/config.toml
    python convert-openclaw-config.py ~/.openclaw/openclaw.json --dry-run
"""

import json
import sys
import argparse
from pathlib import Path
from datetime import datetime


# ── Provider Mapping ─────────────────────────────────────────────────────────

# OpenClaw uses "provider/model" format (e.g., "anthropic/claude-opus-4-6")
# ZeroClaw splits these into default_provider + default_model
PROVIDER_MAP = {
    "anthropic": "anthropic",
    "openai": "openai",
    "openrouter": "openrouter",
    "google": "google",
    "groq": "groq",
    "bedrock": "bedrock",
    "azure": "azure",
    "ollama": "ollama",
    "mistral": "mistral",
    "deepseek": "deepseek",
    "xai": "xai",
    "together": "together",
    "perplexity": "perplexity",
    "fireworks": "fireworks",
}

# OpenClaw model names → ZeroClaw model names (when they differ)
MODEL_MAP = {
    # Most models use the same name, but some differ
    "claude-opus-4-6": "claude-opus-4-6",
    "claude-sonnet-4-6": "claude-sonnet-4-6",
    "claude-sonnet-4-5": "claude-sonnet-4-5-20250929",
    "claude-opus-4-5": "claude-opus-4-5-20251101",
    "gpt-4o": "gpt-4o",
    "gpt-4-turbo": "gpt-4-turbo",
    "gpt-4o-mini": "gpt-4o-mini",
}

# Channel name mapping (OpenClaw → ZeroClaw config section)
CHANNEL_MAP = {
    "whatsapp": "whatsapp",
    "telegram": "telegram",
    "discord": "discord",
    "slack": "slack",
    "signal": None,          # Not natively supported in ZeroClaw
    "imessage": None,        # Not natively supported in ZeroClaw
    "googlechat": None,      # Not natively supported in ZeroClaw
    "msteams": None,         # Not natively supported in ZeroClaw
    "matrix": "matrix",
    "webchat": None,         # Handled differently — use /api/chat
    "bluebubbles": None,     # Not natively supported in ZeroClaw
    "zalo": None,            # Not natively supported in ZeroClaw
    "lark": "lark",
    "feishu": "feishu",
    "nextcloud-talk": "nextcloud_talk",
    "linq": "linq",
}


def escape_toml_string(value: str) -> str:
    """Escape special characters for TOML basic strings."""
    value = value.replace("\\", "\\\\")
    value = value.replace('"', '\\"')
    value = value.replace("\t", "\\t")
    value = value.replace("\r", "\\r")
    # Newlines within single-line strings need escaping;
    # multiline strings are handled separately with triple-quotes.
    return value


def load_openclaw_config(path: str) -> dict:
    """Load and parse OpenClaw's openclaw.json."""
    with open(path, "r") as f:
        content = f.read()
    try:
        return json.loads(content)
    except json.JSONDecodeError as e:
        print(f"Error: Failed to parse JSON from {path}: {e}", file=sys.stderr)
        print("  Hint: Make sure the file is valid JSON (not TOML, YAML, etc.)", file=sys.stderr)
        sys.exit(1)


def parse_model_string(model_str: str) -> tuple[str, str]:
    """
    Parse OpenClaw's 'provider/model' format.
    Returns (provider, model).

    Examples:
        "anthropic/claude-opus-4-6" → ("anthropic", "claude-opus-4-6")
        "openai/gpt-4o" → ("openai", "gpt-4o")
        "claude-opus-4-6" → ("openrouter", "claude-opus-4-6")  # no provider = openrouter
    """
    if "/" in model_str:
        parts = model_str.split("/", 1)
        provider = parts[0].lower()
        model = parts[1]
    else:
        provider = "openrouter"
        model = model_str

    # Map model name if needed
    model = MODEL_MAP.get(model, model)

    # Map provider name
    provider = PROVIDER_MAP.get(provider, provider)

    return provider, model


def convert_gateway(oc: dict) -> dict:
    """Convert gateway settings."""
    gw = oc.get("gateway", {})
    result = {}

    # Port mapping: OpenClaw default 18789, ZeroClaw default 42617
    if "port" in gw:
        result["port"] = gw["port"]
    else:
        result["port"] = 42617  # ZeroClaw default

    # Host/bind
    if "bind" in gw:
        result["host"] = gw["bind"]
    else:
        result["host"] = "127.0.0.1"

    # Pairing
    auth = gw.get("auth", {})
    if auth.get("mode") in ("password", "token"):
        result["require_pairing"] = True
    else:
        result["require_pairing"] = False

    return result


def convert_memory(oc: dict) -> dict:
    """Convert memory/persistence settings."""
    result = {
        "backend": "sqlite",  # ZeroClaw default, best match for OpenClaw behavior
        "auto_save": True,
        "embedding_provider": "openai",
        "embedding_model": "text-embedding-3-small",
        "vector_weight": 0.7,
        "keyword_weight": 0.3,
        "min_relevance_score": 0.4,
        "response_cache_enabled": True,
        "snapshot_enabled": True,
    }

    # Check if OpenClaw had memory/persistence settings
    agent = oc.get("agent", {})
    if agent.get("memory") is False or agent.get("memory", {}).get("enabled") is False:
        result["backend"] = "none"
        result["auto_save"] = False

    return result


def convert_channels(oc: dict) -> tuple[dict, list[str]]:
    """Convert channel configurations."""
    channels = {}
    unsupported = []

    for channel_name, channel_conf in oc.items():
        if not isinstance(channel_conf, dict):
            continue
        if channel_name not in CHANNEL_MAP:
            continue

        zc_name = CHANNEL_MAP[channel_name]
        if zc_name is None:
            unsupported.append(channel_name)
            continue

        channels[zc_name] = channel_conf

    return channels, unsupported


def convert_agents(oc: dict) -> dict:
    """Convert multi-agent configurations."""
    agents = {}
    oc_agents = oc.get("agents", {})

    for name, conf in oc_agents.items():
        if name == "defaults":
            continue
        if not isinstance(conf, dict):
            continue

        agent = {}
        if "model" in conf:
            provider, model = parse_model_string(conf["model"])
            agent["provider"] = provider
            agent["model"] = model

        if "systemPrompt" in conf:
            agent["system_prompt"] = conf["systemPrompt"]
        elif "system_prompt" in conf:
            agent["system_prompt"] = conf["system_prompt"]

        if "temperature" in conf:
            agent["temperature"] = conf["temperature"]

        if "tools" in conf:
            agent["allowed_tools"] = conf["tools"]

        agent["agentic"] = conf.get("agentic", False)
        agent["max_depth"] = conf.get("maxDepth", conf.get("max_depth", 3))

        agents[name] = agent

    return agents


def build_toml(oc: dict) -> str:
    """Build ZeroClaw config.toml from parsed OpenClaw config."""
    lines = []
    lines.append("# ZeroClaw configuration")
    lines.append(f"# Converted from OpenClaw on {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    lines.append("# Review all values before deploying — some settings may need manual adjustment.")
    lines.append("")

    # ── Core provider settings ───────────────────────────────────────────
    agent = oc.get("agent", {})
    model_str = agent.get("model", "anthropic/claude-sonnet-4-6")
    provider, model = parse_model_string(model_str)

    lines.append(f'default_provider = "{provider}"')
    lines.append(f'default_model = "{model}"')

    temp = agent.get("temperature", 0.7)
    lines.append(f"default_temperature = {temp}")
    lines.append("")

    # API key (leave as placeholder)
    lines.append("# API key — set via ZEROCLAW_API_KEY env var or uncomment below:")
    lines.append('# api_key = "sk-..."')
    lines.append("")

    # ── Gateway ──────────────────────────────────────────────────────────
    gw = convert_gateway(oc)
    lines.append("[gateway]")
    lines.append(f'host = "{gw["host"]}"')
    lines.append(f"port = {gw['port']}")
    lines.append(f"require_pairing = {str(gw['require_pairing']).lower()}")
    lines.append("webhook_rate_limit_per_minute = 60")
    lines.append("")

    # ── Memory ───────────────────────────────────────────────────────────
    mem = convert_memory(oc)
    lines.append("[memory]")
    lines.append(f'backend = "{mem["backend"]}"')
    lines.append(f"auto_save = {str(mem['auto_save']).lower()}")
    lines.append(f'embedding_provider = "{mem["embedding_provider"]}"')
    lines.append(f'embedding_model = "{mem["embedding_model"]}"')
    lines.append(f"vector_weight = {mem['vector_weight']}")
    lines.append(f"keyword_weight = {mem['keyword_weight']}")
    lines.append(f"min_relevance_score = {mem['min_relevance_score']}")
    lines.append(f"response_cache_enabled = {str(mem['response_cache_enabled']).lower()}")
    lines.append(f"snapshot_enabled = {str(mem['snapshot_enabled']).lower()}")
    lines.append("")

    # ── Agent ────────────────────────────────────────────────────────────
    lines.append("[agent]")
    lines.append(f"max_tool_iterations = {agent.get('maxToolIterations', agent.get('max_tool_iterations', 20))}")
    lines.append(f"max_history_messages = {agent.get('maxHistoryMessages', agent.get('max_history_messages', 50))}")
    lines.append("compact_context = false")
    lines.append("parallel_tools = false")
    lines.append("")

    # ── Autonomy / Security ──────────────────────────────────────────────
    lines.append("[autonomy]")
    lines.append('level = "supervised"  # read_only | supervised | full')
    lines.append("workspace_only = true")
    lines.append("")

    # ── Runtime ──────────────────────────────────────────────────────────
    lines.append("[runtime]")
    docker = oc.get("docker", oc.get("runtime", {}))
    if isinstance(docker, dict) and docker.get("enabled", False):
        lines.append('kind = "docker"')
    else:
        lines.append('kind = "native"')
    lines.append("")

    # ── Observability ────────────────────────────────────────────────────
    lines.append("[observability]")
    lines.append('backend = "log"  # none | log | prometheus | otel')
    lines.append("")

    # ── Delegate Agents ──────────────────────────────────────────────────
    agents = convert_agents(oc)
    if agents:
        for name, conf in agents.items():
            lines.append(f"[agents.{name}]")
            for k, v in conf.items():
                if isinstance(v, str):
                    # Escape multiline strings
                    if "\n" in v:
                        # Escape triple quotes inside the value to avoid
                        # breaking the TOML multiline basic string delimiter.
                        safe_v = v.replace('"""', '"\\"" ')
                        lines.append(f'{k} = """')
                        lines.append(safe_v)
                        lines.append('"""')
                    else:
                        lines.append(f'{k} = "{escape_toml_string(v)}"')
                elif isinstance(v, bool):
                    lines.append(f"{k} = {str(v).lower()}")
                elif isinstance(v, list):
                    items = ", ".join(f'"{escape_toml_string(str(i))}"' for i in v)
                    lines.append(f"{k} = [{items}]")
                else:
                    lines.append(f"{k} = {v}")
            lines.append("")

    # ── Channel configs (as comments — need manual setup) ────────────────
    channels, unsupported = convert_channels(oc)
    if channels or unsupported:
        lines.append("# ── Channel Configuration ──────────────────────────────────────")
        lines.append("# Channel configs require provider-specific credentials.")
        lines.append("# Uncomment and configure the channels you need.")
        lines.append("")

    if unsupported:
        lines.append("# WARNING: The following OpenClaw channels are not natively supported in ZeroClaw:")
        for ch in unsupported:
            lines.append(f"#   - {ch}")
        lines.append("# Consider using the /api/chat endpoint for these channels instead.")
        lines.append("")

    # ── Composio (if present) ────────────────────────────────────────────
    composio = oc.get("composio", {})
    if composio:
        lines.append("[composio]")
        lines.append(f"enabled = {str(composio.get('enabled', False)).lower()}")
        if composio.get("apiKey") or composio.get("api_key"):
            api_key = composio.get("apiKey", composio.get("api_key", ""))
            lines.append(f'api_key = "{escape_toml_string(api_key)}"')
        lines.append("")

    # ── Skills ───────────────────────────────────────────────────────────
    lines.append("[skills]")
    lines.append("open_skills_enabled = false  # Enable after migrating skills")
    lines.append("")

    # ── Research ─────────────────────────────────────────────────────────
    lines.append("[research]")
    lines.append("enabled = false")
    lines.append('trigger = "never"  # never | always | keywords | length | question')
    lines.append("")

    return "\n".join(lines)


def generate_migration_notes(oc: dict) -> str:
    """Generate migration notes for the user."""
    notes = []
    notes.append("=" * 70)
    notes.append("  MIGRATION NOTES")
    notes.append("=" * 70)

    # Check for unsupported channels
    _, unsupported = convert_channels(oc)
    if unsupported:
        notes.append("")
        notes.append(f"⚠  UNSUPPORTED CHANNELS: {', '.join(unsupported)}")
        notes.append("   These channels don't have native ZeroClaw integrations.")
        notes.append("   Options:")
        notes.append("     1. Use /api/chat as the backend for a custom integration")
        notes.append("     2. Use /v1/chat/completions (OpenAI compat shim) for drop-in replacement")
        notes.append("     3. Check ZeroClaw's community channels for third-party integrations")

    # Check for skills
    if oc.get("skills"):
        notes.append("")
        notes.append("⚠  SKILLS: OpenClaw skills (UV Python scripts) are not directly compatible.")
        notes.append("   ZeroClaw skills use a different format. You'll need to port them.")
        notes.append("   See: docs/skills-migration.md (if available)")

    # Check for workflows
    if oc.get("workflows"):
        notes.append("")
        notes.append("⚠  WORKFLOWS: OpenClaw workflows need to be converted to ZeroClaw's")
        notes.append("   scheduler format ([scheduler] section in config.toml).")

    # API key handling
    notes.append("")
    notes.append("🔑 API KEYS:")
    notes.append("   Set your provider API key via environment variable:")
    notes.append("     export ZEROCLAW_API_KEY='sk-...'")
    notes.append("   Or uncomment the api_key line in config.toml")

    # Pairing
    notes.append("")
    notes.append("🔐 PAIRING:")
    notes.append("   Run your ZeroClaw instance and pair using:")
    notes.append("     curl -X POST http://localhost:42617/pair \\")
    notes.append("       -H 'X-Pairing-Code: <your-code>'")

    # Endpoint changes
    notes.append("")
    notes.append("🔗 ENDPOINT CHANGES:")
    notes.append("   OpenClaw endpoint         → ZeroClaw equivalent")
    notes.append("   ─────────────────────────────────────────────────")
    notes.append("   POST /v1/chat/completions → POST /v1/chat/completions (compat shim)")
    notes.append("   (none)                    → POST /api/chat (recommended, native)")
    notes.append("   GET  /health              → GET  /health (same)")
    notes.append("   POST /pair                → POST /pair (same)")

    notes.append("")
    notes.append("=" * 70)
    return "\n".join(notes)


def main():
    parser = argparse.ArgumentParser(
        description="Convert OpenClaw config to ZeroClaw format"
    )
    parser.add_argument(
        "input",
        help="Path to openclaw.json (e.g., ~/.openclaw/openclaw.json)",
    )
    parser.add_argument(
        "-o", "--output",
        help="Output path for config.toml (default: ./config.toml)",
        default="config.toml",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print converted config to stdout without writing",
    )
    args = parser.parse_args()

    # Load input
    input_path = Path(args.input).expanduser()
    if not input_path.exists():
        print(f"Error: {input_path} not found", file=sys.stderr)
        sys.exit(1)

    oc = load_openclaw_config(str(input_path))

    # Convert
    toml_content = build_toml(oc)
    notes = generate_migration_notes(oc)

    if args.dry_run:
        print(toml_content)
        print()
        print(notes)
    else:
        output_path = Path(args.output)
        output_path.write_text(toml_content)
        print(f"✓ Written to {output_path}")
        print()
        print(notes)


if __name__ == "__main__":
    main()
