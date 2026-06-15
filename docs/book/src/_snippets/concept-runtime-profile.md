<!-- Canonical one-paragraph definition. Edit here; reuse via {{#include}}. -->
**Runtime profile.** A runtime profile is reusable operational tuning: agentic
mode, tool-iteration caps, action and cost budgets, timeouts, context limits,
and delegation policy. It is separate from the risk profile, which governs
autonomy. Quickstart installs the `unbounded` preset for new agents; adjust the
fields afterward. In the config it lives at `[runtime_profiles.<alias>]`. See
[Reference → Config](../reference/config.md).
