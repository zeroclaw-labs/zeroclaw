# Localization catalog lifecycle

ZeroClaw has two localization branches with different formats and consumers. Mozilla Fluent catalogs provide application strings for the runtime and zerocode. gettext catalogs translate the mdBook documentation after English source and generated references have been assembled.

The branches share a locale registry and provider-backed fill philosophy, but they are not interchangeable. A translated file being tracked in the repository also does not prove that a particular binary embeds or loads it. Use this page to follow each catalog from English source through generation, validation, runtime or site consumption, and release.

## Two localization branches

| Branch | English source | Translated catalogs | Materializer | Consumer |
| --- | --- | --- | --- | --- |
| Runtime and tool Fluent | `crates/zeroclaw-runtime/locales/en/cli.ftl` and `tools.ftl` | `crates/zeroclaw-runtime/locales/<locale>/*.ftl` in the main repository | `cargo fluent fill`, with `check`, `scan`, and `stats` for validation and coverage | Runtime CLI and prompt strings through `zeroclaw-runtime/src/i18n.rs`; tool-owned schema and result strings through `zeroclaw-tools/src/i18n.rs` |
| zerocode Fluent | `apps/zerocode/locales/en/zerocode.ftl` | `apps/zerocode/locales/<locale>/zerocode.ftl` in the main repository | The same `cargo fluent` command surface, optionally scoped to the zerocode catalog | zerocode strings loaded over the embedded English catalog from the shared disk locale directory |
| Documentation gettext | English `docs/book/src/` after generated references and preprocessors supply source text | `docs/book/po/<locale>.po` in the translation-catalog submodule | `cargo mdbook sync` plus `tools/fill-translations` | `mdbook-gettext` during each locale build |

`locales.toml` is the shared registry for locale codes and display labels. It drives docs locale builds and the generated language switcher, and is embedded by the runtime for locale discovery. It does not by itself make every catalog available to every consumer; each loader still defines how its files are embedded or found on disk.

## Fluent application strings

English Fluent files are authored sources. Keys identify messages, while values contain the English text and any Fluent variables. Product names, command literals, identifiers, and placeholders remain literal where the message contract requires them.

`cargo fluent` walks the runtime and zerocode catalog roots. `fill` compares each English file with the selected locale, translates missing keys through the configured model provider, writes progress after each batch, and modifies tracked `.ftl` files. `check` parses catalog syntax, `scan` compares source references with catalogs, and `stats` reports coverage without changing catalogs. Fluent diffs belong in a deliberate localization change rather than incidental application work.

Storage and loading are separate concerns:

- Runtime CLI strings always have embedded English. The loader can also use translated CLI catalogs embedded by `builtin_cli_ftl_source`, then applies a disk catalog as the highest-priority locale source.
- Runtime prompt-facing tool descriptions always have embedded English and overlay translated `tools.ftl` values from disk; optional missing lookups return no value.
- `zeroclaw-tools` independently embeds English and loads disk `tools.ftl` for tool-owned schema and result strings because its crate cannot depend on runtime; required missing lookups render a visible `{key}` marker.
- zerocode embeds its English catalog and overlays a translated `zerocode.ftl` from disk. `ZEROCODE_LOCALE_DIR` is an explicit test override; the normal shared location is `<config-dir>/data/ftl/<locale>/zerocode.ftl`.
- `zeroclaw locales fetch` downloads selected runtime and zerocode catalogs into that shared disk locale directory using the catalog paths declared by `zeroclaw-config`.

For runtime, tools, and zerocode, English remains the base map. A translated disk or built-in catalog replaces keys it contains; absent translated keys keep their English value. Required lookups report a key absent from every available source and render a visible `{key}` marker rather than silently inventing text; optional runtime tool-description lookups return no value.

## gettext documentation strings

English Markdown is the authored documentation source, but extraction also sees generated references, included snippets, and preprocessor output materialized for the extraction build. Before running mdBook with xgettext output, `cargo mdbook sync` calls the shared `prepare_generated_book_inputs()` path used by locale builds and single-locale serving. That path regenerates the CLI and config references, locale switcher, theme, keymap, hardware, feature-matrix, and plugin inputs from their canonical sources. Extraction also runs with the built peer-groups preprocessor, so a clean checkout does not depend on ignored files or binaries left by an earlier docs build.

`cargo mdbook sync` extracts English messages into `messages.pot`, normalizes the template, bootstraps or merges each locale without fuzzy matching, removes obsolete entries, and reports the untranslated delta. When a model provider is supplied, it fills missing translations through `tools/fill-translations`; otherwise it makes no provider calls. The maintainer guide owns the command options and operating procedure.

The fill tool treats one gettext entry as one source-to-translation mapping. It repairs or clears model responses that contain prompt leakage or a new machine-local absolute path, preserves required trailing newlines, writes incrementally, and removes fuzzy flags from accepted entries. `cargo mdbook check` separately parses every PO file and rejects suspicious generated responses, corrupted protected literals, and introduced local paths.

## Partial translation and fallback

The gettext preprocessor renders the English `msgid` when a locale has no usable translated value for that entry. A locale can therefore show translated navigation and paragraphs alongside newly added English prose. This mixed-language state means the English source advanced beyond the catalog's accepted coverage; it does not mean mdBook selected two languages for one page.

Common causes are:

- an English docs or generated-reference change added a new `msgid`;
- catalog sync merged the new source but no translation fill has run;
- a safety repair blanked a leaked, path-bearing, or otherwise unusable model response;
- a source edit replaced an old message with a new one;
- a locale catalog or release pin intentionally trails current `master`.

Fuzzy is a catalog-maintenance state, not a promise that the old value is safe to render. The current sync command disables fuzzy matching for new merges, while the fill tool can accept an existing non-empty fuzzy value and remove its flag. Review the resulting `msgstr`; do not infer publication behavior from the flag alone.

Translated locale builds disable full-text search. Only the primary locale, the first entry in `locales.toml`, receives the search index. This is a size decision in `build_locales`, not missing translation coverage.

## Catalog storage and release pins

Fluent catalogs live in the main repository. A normal Fluent translation change updates the intended `.ftl` files directly and is reviewed with the application code that consumes their keys or as a focused translation pass.

Documentation PO catalogs live in `zeroclaw-labs/zeroclaw-docs-translations`, mounted at `docs/book/po` as a git submodule. The main repository records one gitlink commit, not each PO file. `messages.pot` and translation failure logs are generated artifacts and are not part of the pinned catalog set.

The release helper `scripts/release/refresh-translations.sh` owns the translation tag and main-repository gitlink update. By default it runs sync and the catalog check, commits and pushes catalog changes in the submodule, creates and checks out the matching `v<version>` tag, and stages the gitlink. Its `--no-translate` mode skips both sync and the catalog check, so it is only appropriate after the current catalogs have been validated separately. The translation-pin workflow initializes the exact pinned commit, checks PO syntax, and verifies that locale catalogs expose the same `msgid` set.

Docs deployment initializes the pinned submodule and builds every locale already present. It does not call a model provider, fill missing translations, advance the submodule, or create a release tag.

## Validation and review boundaries

- Ordinary English docs PRs may defer broad PO churn to a focused translation-cache pass. Review the English source and generated boundary in the original PR.
- Include PO changes when translation or catalog maintenance is the purpose, a locale is being added, the generated delta is small and reviewable, or a release pass is advancing the pin.
- Include Fluent changes when keys or translated application strings change. Do not claim a translated runtime path works merely because its `.ftl` file exists; verify the relevant loader or fetch/install path.
- Keep protected command syntax, config keys, product names, JSON/TOML literals, and placeholders intact. Translate surrounding prose rather than weakening machine-facing examples.
- Treat an English fallback as visible evidence of missing accepted catalog coverage. Fix or fill the catalog source, not the rendered HTML.
- Review submodule changes as release/catalog operations: inspect both the main-repository gitlink and the catalog commit it selects.

For detailed commands, provider configuration, batching, adding a locale, and release procedure, see [Docs & Translations](../maintainers/docs-and-translations.md). For the English source and generated-reference stages that feed gettext extraction, see [Generated documentation pipeline](./generated-documentation-pipeline.md).

## Source pointers

- Locale registry: `locales.toml`
- Runtime Fluent loader: `crates/zeroclaw-runtime/src/i18n.rs`
- Tool-owned Fluent loader: `crates/zeroclaw-tools/src/i18n.rs`
- Runtime Fluent catalogs: `crates/zeroclaw-runtime/locales/`
- zerocode Fluent loader: `apps/zerocode/src/i18n.rs`
- zerocode Fluent catalogs: `apps/zerocode/locales/`
- Fluent tooling: `xtask/src/cmd/fluent/`
- Catalog download map: `zeroclaw_config::schema::FTL_CATALOGS`
- gettext extraction and merge: `xtask/src/cmd/mdbook/sync.rs`
- gettext safety checks: `xtask/src/cmd/mdbook/check.rs`
- gettext fill and repair: `tools/fill-translations/`
- Locale build and search behavior: `xtask/src/cmd/mdbook/build.rs`
- Translation pin validation: `.github/workflows/validate-translations-pin.yml`
- Release catalog refresh: `scripts/release/refresh-translations.sh`
- Docs deployment: `.github/workflows/docs-deploy.yml`
