# Translations

ZeroClaw has two independent translation layers — app strings (Mozilla Fluent `.ftl`) and
docs (gettext `.po`). Both are filled locally via a configured AI provider before opening
a PR; translation is never a CI operation.

Full contributor workflow: [`docs/book/src/maintainers/docs-and-translations.md`](docs/book/src/maintainers/docs-and-translations.md)

Quick reference:

```
# Fill docs translations (extract → merge → AI-fill)
cargo mdbook sync --provider <name>

# Fill app strings
cargo fluent fill --provider <name>

# Check coverage
cargo mdbook stats
cargo fluent stats
```

Ollama is the canonical local provider. See the full docs page for provider configuration,
batch size tuning, failure log inspection, and the self-healing startup repair pass.
