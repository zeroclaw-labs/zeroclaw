# Centrul de Documentație ZeroClaw

Această pagină este punctul de intrare principal al sistemului de documentație.

Ultima actualizare: **20 februarie 2026**.

Centre localizate: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Începeți Aici

| Vreau să…                                                           | Citiți aceasta                                                                 |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Instalez și rulez ZeroClaw rapid                                    | [README.md (Start Rapid)](../README.md#quick-start)                            |
| Bootstrap cu o singură comandă                                      | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| Găsesc comenzi după sarcină                                         | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Verific rapid cheile de configurare și valorile implicite           | [config-reference.md](reference/api/config-reference.md)                       |
| Configurez furnizori/endpoint-uri personalizate                     | [custom-providers.md](contributing/custom-providers.md)                         |
| Configurez furnizorul Z.AI / GLM                                    | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| Folosesc modelele de integrare LangGraph                            | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| Administrez runtime-ul (runbook ziua-2)                             | [operations-runbook.md](ops/operations-runbook.md)                             |
| Depanez probleme de instalare/runtime/canal                         | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Configurez și diagnostichez camerele criptate Matrix                | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| Navighez documentația pe categorii                                  | [SUMMARY.md](SUMMARY.md)                                                       |
| Văd instantaneul documentației PR-urilor/issue-urilor proiectului   | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Arbore de Decizie Rapid (10 secunde)

- Aveți nevoie de instalare sau configurare inițială? → [setup-guides/README.md](setup-guides/README.md)
- Aveți nevoie de chei CLI/configurare exacte? → [reference/README.md](reference/README.md)
- Aveți nevoie de operațiuni de producție/serviciu? → [ops/README.md](ops/README.md)
- Vedeți erori sau regresii? → [troubleshooting.md](ops/troubleshooting.md)
- Lucrați la consolidarea securității sau foaia de parcurs? → [security/README.md](security/README.md)
- Lucrați cu plăci/periferice? → [hardware/README.md](hardware/README.md)
- Contribuție/recenzie/workflow CI? → [contributing/README.md](contributing/README.md)
- Doriți harta completă? → [SUMMARY.md](SUMMARY.md)

## Colecții (Recomandate)

- Primii pași: [setup-guides/README.md](setup-guides/README.md)
- Cataloage de referință: [reference/README.md](reference/README.md)
- Operațiuni și implementare: [ops/README.md](ops/README.md)
- Documentație de securitate: [security/README.md](security/README.md)
- Hardware/periferice: [hardware/README.md](hardware/README.md)
- Contribuție/CI: [contributing/README.md](contributing/README.md)
- Instantanee ale proiectului: [maintainers/README.md](maintainers/README.md)

## După Public

### Utilizatori / Operatori

- [commands-reference.md](reference/cli/commands-reference.md) — căutare comenzi după workflow
- [providers-reference.md](reference/api/providers-reference.md) — ID-uri furnizori, aliasuri, variabile de mediu pentru acreditări
- [channels-reference.md](reference/api/channels-reference.md) — capacitățile canalelor și căile de configurare
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — configurarea camerelor criptate Matrix (E2EE) și diagnosticarea lipsei de răspuns
- [config-reference.md](reference/api/config-reference.md) — chei de configurare cu semnal ridicat și valori implicite sigure
- [custom-providers.md](contributing/custom-providers.md) — modele de integrare furnizor personalizat/URL de bază
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — configurare Z.AI/GLM și matricea endpoint-urilor
- [langgraph-integration.md](contributing/langgraph-integration.md) — integrare de rezervă pentru cazurile limită ale modelului/apelului de instrumente
- [operations-runbook.md](ops/operations-runbook.md) — operațiuni runtime ziua-2 și fluxuri de rollback
- [troubleshooting.md](ops/troubleshooting.md) — semnături de erori comune și pași de recuperare

### Contribuitori / Întreținători

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Securitate / Fiabilitate

> Notă: această secțiune include documente de propunere/foaie de parcurs. Pentru comportamentul actual, începeți cu [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) și [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Navigare în Sistem și Guvernanță

- Cuprins unificat: [SUMMARY.md](SUMMARY.md)
- Harta structurii documentației (limbă/parte/funcție): [structure/README.md](maintainers/structure-README.md)
- Inventar/clasificare a documentației: [docs-inventory.md](maintainers/docs-inventory.md)
- Instantaneu de triaj al proiectului: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Alte limbi

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
