# Docs & Translations

ZeroClaw has two independent translation layers:

| Layer | Format | What it covers |
|---|---|---|
| **App strings** | Mozilla Fluent (`.ftl`) | CLI help text, command descriptions, runtime messages |
| **Docs** | gettext (`.po`) | Everything in this mdBook |

They are filled separately and stored separately. Both use a provider-agnostic fill pipeline: configure any OpenAI-compatible endpoint in `~/.zeroclaw/config.toml` under `[providers.models.<name>]` and pass `--provider <name>` to the fill commands.

Local models via [Ollama](https://ollama.com) are a first-class option — no API keys required, no per-call cost. A hosted provider is also fine for release-grade quality. Translation is a local operation — run `cargo mdbook sync` before you PR.

## Provider configuration

Ollama is the current canonical source for docs. Ensure you have [Ollama](https://ollama.com/) installed and have `qwen3.6:35-a3b` pulled. Then, in `~/.zeroclaw/config.toml` (or your established config home):

```toml
# Local via Ollama — free, runs on your machine
[providers.models.ollama]
base_url = "http://localhost:11434"
model = "qwen3.6:35b-a3b" # Current preferred model
```

## Building the docs locally

{{#include ../developing/building-docs.md}}

## Filling app strings (Fluent)

App strings live in `crates/zeroclaw-runtime/locales/`. English is the source of truth and is embedded at compile time. Non-English locales are loaded from `~/.zeroclaw/workspace/locales/` at runtime.

```bash
cargo fluent stats                                          # coverage per locale
cargo fluent check                                          # validate .ftl syntax
cargo fluent fill --locale ja --provider ollama             # fill missing keys (default batch 50)
cargo fluent fill --locale ja --provider ollama --batch 1   # one-at-a-time (use when a file has long entries that truncate at batch 50, e.g. tools.ftl)
cargo fluent fill --locale ja --provider ollama --force     # retranslate everything
cargo fluent scan                                           # find stale or missing keys vs Rust source
```

Each batch is written to disk before the next API call, so a mid-run failure only loses the in-flight batch. Re-running skips keys that already exist in the target `.ftl`, so resume is automatic — no `--force` needed.

After filling, copy the updated `.ftl` file to your workspace and rebuild to pick up the changes:

```bash
mkdir -p ~/.zeroclaw/workspace/locales/ja
cp crates/zeroclaw-runtime/locales/ja/cli.ftl ~/.zeroclaw/workspace/locales/ja/cli.ftl
```

## Filling doc translations (gettext)

Doc translations live in `docs/book/po/`. `cargo mdbook sync` runs extract → merge → strip obsolete → AI-fill in one step. Without `--provider`, sync still runs extract + merge and reports how many strings need translation — partial translations fall back to English at render time.

```bash
cargo mdbook sync --provider ollama              # delta fill
cargo mdbook sync --provider ollama --force      # quality pass: retranslate all entries
cargo mdbook sync --provider ollama --batch 1    # one-at-a-time (helpful for flaky local models)
cargo mdbook sync --locale ja --provider ollama  # single locale
```

The pipeline has built-in resilience:

- **Leak detection** — if a model returns its own instructions instead of a translation, the tool detects the pattern (via response-length ratio and bullet-list structure), attempts to recover the real translation from the response tail, and blanks the entry for re-translation if recovery fails.
- **Incremental writes** — after each batch, the `.po` file is rewritten. A Ctrl-C mid-run doesn't lose the progress up to that point.
- **Obsolete stripping** — `msgmerge` + `msgattrib --no-obsolete` keep removed source strings from accumulating as `#~` entries.

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

Everything else — `lang-switcher.js`, CI deploy target list, `cargo mdbook locales` output — reads from `locales.toml` automatically.

## Model quality notes

Translation quality varies significantly by language and model.

| Locale | Well-supported by | Notes |
|---|---|---|
| `ja`, `zh-CN` | qwen3.6 family, any frontier hosted model | Qwen is Chinese-first; Japanese also strong |
| `es`, `fr` | qwen3.6, mistral, gemma3, hosted | Romance languages are broadly well-trained |
| Low-resource locales | Hosted frontier models only | Local models often hallucinate words |

For release-grade passes, prefer a hosted frontier model via `--force`. For ongoing delta fills during development, a local Ollama model is fine and free.
