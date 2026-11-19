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
5. **Programmatic**: `Config::prop_is_env_overridden(path) -> bool` resolves the stored override identity, including canonical registry JSON pointers. Hooks here for any custom render layer.

## Deriving env-var names from your config

Use `ZEROCLAW_` followed by the TOML path segments separated by `__`.
Field names stay in snake_case. Each quoted registry key is **one segment**;
preserve its dots and letter case instead of replacing dots inside the key.
For example:

```toml
[providers.connections."gateway.Prod"]
kind = "custom"
uri = "https://gateway.example.com/v1"
auth = { mode = "api_key", api_key = "<API_KEY>" }

[providers.models."chat.Fast.v2"]
capability = "chat"
provider = "gateway.Prod"
model = "wire-model"
```

Set overrides through a launcher that accepts arbitrary environment names:

```sh
env 'ZEROCLAW_providers__connections__gateway.Prod__auth__api_key=<API_KEY>' \
    'ZEROCLAW_providers__models__chat.Fast.v2__options__temperature=null' \
    zeroclaw daemon
```

Omitting an option inherits it; the literal `null` clears a nullable model
option. An empty environment value removes the field. The `__` separator
cannot occur inside a key addressed through this environment grammar. The
legacy lowercase alias grammar above applies to historical schema paths;
canonical registry keys preserve their complete TOML spelling.

## Bridging ecosystem-default env vars

The schema-mirror grammar is the canonical way to inject values, but `ANTHROPIC_API_KEY` / `OPENROUTER_API_KEY` / `QDRANT_URL` / etc. are still common names in `.env` files and CI configs. One-line shell expansions point a schema-mirror name at the ecosystem-default value:

{{#env-var-bridge}}

Substitute the alias name in place of `home` to match your config. For multiple aliases on the same family, repeat the line with each alias.

These lines are shell bridges into typed config, not a general rule that constructors read provider-native env vars. Runtime code should receive the resolved value from `Config` unless the integration family explicitly documents a native env bridge.

## OAuth and CLI-path fields

A handful of fields live as schema fields, reachable via the standard mapping:

1. **MiniMax / Qwen OAuth**: connection `auth = { mode = "oauth", refresh_token = "...", client_id = "..." }`; vendor endpoint selection lives in `extra.endpoint`. Qwen's URI override lives on the connection.
2. **Gemini OAuth**: connection `auth = { mode = "oauth", client_id = "...", client_secret = "..." }`; optional `extra.project` pins a Code Assist GCP project ID.
3. **KiloCLI / Gemini CLI / Grok Build CLI process settings**: use the connection's `api.chat` object for `binary_path`, `working_directory`, `extra_args`, `env_passthrough`, and `max_acp_stdout_bytes` where supported by that driver. Authentication uses the CLI login cache by default. Grok's explicit `XAI_API_KEY` environment passthrough remains the documented native bridge; other `XAI_*` and all `GROK_*` names are rejected.
4. **ASR / TTS keys**: connection `auth.api_key`; select a capability model referencing the connection. Speech consumers do not read historical provider tables.
5. **Notion / WhatsApp**: `[notion].api_key`, `[channels.whatsapp.<alias>].ws_url` (test/proxy WebSocket override).
