# JhedaiClaw Dokumentationshub

Denne side er det primære indgangspunkt til dokumentationssystemet.

Sidst opdateret: **20. februar 2026**.

Lokaliserede hubs: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Start her

| Jeg vil…                                               | Læs dette                                                                                  |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| Hurtigt installere og køre JhedaiClaw                  | [README.md (Hurtig start)](../README.md#quick-start)                                       |
| Bootstrap med én kommando                              | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| Finde kommandoer efter opgave                          | [commands-reference.md](reference/cli/commands-reference.md)                               |
| Hurtigt tjekke konfigurationsnøgler og standardværdier | [config-reference.md](reference/api/config-reference.md)                                   |
| Opsætte brugerdefinerede udbydere/endpoints            | [custom-providers.md](contributing/custom-providers.md)                                    |
| Opsætte Z.AI / GLM-udbyderen                           | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| Bruge LangGraph-integrationsmønstre                    | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| Drifte runtime (driftshåndbog)                         | [operations-runbook.md](ops/operations-runbook.md)                                         |
| Fejlfinde installations-/runtime-/kanalproblemer       | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Køre opsætning og diagnostik for krypterede Matrix-rum | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| Gennemse dokumentation efter kategori                  | [SUMMARY.md](SUMMARY.md)                                                                   |
| Se projektets PR/issue-dokumentationssnapshot          | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Hurtigt beslutningstræ (10 sekunder)

- Har du brug for opsætning eller førstegangsinstallation? → [setup-guides/README.md](setup-guides/README.md)
- Har du brug for præcise CLI/konfigurationsnøgler? → [reference/README.md](reference/README.md)
- Har du brug for produktions-/servicedrift? → [ops/README.md](ops/README.md)
- Ser du fejl eller regressioner? → [troubleshooting.md](ops/troubleshooting.md)
- Arbejder du på sikkerhedshærdning eller roadmap? → [security/README.md](security/README.md)
- Arbejder du med boards/periferienheder? → [hardware/README.md](hardware/README.md)
- Bidrag/review/CI-workflow? → [contributing/README.md](contributing/README.md)
- Vil du se det fulde kort? → [SUMMARY.md](SUMMARY.md)

## Samlinger (anbefalet)

- Kom i gang: [setup-guides/README.md](setup-guides/README.md)
- Referencekataloger: [reference/README.md](reference/README.md)
- Drift og udrulning: [ops/README.md](ops/README.md)
- Sikkerhedsdokumentation: [security/README.md](security/README.md)
- Hardware/periferienheder: [hardware/README.md](hardware/README.md)
- Bidrag/CI: [contributing/README.md](contributing/README.md)
- Projektsnapshots: [maintainers/README.md](maintainers/README.md)

## Efter målgruppe

### Brugere / Operatører

- [commands-reference.md](reference/cli/commands-reference.md) — kommandoopslag efter workflow
- [providers-reference.md](reference/api/providers-reference.md) — udbyder-ID'er, aliaser, legitimationsoplysningers miljøvariabler
- [channels-reference.md](reference/api/channels-reference.md) — kanalegenskaber og konfigurationsstier
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — opsætning af krypterede Matrix-rum (E2EE) og diagnostik ved manglende svar
- [config-reference.md](reference/api/config-reference.md) — vigtige konfigurationsnøgler og sikre standardværdier
- [custom-providers.md](contributing/custom-providers.md) — integrationsmønstre for brugerdefineret udbyder/base-URL
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM-opsætning og endpoint-matrix
- [langgraph-integration.md](contributing/langgraph-integration.md) — fallback-integration for model/tool-call-edgecases
- [operations-runbook.md](ops/operations-runbook.md) — daglig runtime-drift og rollback-flows
- [troubleshooting.md](ops/troubleshooting.md) — almindelige fejlsignaturer og genoprettelsestrin

### Bidragydere / Vedligeholdere

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Sikkerhed / Pålidelighed

> Bemærk: dette afsnit inkluderer forslags-/roadmap-dokumenter. For aktuel adfærd, start med [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) og [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Systemnavigation og governance

- Samlet indholdsfortegnelse: [SUMMARY.md](SUMMARY.md)
- Dokumentationsstrukturkort (sprog/del/funktion): [structure/README.md](maintainers/structure-README.md)
- Dokumentationsinventar/-klassificering: [docs-inventory.md](maintainers/docs-inventory.md)
- Projekt-triage-snapshot: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Andre sprog

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
