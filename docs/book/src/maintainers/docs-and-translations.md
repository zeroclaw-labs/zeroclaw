# Docs & Translations

ZeroClaw has three independent translation layers:

| Layer | Format | What it covers |
|---|---|---|
| **Runtime app strings** | Mozilla Fluent (`.ftl`) | CLI help text, command descriptions, runtime messages |
| **Web dashboard strings** | TypeScript locale modules (`web/src/lib/i18n/locales/*.ts`) | React dashboard navigation, settings, config editor labels, helper text, and placeholders |
| **Docs** | gettext (`.po`) | Everything in this mdBook |

They are filled separately and stored separately. Fluent and docs translations use a provider-agnostic fill pipeline: configure any OpenAI-compatible endpoint in `~/.zeroclaw/config.toml` under `[providers.models.<name>]` and pass `--provider <name>` to the fill commands. Web dashboard strings are curated directly in TypeScript locale modules so UI-specific keys can stay close to the React code that consumes them.

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

## Updating web dashboard strings (TypeScript)

Dashboard locale data lives under `web/src/lib/i18n/`:

| Path | Purpose |
|---|---|
| `web/src/lib/i18n.ts` | Runtime locale state plus helpers such as `t()`, `tWithFallback()`, and config-specific fallback helpers |
| `web/src/lib/i18n/types.ts` | Supported `Locale` union and message-map type |
| `web/src/lib/i18n/supportedLocales.ts` | Language picker metadata |
| `web/src/lib/i18n/translations.ts` | Locale module registry |
| `web/src/lib/i18n/locales/<locale>.ts` | Per-locale dashboard messages |

When adding or changing dashboard copy:

1. Add English source strings in `web/src/lib/i18n/locales/en.ts`.
2. Add the requested target locale strings in `web/src/lib/i18n/locales/<locale>.ts`.
3. Import and call `t()` / `tWithFallback()` from React components instead of hardcoding visible UI text.
4. For schema-driven config editor fields, prefer stable keys:

   | Key family | Used for |
   |---|---|
   | `config.label.<path>` / `config.label.leaf.<leaf>` | Field labels and common leaf-name fallbacks |
   | `config.description.<path>` | Schema-derived field descriptions and picker descriptions |
   | `config.placeholder.<path>` | Field-specific placeholders |

   Dynamic map entries should use wildcard keys such as
   `config.description.providers.models.*.model` instead of per-user map keys.
   The helper layer normalizes snake_case and kebab-case paths, tries wildcard
   candidates for map-shaped sections, then falls back to English and finally to
   the schema-provided text.

5. Validate dashboard changes with:

   ```bash
   npm run build --prefix web
   git diff --check
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
