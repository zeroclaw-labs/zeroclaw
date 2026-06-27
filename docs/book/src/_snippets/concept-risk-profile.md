<!-- Canonical one-paragraph definition. Edit here; reuse via {{#include}}. -->
**Risk profile.** A risk profile is a named autonomy and sandbox posture. Its
level is `readonly`, `supervised` (the default), or `full`, controlling whether
tools run automatically, prompt for approval, or are blocked. Each agent
references exactly one risk profile. In the config it lives at
`[risk_profiles.<alias>]`. See
[Security → Autonomy Levels](../security/autonomy.md).
