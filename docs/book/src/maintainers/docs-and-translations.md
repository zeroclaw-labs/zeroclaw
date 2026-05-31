# Docs & Translations

ZeroClaw has two independent translation layers:

| Layer | Format | What it covers |
|---|---|---|
| **App strings** | Mozilla Fluent (`.ftl`) | CLI help text, command descriptions, runtime messages |
| **Docs** | gettext (`.po`) | Everything in this mdBook |

They are filled separately and stored separately. Both use a provider-agnostic fill pipeline: configure any OpenAI-compatible endpoint in `~/.zeroclaw/config.toml` under `[providers.models.<kind>.<alias>]` and pass `--model-provider <alias>` to the fill commands. Any configured alias is choosable — a bare alias (`--model-provider lineation`), or a `kind.alias` qualifier (`--model-provider anthropic.lineation`) when the same alias exists under more than one kind. The resolver reads `uri`, `model`, and `api_key` straight from the matched entry; a missing `uri` or `model` is a hard error, not a guessed default.

Local models via [Ollama](https://ollama.com) are a first-class option — no API keys required, no per-call cost. A hosted provider is also fine for release-grade quality. Translation is a local operation. Run `cargo mdbook sync` for dedicated translation-cache PRs, release translation passes, and new locales; routine English docs PRs may defer broad generated `.po` churn to a focused follow-up.

## Provider configuration

Ollama is the current canonical source for docs. Ensure you have [Ollama](https://ollama.com/) installed and have `qwen3.6:35-a3b` pulled. Then, in `~/.zeroclaw/config.toml` (or your established config home):

```toml
# Local via Ollama — free, runs on your machine
[providers.models.ollama.local]
uri   = "http://localhost:11434/v1/chat/completions"
model = "qwen3.6:35b-a3b" # Current preferred model
```

`uri` is the full endpoint URL. If it already ends in `/chat/completions` it is used verbatim; a bare base (`http://localhost:11434`) has `/v1/chat/completions` appended. A hosted alias works the same way — point `uri` at the provider's OpenAI-compatible completions endpoint and set `api_key`.

## Building the docs locally

{{#include ../developing/building-docs.md}}

## Filling app strings (Fluent)

App strings live in `crates/zeroclaw-runtime/locales/`. English is the source of truth and is embedded at compile time. Non-English locales are loaded from `~/.zeroclaw/workspace/locales/` at runtime.

> The `apps/zerocode` TUI maintains an independent Fluent catalogue (`apps/zerocode/locales/`) — see [zerocode strings](#zerocode-strings-fluent-independent) below. `cargo fluent` walks **both** catalogue roots (runtime + zerocode), so every subcommand below covers zerocode automatically.

```bash
cargo fluent stats                                                 # coverage per locale, per catalogue
cargo fluent check                                                 # validate .ftl syntax across both catalogues
cargo fluent fill --locale ja --model-provider lineation           # fill missing keys (default batch 50)
cargo fluent fill --locale ja --model-provider lineation --batch 1 # one-at-a-time (use when a file has long entries that truncate at batch 50, e.g. tools.ftl)
cargo fluent fill --locale ja --model-provider lineation --force   # retranslate everything
cargo fluent scan                                                  # find stale or missing keys vs Rust source
```

`fill` generates `<locale>/<domain>.ftl` for **every** catalogue root that has an `en/` directory — the runtime's `cli.ftl`/`tools.ftl` and zerocode's `zerocode.ftl` are all produced in one run. `--model-provider` accepts any alias configured under `[providers.models.<kind>.<alias>]`; if the alias is ambiguous across kinds the error lists the qualified `kind.alias` forms to pick from.

Each batch is written to disk before the next API call, so a mid-run failure only loses the in-flight batch. Re-running skips keys that already exist in the target `.ftl`, so resume is automatic — no `--force` needed.

After filling, copy the updated `.ftl` file to your workspace and rebuild to pick up the changes:

```bash
mkdir -p ~/.zeroclaw/workspace/locales/ja
cp crates/zeroclaw-runtime/locales/ja/cli.ftl ~/.zeroclaw/workspace/locales/ja/cli.ftl
```

## zerocode strings (Fluent, independent)

`apps/zerocode` carries its own self-contained Fluent setup, separate from the runtime catalogues above. The TUI is intentionally decoupled from the rest of the workspace — it has no `zeroclaw-*` crate dependency, and its strings live next to its source rather than under `zeroclaw-runtime/locales/`.

| Where | What |
|---|---|
| `apps/zerocode/locales/en/zerocode.ftl` | Source of truth, embedded at compile time |
| `apps/zerocode/locales/<locale>/zerocode.ftl` | Other locales, embedded if present in-tree |
| `$ZEROCODE_LOCALE_DIR/<locale>/zerocode.ftl` | Explicit override, useful for testing translations |
| `<config-dir>/zerocode/locales/<locale>/zerocode.ftl` | Per-user catalogue override |
| `~/.zeroclaw/zerocode/locales/<locale>/zerocode.ftl` | Alternate per-user location |
| `<install-prefix>/share/zerocode/locales/<locale>/zerocode.ftl` | System install path |

### Key namespace

All zerocode keys are prefixed `zc-` and never collide with the runtime's `cli-`, `channel-`, or `tool-` namespaces. The convention inside `zc-` is `zc-<pane>-<purpose>`:

- `zc-pane-<name>` — top-level mode bar labels
- `zc-app-<purpose>` — strings owned by `app.rs` (dialogs, help, status)
- `zc-<pane>-<purpose>` — strings local to a specific pane (`zc-dashboard-*`, `zc-chat-*`, …)

### Chord literals are not translated

Chord glyphs like `Ctrl+C`, `Esc`, `Shift+Up` are protocol, not language. The `HelpEntry` and `HelpNode` constructors take the chord vector as `&'static str` and the description as `String`, so chord literals stay hard-coded while descriptions flow through `t()`. When prose embeds a chord inline, use a `{ $keys }` Fluent slot and pass the chord at render time rather than concatenating translated text around a literal.

### Locale resolution

Locale comes from a top-level `locale` field in `zerocode-config.toml`. When unset, `i18n::detect_locale()` walks (in order) `<config-dir>/zerocode/zerocode-config.toml`, `~/.zeroclaw/zerocode-config.toml`, `~/.zeroclaw/config.toml`, then `<config-dir>/zeroclaw/config.toml`, finally falling back to `en`. The same lookup matches how the daemon resolves its own locale.

### Adding strings

1. Add the key + English value to `apps/zerocode/locales/en/zerocode.ftl`. Group keys by source file with a section comment so the catalogue stays scannable.
2. Replace the literal in the source with `crate::i18n::t("zc-…")`. For enum→label `match` arms, return the key constant (`&'static str`) from a `fluent_key()` method and call `t()` at the render site — never `match` on a string.
3. `cargo check -p zerocode` and the `i18n` unit tests (`cargo test -p zerocode i18n`) catch missing keys at compile/test time. Missing keys at runtime render as `{zc-key-name}` and emit a one-shot stderr warning.

### Filling translations

`cargo fluent` walks the zerocode catalogue alongside the runtime one, so no manual step is needed. Running `cargo fluent fill --locale <code> --model-provider <alias>` generates `apps/zerocode/locales/<code>/zerocode.ftl` in the same pass that fills the runtime catalogue. `cargo fluent check` and `cargo fluent stats` likewise report zerocode; `scan` indexes `apps/` so `zc-` key references resolve against zerocode's source. The generated `<code>/zerocode.ftl` is embedded in-tree at compile time, or can be dropped into any of the disk-search paths above for testing with `--config-dir`.

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

Maintainers should accept the routine English docs exception documented in [Building the docs locally](../developing/building-docs.md). Ask for `.po` updates only when the PR is itself a translation-cache pass, a release translation pass, a new-locale change, or the generated diff is small enough to review.

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

4. For zerocode parity, copy `apps/zerocode/locales/en/zerocode.ftl` to `apps/zerocode/locales/<code>/zerocode.ftl` and translate the values by hand. `cargo fluent` does not yet operate on the zerocode catalogue; the file can be dropped into any of the disk-search paths or embedded in-tree once translated.

Everything else — `lang-switcher.js`, CI deploy target list, `cargo mdbook locales` output — reads from `locales.toml` automatically.

## Model quality notes

Translation quality varies significantly by language and model.

| Locale | Well-supported by | Notes |
|---|---|---|
| `ja`, `zh-CN` | qwen3.6 family, any frontier hosted model | Qwen is Chinese-first; Japanese also strong |
| `es`, `fr` | qwen3.6, mistral, gemma3, hosted | Romance languages are broadly well-trained |
| Low-resource locales | Hosted frontier models only | Local models often hallucinate words |

For release-grade passes, prefer a hosted frontier model via `--force`. For ongoing delta fills during development, a local Ollama model is fine and free.
