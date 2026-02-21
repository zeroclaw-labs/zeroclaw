# Presets Guide

Preset architecture for ZeroClaw onboarding and long-term package composition.

Last updated: **February 21, 2026**.

## Why presets exist

ZeroClaw is moving toward a **core-first** delivery model:

- Core binary remains lean.
- Optional capabilities are grouped into **feature packs**.
- Users choose packs (directly or via presets) during onboarding.
- Agent-assisted onboarding can infer intent and propose a preset composition.

This guide defines the operational contract for that flow, including import/export/share and contribution standards.

## Core concepts

- **Feature pack**: smallest selectable capability bundle (for example, `browser-native`, `tools-update`).
- **Preset**: curated list of packs that fit a workflow (for example, `minimal`, `hardware-lab`).
- **Composable preset**: user-defined preset assembled from one or more base presets plus overrides.
- **Risk-gated pack**: pack that may change runtime security posture or execute high-impact actions and therefore requires explicit user confirmation.

Canonical catalog source: [`src/onboard/feature_packs.rs`](../src/onboard/feature_packs.rs).

## Built-in pack and preset catalog

The built-in registry currently ships in code:

- Packs: `FEATURE_PACKS`
- Presets: `PRESETS`
- Lookup helpers: `feature_pack_by_id`, `preset_by_id`

Current notable packs:

- `core-agent`
- `hardware-core`
- `probe-rs`
- `browser-native`
- `tools-update`
- `rag-pdf`
- `sandbox-landlock`
- `peripheral-rpi`

Current built-in presets:

- `minimal`
- `default`
- `automation`
- `hardware-lab`
- `hardened-linux`

Official scenario mapping:

- [preset-recommendation-matrix.md](preset-recommendation-matrix.md)

## CLI workflow

Typical end-to-end flow:

1. Initialize workspace with official preset:
- `zeroclaw onboard --preset default`

2. Inspect presets and packs:
- `zeroclaw preset list`
- `zeroclaw preset show automation`

3. Evolve current selection:
- `zeroclaw preset apply --pack browser-native --remove-pack tools-update --dry-run`
- `zeroclaw preset apply --pack browser-native --remove-pack tools-update --yes-risky`
- `zeroclaw preset apply --pack browser-native --rebuild --yes-rebuild --yes-risky`

4. Intent-driven post-onboarding orchestration:
- `zeroclaw preset intent "我要最小体积但保留自动更新" --dry-run`
- `zeroclaw preset intent "need embedded debug with datasheet support" --apply --yes-risky`
- `zeroclaw preset intent "need browser automation but no update" --apply --rebuild --yes-rebuild`
- `zeroclaw preset intent "need webhook integration" --capabilities-file ./presets/capabilities/team.intent-capabilities.json --dry-run`

5. Export/import/share:
- `zeroclaw preset export ./team-automation.json`
- `zeroclaw preset import ./team-automation.json --mode merge --dry-run`
- `zeroclaw preset import ./team-automation.json --mode fill`
- `zeroclaw preset import ./team-automation.json --mode overwrite --rebuild --yes-rebuild --yes-risky`

6. Validate payloads before import/share:
- `zeroclaw preset validate ./presets/community`
- `zeroclaw preset validate ./presets/community --allow-unknown-packs`
- `zeroclaw preset validate ./presets/community --json`

7. Optional rebuild from selected packs:
- `zeroclaw preset rebuild --dry-run`
- `zeroclaw preset rebuild --yes`

## Import modes (required behavior)

When importing a preset from file/share payload, callers should expose these modes:

1. `overwrite`:
- Replace current workspace preset selection with imported content.
- Use when the source is authoritative.

2. `merge`:
- Merge imported packs into current selection.
- De-duplicate packs by pack id.
- Imported scalar fields (for example, preset id) win on conflicts.

3. `fill`:
- Fill only missing fields from imported payload.
- Existing scalar fields are preserved.
- Missing packs are appended and de-duplicated.
- Best for safe enrichment of local presets.

## Risk and consent model

Preset application must honor explicit consent for high-impact operations.

High-impact examples:

- Enabling risk-gated packs (for example, `tools-update`, sandbox policy changes)
- Operations that replace binaries, modify service units, or alter security-critical settings

Recommended guardrails:

- `dry-run` preview before apply
- explicit confirmation flags (`--yes-risky`, `--yes-rebuild`, or tool-level `approved=true`)
- clear diff output for changed packs and config fields

The `self_update` tool and `zeroclaw update --apply` already follow this pattern by requiring explicit approval/confirmation.

## Export and share format

Preset payloads should be portable, deterministic, and secret-free.

Recommended JSON shape:

```json
{
  "schema_version": 1,
  "id": "team-automation",
  "title": "Team Automation",
  "description": "Browser + scheduling + update workflows",
  "packs": ["core-agent", "browser-native", "tools-update"],
  "config_overrides": {
    "autonomy": {
      "level": "supervised"
    }
  },
  "metadata": {
    "author": "team-platform",
    "created_at": "2026-02-21T00:00:00Z"
  }
}
```

Export rules:

- Never include credentials or secrets.
- Prefer stable key ordering for diffability.
- Include schema version for forward compatibility.

Share rules:

- Prefer short-lived signed links or explicit file exchange.
- Receiver chooses import mode (`overwrite` / `merge` / `fill`).

## Agent-driven preset orchestration

Scope boundary:

- During onboarding: user picks an official preset, then optionally adds packs.
- Post-onboarding: natural-language intent can drive re-composition/rebuild workflows.

Current implementation uses a capability graph (not provider-dependent inference) with:

- capability detection (`capability_signals`)
- preset ranking (`preset_ranking`)
- confidence decomposition (`confidence_breakdown`)

### Hot-plug capability graph

Intent capability rules are hot-pluggable and merged in this order:

1. built-in capability graph in `src/presets.rs`
2. workspace file: `.zeroclaw-intent-capabilities.json`
3. env file: `ZEROCLAW_INTENT_CAPABILITIES_FILE`
4. CLI overlays: `--capabilities-file <path>` (repeatable)

Merge behavior:

- `append` (default): merge/override by `id`, keep unspecified built-ins
- `replace`: clear previous rules and replace with current file rules

Document shape:

```json
{
  "schema_version": 1,
  "merge_mode": "append",
  "capabilities": [
    {
      "id": "webhook-focus",
      "rationale": "Prioritize webhook-first automation workflows",
      "keywords": ["webhook", "callback", "event push"],
      "add_packs": ["browser-native"],
      "remove_packs": ["tools-update"],
      "preset_biases": {
        "automation": 0.8,
        "default": 0.3
      },
      "base_weight": 0.7,
      "enabled": true
    }
  ]
}
```

Template starter:

- [`../presets/capabilities/template.intent-capabilities.json`](../presets/capabilities/template.intent-capabilities.json)

When users describe intent in natural language, agent orchestration should:

1. Parse intent into capability requirements.
2. Map requirements to packs.
3. Select nearest built-in preset as base.
4. Compose additional packs and config overrides.
5. Show plan + risk summary.
6. Require explicit consent for risky changes.
7. Apply and verify.

Intent examples:

- "I need embedded debugging" → `hardware-lab` baseline + `probe-rs`
- "I want smallest footprint" → `minimal`
- "I need browser automation and safe update controls" → `automation` + `tools-update`

Typical plan output layers:

1. Intent directives (`add/remove packs`)
2. Capability graph matches + matched terms
3. Preset ranking candidates (top scored bases)
4. Confidence breakdown (`base + signal + ranking - penalty`)

## Contributor guide: adding packs/presets

### 1) Add catalog entries

Edit [`src/onboard/feature_packs.rs`](../src/onboard/feature_packs.rs):

- add/update `FEATURE_PACKS`
- add/update `PRESETS`
- keep IDs stable and human-readable

### 2) Preserve quality constraints

- pack IDs must be unique
- preset IDs must be unique
- every preset pack reference must resolve to an existing pack
- mark risky packs with `requires_confirmation = true`

These constraints are enforced by unit tests in `feature_packs.rs`.

### 3) Document behavior changes

Update at least:

- [`docs/commands-reference.md`](commands-reference.md) when CLI surface changes
- [`docs/README.md`](README.md)
- [`docs/SUMMARY.md`](SUMMARY.md)
- [`docs/docs-inventory.md`](docs-inventory.md)

### 4) Validate before PR

```bash
cargo fmt --all
cargo check --locked
cargo test --locked onboard::feature_packs
zeroclaw preset validate presets/community/template.preset.json
```

### 5) Community preset template and validation

- Starter template: [`../presets/community/template.preset.json`](../presets/community/template.preset.json)
- Community contribution notes: [`../presets/community/README.md`](../presets/community/README.md)
- Optional validator script (for CI helpers): [`../scripts/validate_preset_payload.py`](../scripts/validate_preset_payload.py)

Validate a candidate preset:

```bash
zeroclaw preset validate presets/community/my-team-automation.json
```

## Preset PR checklist

- [ ] New pack/preset IDs are unique and descriptive.
- [ ] Risk-gated packs are explicitly marked.
- [ ] Import mode behavior documented for new fields.
- [ ] No secrets in export/share examples.
- [ ] Docs index links updated.
- [ ] Tests pass locally.
