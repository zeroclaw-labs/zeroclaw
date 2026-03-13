# Sentro ng Dokumentasyon ng ZeroClaw

Ang pahinang ito ang pangunahing entry point ng sistema ng dokumentasyon.

Huling na-update: **Pebrero 21, 2026**.

Mga lokal na sentro: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Magsimula Dito

| Gusto ko…                                                           | Basahin ito                                                                    |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| I-install at patakbuhin ang ZeroClaw nang mabilis                    | [README.md (Mabilis na Pagsisimula)](../README.md#quick-start)                 |
| Bootstrap sa isang utos                                              | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| Hanapin ang mga utos ayon sa gawain                                  | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Mabilisang suriin ang mga config key at default na halaga             | [config-reference.md](reference/api/config-reference.md)                       |
| Mag-set up ng custom na provider/endpoint                            | [custom-providers.md](contributing/custom-providers.md)                         |
| I-set up ang Z.AI / GLM provider                                    | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| Gamitin ang mga pattern ng integrasyon ng LangGraph                  | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| Pamahalaan ang runtime (day-2 runbook)                               | [operations-runbook.md](ops/operations-runbook.md)                             |
| I-troubleshoot ang mga isyu sa pag-install/runtime/channel           | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Patakbuhin ang setup at diagnostics ng encrypted Matrix room         | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| I-browse ang mga dokumento ayon sa kategorya                         | [SUMMARY.md](SUMMARY.md)                                                       |
| Tingnan ang snapshot ng mga PR/issue ng proyekto                     | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Mabilisang Decision Tree (10 segundo)

- Kailangan ng setup o unang pag-install? → [setup-guides/README.md](setup-guides/README.md)
- Kailangan ng eksaktong CLI/config key? → [reference/README.md](reference/README.md)
- Kailangan ng production/service operations? → [ops/README.md](ops/README.md)
- May nakikitang pagkabigo o regression? → [troubleshooting.md](ops/troubleshooting.md)
- Nagtatrabaho sa security hardening o roadmap? → [security/README.md](security/README.md)
- Nagtatrabaho sa mga board/peripheral? → [hardware/README.md](hardware/README.md)
- Kontribusyon/review/CI workflow? → [contributing/README.md](contributing/README.md)
- Gusto mo ang buong mapa? → [SUMMARY.md](SUMMARY.md)

## Mga Koleksyon (Inirerekomenda)

- Pagsisimula: [setup-guides/README.md](setup-guides/README.md)
- Mga katalogo ng reference: [reference/README.md](reference/README.md)
- Operasyon at deployment: [ops/README.md](ops/README.md)
- Mga dokumento ng seguridad: [security/README.md](security/README.md)
- Hardware/peripheral: [hardware/README.md](hardware/README.md)
- Kontribusyon/CI: [contributing/README.md](contributing/README.md)
- Mga snapshot ng proyekto: [maintainers/README.md](maintainers/README.md)

## Ayon sa Audience

### Mga Gumagamit / Operator

- [commands-reference.md](reference/cli/commands-reference.md) — paghahanap ng utos ayon sa workflow
- [providers-reference.md](reference/api/providers-reference.md) — mga ID ng provider, alias, credential environment variable
- [channels-reference.md](reference/api/channels-reference.md) — mga kakayahan ng channel at landas ng configuration
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — setup ng encrypted Matrix room (E2EE) at diagnostics ng hindi pagtugon
- [config-reference.md](reference/api/config-reference.md) — mahahalagang config key at secure na default
- [custom-providers.md](contributing/custom-providers.md) — pattern ng integrasyon ng custom provider/base URL
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — setup ng Z.AI/GLM at endpoint matrix
- [langgraph-integration.md](contributing/langgraph-integration.md) — fallback na integrasyon para sa edge case ng model/tool call
- [operations-runbook.md](ops/operations-runbook.md) — day-2 runtime operations at rollback flow
- [troubleshooting.md](ops/troubleshooting.md) — karaniwang failure signature at mga hakbang sa pagbawi

### Mga Kontribyutor / Maintainer

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Seguridad / Pagiging Maaasahan

> Paalala: Kasama sa seksyong ito ang mga proposal/roadmap na dokumento. Para sa kasalukuyang gawi, magsimula sa [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), at [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Nabigasyon ng Sistema at Pamamahala

- Pinag-isang talaan ng nilalaman: [SUMMARY.md](SUMMARY.md)
- Mapa ng istruktura ng docs (wika/bahagi/function): [structure/README.md](maintainers/structure-README.md)
- Imbentaryo/klasipikasyon ng dokumentasyon: [docs-inventory.md](maintainers/docs-inventory.md)
- Snapshot ng triage ng proyekto: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Iba Pang Wika

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
