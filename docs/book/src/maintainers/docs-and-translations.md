# Docs & Translations

ZeroClaw has two independent translation layers:

| Layer | Format | What it covers |
|---|---|---|
| **App strings** | Mozilla Fluent (`.ftl`) | CLI help text, command descriptions, runtime messages |
| **Docs** | gettext (`.po`) | Everything in this mdBook |

They are filled separately and stored separately. Both use Claude as the translation backend.

## Building the docs locally

{{#include ../developing/building-docs.md}}

## Filling app strings (Fluent)

App strings live in `crates/zeroclaw-runtime/locales/`. English is the source of truth and is embedded at compile time. Non-English locales are loaded from `~/.zeroclaw/workspace/locales/` at runtime.

```bash
cargo fluent stats                        # coverage per locale
cargo fluent check                        # validate .ftl syntax
cargo fluent fill --locale ja             # fill missing keys via Anthropic API
cargo fluent fill --locale ja --force     # retranslate everything (quality pass)
cargo fluent fill --locale ja --provider ollama  # use a local Ollama provider instead
cargo fluent scan                         # find stale or missing keys vs Rust source
```

`cargo fluent fill` reads `ANTHROPIC_API_TOKEN` for the Anthropic backend. For a local model, configure a provider in `config.toml` and pass `--provider <name>`:

```toml
[providers.models.ollama]
name = "ollama"
base_url = "http://localhost:11434"
model = "llama3.2"
```

After filling, copy the updated `.ftl` file to your workspace and rebuild the binary to pick up the changes:

```bash
cp crates/zeroclaw-runtime/locales/ja/cli.ftl ~/.zeroclaw/workspace/locales/ja/cli.ftl
```

## Filling doc translations (gettext)

Doc translations live in `docs/book/po/`. `cargo mdbook sync` runs extract → merge → AI-fill in one step.

The fill step calls `ANTHROPIC_API_TOKEN` (or `ANTHROPIC_API_KEY`). Without a key, sync still runs extract + merge and reports how many strings need translation — partial translations are valid and fall back to English at render time.

```bash
ANTHROPIC_API_TOKEN=sk-ant-... cargo mdbook sync
ANTHROPIC_API_TOKEN=sk-ant-... FILL_MODEL=claude-sonnet-4-6 cargo mdbook sync --force
```

## Adding a new locale

1. Add the locale code to `crates/zeroclaw-runtime/locales/` — create the directory and run `cargo fluent fill --locale <code>`.

2. Add the locale to `xtask/src/util.rs` → `locales()`:
   ```rust
   &["en", "ja", "<code>"]
   ```

3. Bootstrap the `.po` file for docs:
   ```bash
   msginit --no-translator --locale=<code> \
     --input=docs/book/po/messages.pot \
     --output=docs/book/po/<code>.po
   ```
   Then run `cargo mdbook sync --locale <code>` to fill it.

4. Register the locale in the lang switcher — `docs/book/theme/lang-switcher.js`:
   ```js
   { code: "<code>", label: "Language Name" },
   ```

5. Add the locale to the deploy workflow — `.github/workflows/docs-deploy.yml`:
   ```yaml
   LOCALES: en ja <code>
   ```
