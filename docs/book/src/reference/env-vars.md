# Environment Variables

Every operator env-var override uses a single schema-mirror grammar. The tail of a `ZEROCLAW_*` env var is the dotted prop-path that `zeroclaw config set` accepts, with each `__` (double underscore) separating path segments and each single `_` either a snake-case joiner inside a field name (`api_key` → `api-key` in `set_prop`) or a literal char inside an alias key.

<div class="os-tabs-src">

#### sh

```sh
ZEROCLAW_<dotted_path_with_double_underscores>=<value>
```

</div>

## Examples

{{#env-var example}}

The mapping from env-var name to TOML path is mechanical:

{{#env-var-table}}

The `<alias>` segments above (`home`, `prod_v2`) are operator-chosen, substitute whatever names your config actually uses.

## Bootstrap (uppercase tail)

These env vars decide *where* the config file and instance data live, before any `Config` exists. They keep their UPPERCASE form so the case rule disambiguates them from the schema-mirror surface. They resolve in the order `ZEROCLAW_CONFIG_DIR` > `ZEROCLAW_DATA_DIR` > `ZEROCLAW_WORKSPACE` (deprecated):

<div class="os-tabs-src">

#### sh

```sh
ZEROCLAW_CONFIG_DIR=/etc/zeroclaw         # config-file location (takes precedence)
ZEROCLAW_DATA_DIR=/srv/zeroclaw           # instance data directory (canonical)
ZEROCLAW_WORKSPACE=/srv/zeroclaw          # DEPRECATED — alias for ZEROCLAW_DATA_DIR
```

</div>

The gateway's web-dashboard location is configured via the standard
schema-mirror form {{#env-var-name gateway.web_dist_dir}}, see
[Web dashboard (web_dist_dir)](../gateway/web-dashboard.md) for the full
setting reference.

## Persistence boundary

Values applied via `ZEROCLAW_*` env vars land on the **in-memory** `Config` at load time and are **never** persisted to disk. `zeroclaw config save` masks env-overridden paths back to their disk-or-default values before encryption. A `WARN` log line is emitted whenever a secret-typed path (e.g. an API key) is env-overridden, so audit logs make the injection visible.

## Alias grammar

Aliases (the `<alias>` segments in the examples above, `home`, `prod_v2`, `mymatrixalias`, etc.) follow these rules:

1. Lowercase ASCII letters, digits, and single underscores.
2. Must start AND end with a letter or digit (no leading or trailing underscore).
3. No `__` substring (reserved as the env-var grammar's path separator).
4. No hyphen (illegal in env-var identifiers).
5. No uppercase (would conflict with bootstrap names).
6. 1–63 characters.

`prod_v2` is a single alias token; `home__api_key` parses as two segments (alias `home`, field `api_key`). Configs with non-conforming aliases produce a load-time error naming the offending alias.

## Errors

Unresolvable `ZEROCLAW_<lowercase_*>` names (typos, paths that don't match any prop in the schema) abort startup with a hard error naming the offending env var. Env-var names without the `ZEROCLAW_` prefix are not read by this override layer.

## Visibility

The override state is surfaced wherever the config is rendered, with a 💉 indicator marking env-overridden fields:

1. **`zeroclaw config list`**: legend `💉 env-overridden  🔒 secret` printed once at the top; rows for env-overridden fields are prefixed with 💉.
2. **Web Config editor**: every `ListEntry` carries an `is_env_overridden` bool. Env-overridden field rows render the 💉 badge and a persistent warning *"Edits here won't take effect, overridden by ZEROCLAW_..."* so operators see the override without having to attempt an edit.
3. **CLI/TUI onboarding**: `prompt_field` skips env-overridden fields and prints a 💉 three-line note (the env var name, the TOML path, and a skip notice) that clears on next/back navigation. Operators don't get prompted to type a value they've already injected.
4. **Reload drift**: `GET /api/config/drift`, `GET /api/config/list`, and the reload banner exclude env-overridden paths from drift computation. Because these values live only in memory and are never written to disk, they would otherwise report as permanent drift that no config-file edit could reconcile. Excluding them keeps drift output limited to differences an operator can actually resolve by editing the stored config.
5. **Programmatic**: `Config::prop_is_env_overridden(path) -> bool` is an O(1) HashSet lookup. Hooks here for any custom render layer.

## Deriving env-var names from your config

Three mechanical steps to derive an env-var name from any TOML key:

1. **Prefix the path with `ZEROCLAW_`.** The dotted config path is the source of truth, find the field via `zeroclaw config schema`.
2. **Replace `.` with `__`** (double underscore, the path separator).
3. **Field name stays as-is** (snake_case). Aliases stay as-is. Nothing else transforms.

For example, `[providers.models.anthropic.home] api_key = "sk-..."` lives at the dotted path `providers.models.anthropic.home.api_key`. Apply the three rules and the env var is `ZEROCLAW_providers__models__anthropic__home__api_key=sk-...`. Same mechanical mapping for any field in any section.

## Bridging ecosystem-default env vars

The schema-mirror grammar is the canonical way to inject values, but `ANTHROPIC_API_KEY` / `OPENROUTER_API_KEY` / `QDRANT_URL` / etc. are still common names in `.env` files and CI configs. One-line shell expansions point a schema-mirror name at the ecosystem-default value:

{{#env-var-bridge}}

Substitute the alias name in place of `home` to match your config. For multiple aliases on the same family, repeat the line with each alias.

These lines are shell bridges into typed config, not a general rule that constructors read provider-native env vars. Runtime code should receive the resolved value from `Config` unless the integration family explicitly documents a native env bridge.

## OAuth and CLI-path fields

A handful of fields live as schema fields, reachable via the standard mapping:

1. **MiniMax OAuth refresh flow**: `[providers.models.minimax.<alias>] oauth_refresh_token = "..."` (with optional `oauth_client_id`); region selection is the typed `endpoint` enum (`cn` / `intl`). The runtime exchanges the refresh token for a short-lived access token at provider construction time.
2. **Qwen OAuth refresh flow**: `[providers.models.qwen.<alias>] oauth_refresh_token = "..."` (with optional `oauth_client_id` and `oauth_resource_url`).
3. **Gemini OAuth**: `[providers.models.gemini.<alias>] oauth_client_id` and `oauth_client_secret`; optional `oauth_project` pins a Code Assist GCP project ID.
4. **KiloCLI / Gemini CLI paths**: `[providers.models.kilocli.<alias>] binary_path` and `[providers.models.gemini_cli.<alias>] binary_path`.
5. **Transcription / TTS keys**: `[transcription].api_key`, `[providers.tts.openai.<alias>].api_key`, `[providers.tts.elevenlabs.<alias>].api_key`, `[providers.tts.google.<alias>].api_key`.
6. **Notion / WhatsApp**: `[notion].api_key`, `[channels.whatsapp.<alias>].ws_url` (test/proxy WebSocket override).
