# Building the docs locally

The docs site you're reading is published from `docs/book/`. You can build the same site on your own machine, useful for offline reading, previewing edits before opening a PR, or developing translations.

{{#include ../_snippets/docs-build-commands.md}}

`cargo mdbook` is an alias for `cargo run -p xtask --bin mdbook --` (defined in the cargo config).

## Translations

English markdown is the only source maintained by humans. Translations live in `docs/book/po/<locale>.po` files that act as a cache, and `cargo mdbook sync` keeps them current. Routine English docs PRs do not need to carry the generated `.po` churn: leave it for a dedicated translation-cache PR. For the full translation pipeline (app strings, docs, zerocode, adding a locale, release passes), see [Docs & Translations](../maintainers/docs-and-translations.md).

## Tips

- **Fast iteration on prose:** `cargo mdbook serve` auto-rebuilds on save. Skip `cargo mdbook refs` unless you've changed CLI flags or config schema.
- **Fast iteration on translations:** edit `po/<locale>.po` and reload the browser, mdbook serve detects `.po` changes and rebuilds automatically.
- **Cleaning up:** `rm -rf docs/book/book target/doc` removes everything generated.
- **Zero-cost re-runs:** `cargo mdbook sync` against unchanged English source completes in seconds, no AI calls, no cost.
