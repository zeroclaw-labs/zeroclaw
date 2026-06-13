<!-- Canonical one-paragraph definition. Edit here; reuse via {{#include}}. -->
**Model provider.** A model provider is ZeroClaw's abstraction over an LLM
endpoint. Every chat-completion request goes through a provider, whether the
target is a remote API, a self-hosted server, or a local Ollama model.
Providers are typed by vendor family, and you can run several named instances
of the same family. In the config each one lives at
`[providers.models.<type>.<alias>]`. See
[Model Providers → Overview](../providers/overview.md).
