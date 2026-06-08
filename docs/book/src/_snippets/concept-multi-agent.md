<!-- Canonical one-paragraph definition. Edit here; reuse via {{#include}}. -->
**Multi-agent.** ZeroClaw runs many agents from one install. Each agent has its
own set of references (risk profile, model provider, channels), its own
workspace directory, and its own memory backend. An agent can spawn an
ephemeral **SubAgent** that inherits its parent's identity and security policy,
and agents can talk to each other when they share a **peer group**. In the
config each agent is an `[agents.<alias>]` block. See
[Architecture → Multi-agent runtime](../architecture/multi-agent.md).
