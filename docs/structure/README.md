# ZeroClaw Docs Structure Map

This page defines the canonical documentation layout and compatibility layers.

Last refreshed: **February 24, 2026**.

## 1) Directory Spine (Canonical)

### Layer A: global entry points

- Root product READMEs: `README.md`, `README.<locale>.md`
- Docs hub: `docs/README.md`
- Unified TOC: `docs/SUMMARY.md`

### Layer B: category collections (English source-of-truth)

- `docs/getting-started/`
- `docs/reference/`
- `docs/operations/`
- `docs/security/`
- `docs/hardware/`
- `docs/contributing/`
- `docs/project/`
- `docs/sop/`

### Layer C: canonical locale trees

- `docs/i18n/zh-CN/`
- `docs/i18n/ja/`
- `docs/i18n/ru/`
- `docs/i18n/fr/`
- `docs/i18n/vi/`
- `docs/i18n/el/`

### Layer D: compatibility shims (non-canonical)

- `docs/README.<locale>.md`, `docs/SUMMARY.<locale>.md`
- `docs/vi/**`
- legacy `docs/*.<locale>.md` files where present

Use compatibility paths for backward links only. New localized edits should target `docs/i18n/<locale>/**`.

## 2) Language Topology

| Locale | Root landing | Canonical docs hub | Coverage level | Notes |
|---|---|---|---|---|
| `en` | `README.md` | `docs/README.md` | Full source | Authoritative runtime-contract wording |
| `zh-CN` | `README.zh-CN.md` | `docs/i18n/zh-CN/README.md` | Hub-level scaffold | Runtime-contract docs mainly shared in English |
| `ja` | `README.ja.md` | `docs/i18n/ja/README.md` | Hub-level scaffold | Runtime-contract docs mainly shared in English |
| `ru` | `README.ru.md` | `docs/i18n/ru/README.md` | Hub-level scaffold | Runtime-contract docs mainly shared in English |
| `fr` | `README.fr.md` | `docs/i18n/fr/README.md` | Hub-level scaffold | Runtime-contract docs mainly shared in English |
| `vi` | `README.vi.md` | `docs/i18n/vi/README.md` | Full localized tree | `docs/vi/**` kept as compatibility layer |
| `el` | `README.el.md` | `docs/i18n/el/README.md` | Full localized tree | Greek full tree is canonical in `docs/i18n/el/**` |

## 3) Category Intent Map

| Category | Canonical index | Intent |
|---|---|---|
| Getting Started | `docs/getting-started/README.md` | first-run and install flows |
| Reference | `docs/reference/README.md` | commands/config/providers/channels and integration references |
| Operations | `docs/operations/README.md` | day-2 operations, release, troubleshooting runbooks |
| Security | `docs/security/README.md` | current hardening guidance + proposal boundary |
| Hardware | `docs/hardware/README.md` | boards, peripherals, datasheets navigation |
| Contributing | `docs/contributing/README.md` | PR/review/CI policy and process |
| Project | `docs/project/README.md` | time-bound snapshots and planning audit history |
| SOP | `docs/sop/README.md` | SOP runtime contract and procedure docs |

## 4) Placement Rules

1. Runtime behavior docs go in English canonical paths first.
2. Every new major doc must be linked from:
- the nearest category index (`docs/<category>/README.md`)
- `docs/SUMMARY.md`
- `docs/docs-inventory.md`
3. Locale navigation changes must update all supported locales (`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el`).
4. For localized hubs/summaries, canonical path is always `docs/i18n/<locale>/`.
5. Keep compatibility shims aligned when touched; do not introduce new primary content under compatibility-only paths.

## 5) Governance Links

- i18n docs index: [../i18n/README.md](../i18n/README.md)
- i18n coverage matrix: [../i18n-coverage.md](../i18n-coverage.md)
- i18n completion checklist: [../i18n-guide.md](../i18n-guide.md)
- i18n gap backlog: [../i18n-gap-backlog.md](../i18n-gap-backlog.md)
- docs inventory/classification: [../docs-inventory.md](../docs-inventory.md)
