# Translations

ZeroClaw has three independent translation layers — runtime app strings (Mozilla
Fluent `.ftl`), web dashboard strings (TypeScript locale modules), and docs
(gettext `.po`). Fluent and docs translations are filled locally via a configured
AI provider before opening a PR; translation is never a CI operation.

Full contributor workflow: [`docs/book/src/maintainers/docs-and-translations.md`](docs/book/src/maintainers/docs-and-translations.md)

Quick reference:

```
# Fill docs translations (extract → merge → AI-fill)
cargo mdbook sync --provider <name>

# Fill app strings
cargo fluent fill --provider <name>

# Edit web dashboard strings
$EDITOR web/src/lib/i18n/locales/<locale>.ts

# Check coverage
cargo mdbook stats
cargo fluent stats
```

Ollama is the canonical local provider for generated translations. See the full docs
page for provider configuration, batch size tuning, failure log inspection, and the
self-healing startup repair pass. Web dashboard strings are currently curated in
`web/src/lib/i18n/locales/*.ts`; config-field labels, descriptions, and placeholders
use `config.label.*`, `config.description.*`, and `config.placeholder.*` keys with
English/schema fallback.
