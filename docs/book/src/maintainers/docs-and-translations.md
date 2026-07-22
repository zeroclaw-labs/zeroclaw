# Docs & Translations

ZeroClaw has two independent translation layers:

| Layer | Format | What it covers |
|---|---|---|
| **App strings** | Mozilla Fluent (`.ftl`) | CLI help text, command descriptions, runtime messages |
| **Docs** | gettext (`.po`) | Everything in this mdBook |

For the source-of-truth, storage, loading, fallback, and release boundaries behind these procedures, see [Localization catalog lifecycle](../architecture/localization-catalog-lifecycle.md). The generated English references that feed docs extraction are mapped in [Generated documentation pipeline](../architecture/generated-documentation-pipeline.md).

They are filled separately and stored separately. Both use the shared provider-agnostic runtime path: configure a model provider under `providers.models.<kind>.<alias>` and pass `--model-provider <alias>` to the fill commands. Any configured alias is choosable: a bare alias (`--model-provider <alias>`), or a `kind.alias` qualifier (`--model-provider anthropic.<alias>`) when the same alias exists under more than one kind. The resolver requires the matched entry to name a model, then delegates endpoint defaults, authentication, wire protocol, and optional custom `uri` handling to the runtime provider stack.

Local models via [Ollama](https://ollama.com) are a first-class option: no API keys required, no per-call cost. A hosted provider is also fine for release-grade quality. Translation is a local operation. Run `cargo mdbook sync` for dedicated translation-cache PRs, release translation passes, and new locales; routine English docs PRs may defer broad generated `.po` churn to a focused follow-up.

## Provider configuration

Ollama is the current canonical source for docs. Ensure you have [Ollama](https://ollama.com/) installed and have `qwen3:30b-a3b` pulled, then configure an Ollama provider entry. `uri` is the full endpoint URL and is **optional**: leave it unset to use the provider family's default endpoint (resolved by the runtime provider stack). Set it only to point at a self-hosted gateway or proxy. Any configured family works (Anthropic, OpenAI, OpenRouter, Ollama, …); the translation tools build the real runtime provider, so each family's endpoint, auth header, and wire protocol are handled for you: no OpenAI-compatibility requirement.

## Building the docs locally

{{#include ../_snippets/docs-build-commands.md}}

`cargo mdbook` is an alias for `cargo run -p xtask --bin mdbook --` (defined in the cargo config). For a lean contributor-facing version of this section, see [Building the docs locally](../developing/building-docs.md).

> [!NOTE]
> Full-text search is built only for the primary locale (English, first in `locales.toml`). Translated locales build without a search index or search box. Per-locale search indexes are large (~6-7 MB each) and dominate `gh-pages` clone size; restricting search to English keeps clones lean. Adding a search box back to a translated locale means re-enabling `output.html.search.enable` for that build in `build_locales` (`xtask/src/cmd/mdbook/build.rs`).

### How translations stay current

When English source changes, `cargo mdbook sync` runs two stages:

1. **Extract**: `mdbook-xgettext` regenerates `po/messages.pot` from the current English source.
2. **Merge**: `msgmerge --no-fuzzy-matching` updates each locale's `.po` file, gives new or changed source strings an empty `msgstr ""`, and removes obsolete entries. Only fuzzy entries already present before the merge can remain available for later review or fill acceptance.

Then the command counts fuzzy + untranslated entries and, when `--model-provider` is given, fills only those. Unchanged strings cost nothing: the `.po` cache means re-running against unchanged source is a no-op. Without `--model-provider`, sync still runs extract + merge and reports the delta; strings without a `msgstr` fall back to English at render time.

Sync normalizes catalogs with stable output rules (`msgcat --sort-output --no-wrap --add-location=file`), so diffs stay focused on real source changes. Unavoidable churn: header metadata (`POT-Creation-Date` etc.), reference-location updates when a string moves files, and actual source-string edits.

Routine English docs PRs may defer broad `.po` churn to a focused follow-up. Include `.po` updates only when the PR is a translation-cache pass, a release-translation pass, adds a locale, or produces a small reviewable diff.

## Filling app strings (Fluent)

App strings live in `crates/zeroclaw-runtime/locales/`. English is the source of truth and is embedded at compile time.

> **Runtime loading boundary.**
>
> - **Embedded sources:** English `cli.ftl` and `tools.ftl` are embedded. `builtin_cli_ftl_source()` enumerates the non-English CLI catalogs embedded by the runtime; `zeroclaw-tools` separately embeds English tool strings to preserve crate dependency direction.
> - **Disk overlay:** A catalog at `<config-dir>/data/ftl/<locale>/` overrides an embedded CLI value and supplies translated runtime/tool values. `zeroclaw locales fetch` populates this shared directory.
> - **Consumption caveat:** Filling and committing an `.ftl` file updates tracked catalog source, but a consumer uses it only when its loader embeds that catalog or the file is installed where that loader reads it.
>
> The `apps/zerocode` TUI maintains an independent Fluent catalogue (`apps/zerocode/locales/`), see [zerocode strings](#zerocode-strings-fluent-independent) below. `cargo fluent` walks **both** catalogue roots (runtime + zerocode), so every subcommand below covers both by default.

<div class="os-tabs-src">

#### sh

```sh
cargo fluent stats                                                   # coverage per locale, per catalogue
cargo fluent check                                                   # validate .ftl syntax across both catalogues
cargo fluent fill --locale ja --model-provider anthropic.<alias>             # fill missing keys (default batch 50)
cargo fluent fill --locale ja --model-provider anthropic.<alias> --batch 10  # smaller batches: fewer entries per request (eases rate limits / truncation)
cargo fluent fill --locale ja --model-provider anthropic.<alias> --force     # retranslate everything
cargo fluent scan                                                    # find stale or missing keys vs Rust source
```

</div>

**Scoping to one catalogue**: every subcommand takes `--catalog <runtime|zerocode>` (default: both). To translate only the TUI:

<div class="os-tabs-src">

#### sh

```sh
cargo fluent fill --locale ja --model-provider anthropic.<alias> --catalog zerocode
cargo fluent check --catalog zerocode                                # syntax-check only zerocode
```

</div>

An unknown `--catalog` value errors with the valid choices.

`fill` generates `<locale>/<domain>.ftl` for every selected catalogue root that has an `en/` directory: the runtime's `cli.ftl`/`tools.ftl` and zerocode's `zerocode.ftl`.

**Provider resolution is shared with the runtime.** `--model-provider` accepts any alias configured under `[providers.models.<kind>.<alias>]`: a bare alias (`<alias>`) or a `kind.alias` qualifier (`anthropic.<alias>`) when ambiguous. The tool builds the actual runtime provider, so the endpoint, auth header, and wire protocol are resolved per family (Anthropic `/v1/messages` + `x-api-key`, OpenAI-compatible `/v1/chat/completions` + `Bearer`, etc.): nothing is assumed. Encrypted `api_key` values are decrypted through the canonical `SecretStore`. Use `--config-dir <dir>` (mirrors `zeroclaw --config-dir`) to read config + `.secret-key` from a non-default location; defaults to `~/.zeroclaw` then `~/.config/zeroclaw`.

**Batching:** `fill` sends one request per batch (all N entries as a single JSON object); `--batch` lowers N to ease provider rate limits or response truncation on long entries. Each batch is written to disk before the next request, so a mid-run failure only loses the in-flight batch. Re-running skips keys that already exist in the target `.ftl`, so resume is automatic: no `--force` needed.

## zerocode strings (Fluent, independent)

`apps/zerocode` carries its own self-contained Fluent setup, separate from the runtime catalogues above. The TUI is intentionally decoupled from the rest of the workspace: it has no `zeroclaw-*` crate dependency, and its strings live next to its source rather than under `zeroclaw-runtime/locales/`.

| Where | What |
|---|---|
| `apps/zerocode/locales/en/zerocode.ftl` | Source of truth, embedded at compile time |
| `apps/zerocode/locales/<locale>/zerocode.ftl` | Tracked translated catalog source used by fill/fetch and release workflows; not embedded automatically |
| `$ZEROCODE_LOCALE_DIR/<locale>/zerocode.ftl` | Explicit override, useful for testing translations |
| `<config-dir>/data/ftl/<locale>/zerocode.ftl` | Shared per-user catalog written by `zeroclaw locales fetch` and loaded by zerocode |

### Key namespace

All zerocode keys are prefixed `zc-` and never collide with the runtime's `cli-`, `channel-`, or `tool-` namespaces. The convention inside `zc-` is `zc-<pane>-<purpose>`:

- `zc-pane-<name>`: top-level mode bar labels
- `zc-app-<purpose>`: strings owned by `app.rs` (dialogs, help, status)
- `zc-<pane>-<purpose>`: strings local to a specific pane (`zc-dashboard-*`, `zc-chat-*`, …)

### Chord literals are not translated

Chord glyphs like `Ctrl+C`, `Esc`, `Shift+Up` are protocol, not language. The `HelpEntry` and `HelpNode` constructors take the chord vector as `&'static str` and the description as `String`, so chord literals stay hard-coded while descriptions flow through `t()`. When prose embeds a chord inline, use a `{ $keys }` Fluent slot and pass the chord at render time rather than concatenating translated text around a literal.

### Locale resolution

Locale comes from a top-level `locale` field in zerocode's config. When unset, `i18n::detect_locale()` reads the config dir resolved as `--config-dir`, then `ZEROCLAW_CONFIG_DIR`, then `~/.zeroclaw`, and otherwise falls back to `en`. zerocode resolves its locale independently from its own config; it does not share the daemon's lookup.

### Adding strings

1. Add the key + English value to `apps/zerocode/locales/en/zerocode.ftl`. Group keys by source file with a section comment so the catalogue stays scannable.
2. Replace the literal in the source with `crate::i18n::t("zc-…")`. For enum→label `match` arms, return the key constant (`&'static str`) from a `fluent_key()` method and call `t()` at the render site, never `match` on a string.
3. `cargo check -p zerocode` and the `i18n` unit tests (`cargo test -p zerocode i18n`) catch missing keys at compile/test time. Missing keys at runtime render as `{zc-key-name}` and emit a one-shot stderr warning.

### Filling translations

`cargo fluent` walks the zerocode catalogue alongside the runtime one, so no separate fill command is needed. Running `cargo fluent fill --locale <code> --model-provider <alias>` generates `apps/zerocode/locales/<code>/zerocode.ftl` in the same pass that fills the runtime catalogue. `cargo fluent check` and `cargo fluent stats` likewise report zerocode; `scan` indexes `apps/` so `zc-` key references resolve against zerocode's source. To exercise the translation in zerocode, install it through `zeroclaw locales fetch` or place it under one of the two disk-search roots above.

## Filling doc translations (gettext)

Doc translations live in `docs/book/po/`. `cargo mdbook sync` runs extract → merge → strip obsolete → AI-fill in one step. Without `--model-provider`, sync still runs extract + merge and reports how many strings need translation: partial translations fall back to English at render time.

<div class="os-tabs-src">

#### sh

```sh
cargo mdbook sync --model-provider anthropic.<alias>              # delta fill
cargo mdbook sync --model-provider anthropic.<alias> --force      # quality pass: retranslate all entries
cargo mdbook sync --model-provider anthropic.<alias> --batch 1    # write after every entry (safest resume)
cargo mdbook sync --locale ja --model-provider anthropic.<alias>  # single locale
cargo mdbook sync --model-provider anthropic.<alias> --config-dir ~/.zeroclaw  # qualified alias + explicit config dir
```

</div>

`--model-provider` resolves through the same shared runtime provider path as `cargo fluent` (any configured family/alias, per-family endpoint + auth + wire protocol, `SecretStore` decryption, `--config-dir` support). Unlike `cargo fluent`, which sends a whole batch as one JSON object, the gettext filler issues **one request per source string** to keep the `msgid → msgstr` mapping unambiguous, so `--batch` controls how often the `.po` is flushed to disk (the checkpoint interval), not the request size. A full-catalogue locale is thousands of sequential requests; for routine delta fills a cheap local Ollama alias is the economical choice.

The pipeline has built-in resilience:

- **Leak detection**: if a model returns its own instructions instead of a translation, the tool detects the pattern (via response-length ratio and bullet-list structure), attempts to recover the real translation from the response tail, and blanks the entry for re-translation if recovery fails.
- **Protected literal checks**: `cargo mdbook check` also rejects high-confidence literal corruption in generated `.po` files. Product names such as `ZeroClaw Maturity Framework`, command literals such as `zeroclaw daemon`, and fenced TOML section/key literals must stay byte-for-byte intact inside translations. Translate the surrounding prose, not the machine-facing text.
- **Path leak checks**: generated translations must not introduce machine-local absolute paths that were not present in the English source; those entries are blanked for re-translation and rejected by `cargo mdbook check`.
- **Incremental writes**: after each batch, the `.po` file is rewritten. A Ctrl-C mid-run doesn't lose the progress up to that point.
- **Obsolete stripping**: `msgmerge` + `msgattrib --no-obsolete` keep removed source strings from accumulating as `#~` entries.

Maintainers should accept the routine English docs exception documented in [Building the docs locally](../developing/building-docs.md). Ask for `.po` updates only when the PR is itself a translation-cache pass, a release translation pass, a new-locale change, or the generated diff is small enough to review.

## Adding a new locale

1. Edit `locales.toml` at the repo root, the **only** file you need to touch:

2. Translate the app strings:

   <div class="os-tabs-src">

   #### sh

   ```sh
   cargo fluent fill --locale <code> --model-provider ollama
   ```

   </div>

3. Bootstrap and fill the docs `.po` file:

   <div class="os-tabs-src">

   #### sh

   ```sh
   cargo mdbook sync --locale <code> --model-provider ollama
   ```

   </div>

4. The `cargo fluent fill` run in step 2 already generates `apps/zerocode/locales/<code>/zerocode.ftl` in the same pass, since `cargo fluent` walks both the runtime and zerocode catalogues. No manual zerocode step is needed; verify coverage with `cargo fluent stats`.

Everything else, `lang-switcher.js`, CI deploy target list, `cargo mdbook locales` output, reads from `locales.toml` automatically.

## Translation catalogue submodule

The translated `.po` catalogues are not in this repo's main tree. They live in the dedicated [`zeroclaw-labs/zeroclaw-docs-translations`](https://github.com/zeroclaw-labs/zeroclaw-docs-translations) repo, mounted as a git submodule at `docs/book/po` (default branch `main`). The mount point is path-transparent: `book.toml`'s gettext preprocessor, `cargo mdbook sync`, and `cargo mdbook build` all read `po/` exactly as before.

The Rust crate dev loop never needs the submodule. Only docs builds and the docs-deploy / release jobs require it; those checkouts pass `submodules: recursive`. Everything else stays submodule-free.

Per release, `scripts/release/refresh-translations.sh` publishes changed catalogues to the submodule's `main` branch, tags that commit as `v{version}`, checks out the tag, and stages the main-repository gitlink. `bump-version.sh` deliberately leaves translation pinning to that helper. `messages.pot` and `*.failures.log` are regenerated artifacts and are gitignored in both repos, not tracked.

## Release translation workflow

The release-time refresh, validation, tagging, push, and gitlink-pin procedure
is part of [Step 2 in the Release Runbook](release-runbook.md#refresh-and-pin-translations).
This page documents the translation system; use the runbook as the operational
source of truth when preparing a release.

## Model quality notes

Translation quality varies significantly by language and model.

| Locale | Well-supported by | Notes |
|---|---|---|
| `ja`, `zh-CN` | qwen3 family, any frontier hosted model | Qwen is Chinese-first; Japanese also strong |
| `es`, `fr` | qwen3, mistral, gemma3, hosted | Romance languages are broadly well-trained |
| Low-resource locales | Hosted frontier models only | Local models often hallucinate words |

For release-grade passes, prefer a hosted frontier model via `--force`. For ongoing delta fills during development, a local Ollama model is fine and free.
