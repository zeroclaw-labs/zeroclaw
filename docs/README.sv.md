# ZeroClaw Dokumentationshubb

Denna sida är den primära ingångspunkten för dokumentationssystemet.

Senast uppdaterad: **20 februari 2026**.

Lokaliserade hubbar: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Börja Här

| Jag vill…                                                           | Läs detta                                                                      |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Installera och köra ZeroClaw snabbt                                 | [README.md (Snabbstart)](../README.md#quick-start)                             |
| Bootstrap med ett enda kommando                                     | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| Hitta kommandon efter uppgift                                       | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Snabbt kontrollera konfigurationsnycklar och standardvärden         | [config-reference.md](reference/api/config-reference.md)                       |
| Konfigurera anpassade leverantörer/endpoints                        | [custom-providers.md](contributing/custom-providers.md)                         |
| Konfigurera Z.AI / GLM-leverantören                                 | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| Använda LangGraph-integrationsmönster                               | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| Hantera runtime (dag-2 runbook)                                     | [operations-runbook.md](ops/operations-runbook.md)                             |
| Felsöka installations-/runtime-/kanalproblem                        | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Konfigurera och diagnostisera krypterade Matrix-rum                 | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| Bläddra i dokumentation efter kategori                              | [SUMMARY.md](SUMMARY.md)                                                       |
| Se dokumentationsöversikt för projektets PR:er/issues               | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Snabbt Beslutsträd (10 sekunder)

- Behöver initial installation eller konfiguration? → [setup-guides/README.md](setup-guides/README.md)
- Behöver exakta CLI-/konfigurationsnycklar? → [reference/README.md](reference/README.md)
- Behöver produktions-/tjänsteoperationer? → [ops/README.md](ops/README.md)
- Ser du fel eller regressioner? → [troubleshooting.md](ops/troubleshooting.md)
- Arbetar med säkerhetshärdning eller färdplan? → [security/README.md](security/README.md)
- Arbetar med kort/kringutrustning? → [hardware/README.md](hardware/README.md)
- Bidrag/granskning/CI-arbetsflöde? → [contributing/README.md](contributing/README.md)
- Vill du se hela kartan? → [SUMMARY.md](SUMMARY.md)

## Samlingar (Rekommenderade)

- Kom igång: [setup-guides/README.md](setup-guides/README.md)
- Referenskataloger: [reference/README.md](reference/README.md)
- Drift och driftsättning: [ops/README.md](ops/README.md)
- Säkerhetsdokumentation: [security/README.md](security/README.md)
- Hårdvara/kringutrustning: [hardware/README.md](hardware/README.md)
- Bidrag/CI: [contributing/README.md](contributing/README.md)
- Projektögonblicksbilder: [maintainers/README.md](maintainers/README.md)

## Per Målgrupp

### Användare / Operatörer

- [commands-reference.md](reference/cli/commands-reference.md) — sök kommandon efter arbetsflöde
- [providers-reference.md](reference/api/providers-reference.md) — leverantörs-ID:n, alias, miljövariabler för autentiseringsuppgifter
- [channels-reference.md](reference/api/channels-reference.md) — kanalkapaciteter och konfigurationsvägar
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — konfiguration av krypterade Matrix-rum (E2EE) och diagnostik vid uteblivet svar
- [config-reference.md](reference/api/config-reference.md) — konfigurationsnycklar med hög signalstyrka och säkra standardvärden
- [custom-providers.md](contributing/custom-providers.md) — integrationsmönster för anpassad leverantör/bas-URL
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM-konfiguration och endpointmatris
- [langgraph-integration.md](contributing/langgraph-integration.md) — reservintegration för modell-/verktygsanropsspecialfall
- [operations-runbook.md](ops/operations-runbook.md) — dag-2 runtime-operationer och rollback-flöden
- [troubleshooting.md](ops/troubleshooting.md) — vanliga felmönster och återställningssteg

### Bidragsgivare / Underhållare

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Säkerhet / Tillförlitlighet

> Observera: denna sektion innehåller förslags-/färdplansdokument. För aktuellt beteende, börja med [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) och [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Systemnavigering och Styrning

- Enhetlig innehållsförteckning: [SUMMARY.md](SUMMARY.md)
- Dokumentationsstrukturkarta (språk/del/funktion): [structure/README.md](maintainers/structure-README.md)
- Dokumentationsinventering/-klassificering: [docs-inventory.md](maintainers/docs-inventory.md)
- Projekttriageringsögonblicksbild: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Andra språk

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
