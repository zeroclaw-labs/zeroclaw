# Docs & Translations

ZeroClaw has two independent translation layers:

| Layer | Format | What it covers |
|---|---|---|
| **App strings** | Mozilla Fluent (`.ftl`) | CLI help text, command descriptions, runtime messages |
| **Docs** | gettext (`.po`) | Everything in this mdBook |

They are filled separately and stored separately. Both use a configurable provider (see `[providers.models.<name>]` in `~/.zeroclaw/config.toml`; CI uses Anthropic via `ANTHROPIC_API_KEY`).

## Building the docs locally

{{#include ../developing/building-docs.md}}

## Filling app strings (Fluent)

App strings live in `crates/zeroclaw-runtime/locales/`. English is the source of truth and is embedded at compile time. Non-English locales are loaded from `~/.zeroclaw/workspace/locales/` at runtime.

Configure a provider in `config.toml` once:

```toml
[providers.models.ollama]
name = "ollama"
base_url = "http://localhost:11434"
model = "llama3.2"
```

Then:

```bash
cargo fluent stats                                      # coverage per locale
cargo fluent check                                      # validate .ftl syntax
cargo fluent fill --locale ja --provider ollama         # fill missing keys
cargo fluent fill --locale ja --provider ollama --force # retranslate everything
cargo fluent scan                                       # find stale or missing keys vs Rust source
```

After filling, copy the updated `.ftl` file to your workspace and rebuild the binary to pick up the changes:

```bash
cp crates/zeroclaw-runtime/locales/ja/cli.ftl ~/.zeroclaw/workspace/locales/ja/cli.ftl
```

## Filling doc translations (gettext)

Doc translations live in `docs/book/po/`. `cargo mdbook sync` runs extract → merge → AI-fill in one step. Without `--provider`, sync still runs extract + merge and reports how many strings need translation — partial translations fall back to English at render time.

```bash
cargo mdbook sync --provider ollama
cargo mdbook sync --provider ollama --force
```

## Adding a new locale

1. Edit `locales.toml` at the repo root — the **only** file you need to touch:
   ```toml
   [[locale]]
   code = "<code>"
   label = "Language Name"
   ```

2. Translate the app strings:
   ```bash
   cargo fluent fill --locale <code> --provider ollama
   ```

3. Bootstrap and fill the docs `.po` file:
   ```bash
   cargo mdbook sync --locale <code> --provider ollama
   ```

Everything else — `lang-switcher.js`, CI deploy — reads from `locales.toml` automatically.
