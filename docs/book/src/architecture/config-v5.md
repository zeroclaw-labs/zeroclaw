# Configuration V5 boundary

V4 was released with ZeroClaw 0.9.0 and is frozen. Its fixture and
`zeroclaw config generate 4` output remain unchanged. Connections, capability
models, presets and the fallback graph are V5 configuration.

The V4 → V5 migration converts legacy profiles and route-owned model settings
into independently named connections and models. Earlier V1–V3 inputs first
run the existing migrations through V4, then the same V5 conversion.
Runtime consumers resolve canonical references and never project old profiles.

Use `zeroclaw config migrate` to persist V5. Loading also admits older input
through the migration boundary, while offline edits, including the
`model_routing_config` agent tool, require the current V5 file. Versioned
backups preserve the old file; rollback to a V4 binary requires its
pre-upgrade configuration backup.
