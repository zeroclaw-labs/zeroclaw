# Generated documentation pipeline

ZeroClaw documentation combines hand-authored Markdown with references and snippets materialized from Rust types, command definitions, registries, WIT contracts, workflow files, and UI metadata. The generated file is not a second source of truth: fix the owning source or generator, then rebuild the documentation.

Use this page when a change touches a schema, CLI flag, feature or hardware inventory, plugin contract, default keymap, theme registry, mdBook directive, generated reference, docs gate, or deployment workflow. For configuration values specifically, also read [Config lifecycle](./config-lifecycle.md). For translated output, continue with [Localization catalog lifecycle](./localization-catalog-lifecycle.md).

## Source-to-output map

| Surface | Canonical source | Materializer | Output | Repository state | Consumer |
| --- | --- | --- | --- | --- | --- |
| Config reference | `zeroclaw_config::schema::Config` plus `Configurable` derives | `cargo mdbook refs` or `cargo mdbook build`, through `markdown-schema` | `docs/book/src/reference/config.md` | Ignored derived file | Config reference chapter and schema-backed directives |
| CLI reference | Clap command tree in `src/main.rs` | `cargo mdbook refs` or `cargo mdbook build`, through `markdown-help` | `docs/book/src/reference/cli.md` | Ignored derived file | CLI reference chapter |
| Installation paths | Typed route contracts in `xtask/src/generate/spec.rs` and generated behavior bodies in the installer renderers | `cargo generate installers`, through `xtask/src/generate/docs.rs` and `xtask/src/generate/install_sh.rs` | `docs/book/src/_snippets/install.md`, generated regions in `install.sh`, and the Windows prebuilt block in `docs/book/src/setup/windows.md` | Tracked generated surfaces | README-linked first-time setup, executable Unix routes, Quickstart, and platform setup pages |
| Rust API reference | Public Rust items across workspace crates | `cargo doc` inside `cargo mdbook refs` or `cargo mdbook build` | `target/doc/`, copied to `docs/book/book/api/` | Ignored build output | Published API reference |
| Feature matrix | Channel inventory, model-provider slots, default tools, and `docs/book/feature-matrix-parity.toml` | `xtask/src/cmd/mdbook/feature_matrix.rs` during locale builds | `docs/book/src/_snippets/feature-matrix-*.md` | Ignored derived snippets | Feature comparison pages through `{{#include}}` |
| Hardware tables | Hardware board registry and tool catalog, transport descriptions in the generator, release workflow targets, and the low-memory threshold in `install.sh` | `xtask/src/cmd/mdbook/hardware.rs` during locale builds | `docs/book/src/_snippets/hardware-*.md` | Ignored derived snippets | Hardware and release-target guides |
| Plugin contract values | WIT contracts, plugin guides, and `src/plugin_registry.rs` limits | `xtask/src/cmd/mdbook/plugins.rs` during locale builds | `docs/book/src/_snippets/plugin-*.md` | Ignored derived snippets | Plugin authoring guides |
| zerocode key tables | Default keymap in `apps/zerocode/src/keymap/actions.rs` | `xtask/src/cmd/mdbook/keymap.rs` during locale builds | `docs/book/src/_snippets/zerocode-*-keys.md` | Ignored derived snippets | zerocode keybinding pages |
| Peer-group blocks | `docs/book/peer-groups.toml` | `xtask/src/cmd/mdbook/peer_groups.rs` mdBook preprocessor | Expanded chapter content | Build-time only | Channel and peer-group pages using peer-group directives |
| Theme CSS and names | `web/src/contexts/themes.json` | `xtask/src/cmd/mdbook/themes.rs` during locale builds | Ignored CSS/name fragments plus the generated marker region in tracked `docs/book/theme/index.hbs` | Mixed: derived files are ignored; the template outside its marker remains authored | mdBook theme picker and zerocode theme reference |
| Locale switcher | `locales.toml` and tracked `docs/book/theme/lang-switcher.js.tpl` | `inject_lang_switcher_locales` during locale builds | `docs/book/theme/lang-switcher.js` | Ignored derived file | Published language selector |
| Authored chapters | `docs/book/src/**/*.md` and tracked snippets | mdBook preprocessors and renderers | Locale/version HTML under `docs/book/book/` | Tracked source, ignored output | Published documentation site |
| Dashboard API types | `zeroclaw_gateway::openapi::build_spec()` and gateway runtime types | `cargo web gen-api` | `target/openapi.json`, `web/src/lib/api-generated.ts`, `web/src/lib/api-descriptions.ts`, and `web/src/lib/api-enums.ts` | Ignored derived files | TypeScript dashboard build |

This matrix describes the current high-value surfaces, not every helper file produced during a build. The reusable rule is ownership: a generated value should have one canonical input and one deterministic materialization path.

## mdBook assembly order

`cargo mdbook` is the xtask command surface defined in `.cargo/config.toml`. Its main commands compose the pipeline rather than invoking a plain `mdbook build` directly.

`cargo mdbook refs` generates the CLI and config Markdown from live code, builds workspace rustdoc, and copies the API output into the docs build tree. `cargo mdbook build` performs the full publication-shaped sequence:

1. Generate `reference/cli.md` and `reference/config.md` from the current command tree and config schema.
2. Build workspace rustdoc.
3. Materialize theme, keymap, hardware, feature-matrix, and plugin snippets.
4. Run mdBook once for every locale in `locales.toml`, with preprocessors configured by `docs/book/book.toml`.
5. Check links in the rendered primary locale.
6. Assemble the version directory, locale redirect, rustdoc tree, and shared theme assets under `docs/book/book/`.

The peer-group preprocessor expands its directives while mdBook processes each chapter. Other standard mdBook preprocessors handle links, Mermaid blocks, and gettext localization. Generated references therefore need to exist before chapter preprocessing, while directive expansion and translation happen during the locale build.

The docs deployment workflow initializes the translation submodule, installs the required mdBook tools, runs `cargo mdbook build`, and merges the assembled version into the `gh-pages` branch. It does not call a translation provider or repair catalogs during deployment.

## Tracked and build-only outputs

Tracked files are reviewable inputs or templates: authored Markdown, `locales.toml`, `docs/book/peer-groups.toml`, feature-matrix parity metadata, theme templates, Rust/WIT sources, and workflow definitions. The `docs/book/po` path is a tracked gitlink to the separate translation-catalog repository; its contents and release tags have their own lifecycle.

Ignored files are reproducible materializations: CLI and config references, most generated snippets, rustdoc, rendered HTML, locale-switcher JavaScript, generated theme CSS, and the dashboard TypeScript API client. They may exist in a working tree after a docs build without belonging in a commit. The tracked installation surfaces are explicit exceptions: `docs/book/src/_snippets/install.md`, generated regions in `install.sh`, and the Windows prebuilt block in `docs/book/src/setup/windows.md`.

`docs/book/theme/index.hbs` is the notable mixed case. It is a tracked template, but the theme generator rewrites only the marked theme-list region from `themes.json`. If a standard generation command changes that region, inspect whether the canonical registry or generator changed; do not treat the generated list as independently authored content.

## Drift and validation gates

Different checks cover different failure classes:

| Check | What it proves | What it does not prove |
| --- | --- | --- |
| Docs quality gate | Changed Markdown passes prose em-dash policy and markdownlint | Ignored references or snippets were regenerated from current code |
| Added-link gate | New local Markdown links in the compared diff resolve | Existing links, generated links outside the diff, or rendered navigation all work |
| `cargo mdbook check` | PO catalogs parse and pass generated-response, protected-literal, and local-path audits | CLI/config references and ignored snippets match current Rust sources |
| `cargo mdbook refs` | CLI/config reference Markdown and rustdoc can be generated from current code | Every locale and theme assembles into a complete site |
| `cargo mdbook build` | Full references, snippets, locale builds, rendered links, and site assembly complete | Ignored outputs are committed or compared by ordinary PR CI |
| Translation pin workflow | The `docs/book/po` gitlink is initialized and satisfies the catalog-repository pin contract | Catalog coverage is complete or translation quality is acceptable |
| Docs deployment | The selected ref builds and can be merged into the versioned `gh-pages` layout | A normal source PR regenerated every ignored output before review |

Required PR CI runs the docs quality and added-link gates, but it does not run the full mdBook build for every documentation change. Reviewers should request the narrowest additional evidence that covers the changed generator or rendered boundary instead of assuming green prose checks prove generated output is current.

## Correction rules

- Fix config reference errors in the typed schema, derives, or schema-to-Markdown generator.
- Fix CLI reference errors in the Clap command definition or Markdown-help generator.
- Fix stable installation behavior in the typed route contract or its renderer, then run `cargo generate installers`; do not hand-edit the tracked installation snippet.
- Fix source-backed snippet errors in the owning registry, metadata file, contract, or snippet generator.
- Fix theme-list drift in `themes.json` or the marked-region generator, not by hand-editing generated buttons.
- Fix translated content or fallback behavior through the catalog lifecycle, not in rendered locale HTML.
- Never commit `docs/book/book/`, rustdoc output, or another ignored materialization merely to make a local build look current.
- When generator behavior itself changes, review both the source change and a representative regenerated output, then run the check that consumes that output.

## Source pointers

- mdBook command composition: `xtask/src/cmd/mdbook/`
- CLI and config references: `xtask/src/cmd/mdbook/refs.rs`
- Locale builds and site assembly: `xtask/src/cmd/mdbook/build.rs`
- mdBook preprocessor configuration: `docs/book/book.toml`
- Locale registry: `locales.toml`
- Docs quality and link gates: `scripts/ci/docs_quality_gate.sh`, `scripts/ci/docs_links_gate.sh`
- Translation pin validation: `.github/workflows/validate-translations-pin.yml`
- Docs deployment: `.github/workflows/docs-deploy.yml`
- Dashboard OpenAPI generation: [Building the web dashboard](../developing/web.md)
- Local build commands: [Building the docs locally](../developing/building-docs.md)
