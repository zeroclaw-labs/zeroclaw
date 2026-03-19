# JhedaiClaw Documentatiehub

Deze pagina is het primaire toegangspunt voor het documentatiesysteem.

Laatst bijgewerkt: **20 februari 2026**.

Gelokaliseerde hubs: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Begin Hier

| Ik wil…                                                    | Lees dit                                                                                   |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| JhedaiClaw snel installeren en uitvoeren                   | [README.md (Snelle Start)](../README.md#quick-start)                                       |
| Bootstrap met één commando                                 | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| Commando's zoeken op taak                                  | [commands-reference.md](reference/cli/commands-reference.md)                               |
| Snel configuratiesleutels en standaardwaarden controleren  | [config-reference.md](reference/api/config-reference.md)                                   |
| Aangepaste providers/endpoints configureren                | [custom-providers.md](contributing/custom-providers.md)                                    |
| Z.AI / GLM-provider instellen                              | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| LangGraph-integratiepatronen gebruiken                     | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| De runtime beheren (dag-2 runbook)                         | [operations-runbook.md](ops/operations-runbook.md)                                         |
| Installatie-/runtime-/kanaalproblemen oplossen             | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Matrix versleutelde ruimtes configureren en diagnosticeren | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| Documentatie per categorie bekijken                        | [SUMMARY.md](SUMMARY.md)                                                                   |
| Docs-momentopname van project-PR's/issues bekijken         | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Snelle Beslisboom (10 seconden)

- Eerste installatie of configuratie nodig? → [setup-guides/README.md](setup-guides/README.md)
- Exacte CLI-/configuratiesleutels nodig? → [reference/README.md](reference/README.md)
- Productie-/servicebeheer nodig? → [ops/README.md](ops/README.md)
- Fouten of regressies? → [troubleshooting.md](ops/troubleshooting.md)
- Bezig met beveiligingsverharding of roadmap? → [security/README.md](security/README.md)
- Werken met boards/randapparatuur? → [hardware/README.md](hardware/README.md)
- Bijdrage/review/CI-workflow? → [contributing/README.md](contributing/README.md)
- De volledige kaart bekijken? → [SUMMARY.md](SUMMARY.md)

## Collecties (Aanbevolen)

- Aan de slag: [setup-guides/README.md](setup-guides/README.md)
- Referentiecatalogi: [reference/README.md](reference/README.md)
- Beheer & implementatie: [ops/README.md](ops/README.md)
- Beveiligingsdocs: [security/README.md](security/README.md)
- Hardware/randapparatuur: [hardware/README.md](hardware/README.md)
- Bijdrage/CI: [contributing/README.md](contributing/README.md)
- Projectmomentopnamen: [maintainers/README.md](maintainers/README.md)

## Per Doelgroep

### Gebruikers / Beheerders

- [commands-reference.md](reference/cli/commands-reference.md) — commando's zoeken op workflow
- [providers-reference.md](reference/api/providers-reference.md) — provider-ID's, aliassen, omgevingsvariabelen voor inloggegevens
- [channels-reference.md](reference/api/channels-reference.md) — kanaalmogelijkheden en configuratiepaden
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — Matrix versleutelde ruimtes (E2EE) instellen en diagnostiek bij geen reactie
- [config-reference.md](reference/api/config-reference.md) — configuratiesleutels met hoog belang en veilige standaardwaarden
- [custom-providers.md](contributing/custom-providers.md) — integratie-patronen voor aangepaste providers/basis-URL
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM-configuratie en endpointmatrix
- [langgraph-integration.md](contributing/langgraph-integration.md) — fallback-integratie voor model-/toolaanroep-randgevallen
- [operations-runbook.md](ops/operations-runbook.md) — dag-2 runtime-operaties en rollbackflows
- [troubleshooting.md](ops/troubleshooting.md) — veelvoorkomende foutpatronen en herstelstappen

### Bijdragers / Beheerders

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Beveiliging / Betrouwbaarheid

> Opmerking: dit gedeelte bevat voorstel-/roadmapdocumenten. Voor het huidige gedrag, begin met [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) en [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Systeemnavigatie & Governance

- Uniforme inhoudsopgave: [SUMMARY.md](SUMMARY.md)
- Documentatiestructuurkaart (taal/deel/functie): [structure/README.md](maintainers/structure-README.md)
- Documentatie-inventaris/-classificatie: [docs-inventory.md](maintainers/docs-inventory.md)
- Projecttriage-momentopname: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Andere talen

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
