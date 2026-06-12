<!-- Canonical one-paragraph definition. Edit here; reuse via {{#include}}. -->
**Alias.** An alias is the name you assign to a configured instance, then
reference elsewhere to point at it. You choose the name freely; other parts of
the config wire things together by that name. Aliases are lowercase ASCII
letters, digits, and single underscores, must start and end with a letter or
digit, and cannot contain `__` or hyphens. In the config they appear as the
`<alias>` segment of a section header, such as `[agents.<alias>]` or
`[providers.models.<type>.<alias>]`. See
[Reference → Environment variables → Alias grammar](../reference/env-vars.md#alias-grammar).
