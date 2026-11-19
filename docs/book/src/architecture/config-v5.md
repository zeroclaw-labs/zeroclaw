# Configuration V5 boundary

V4 was released with ZeroClaw 0.9.0 and is frozen. Its committed fixture and
`zeroclaw config generate 4` output remain unchanged. New configuration shapes
belong to V5 and the isolated V4 → V5 migration, not to V3 → V4.

The initial V5 step changes only the version stamp. Existing V1–V3 inputs run
the existing chain through V4 before entering V5. The provider registry layer
will extend the final step with its structural conversion.

Use `zeroclaw config migrate` to write V5. Migration retains versioned backups;
an older binary must use its pre-upgrade configuration rather than a V5 file.
Offline edits, including the `model_routing_config` agent tool, require the
current V5 file; migrate a released V4 file before using those paths.
