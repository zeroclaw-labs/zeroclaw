<!-- Canonical one-paragraph definition. Edit here; reuse via {{#include}}. -->
**Permission profile.** A permission profile is a named grant set: which
agents a principal may address, which config paths it may write, which tools
it may cause an agent to run, and which resource verbs (`create`, `read`,
`update`, `delete`, `execute`) it holds on each resource class. Profiles are
the single authorization vocabulary: OIDC `role_map` values and
`[users.<name>].permission_profile` both resolve to one. A profile grants
exactly what it lists; everything else is denied. In the config it lives at
`[permission_profiles.<alias>]`. See
[Security → Authentication](../security/authentication.md).
