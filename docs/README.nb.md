# ZeroClaw Dokumentasjonshub

Denne siden er hovedinngangen til dokumentasjonssystemet.

Sist oppdatert: **21. februar 2026**.

Lokaliserte huber: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Start her

| Jeg vil…                                                            | Les dette                                                                      |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Installere og kjøre ZeroClaw raskt                                  | [README.md (Hurtigstart)](../README.md#quick-start)                            |
| Bootstrap med en enkelt kommando                                    | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                  |
| Oppdatere eller avinstallere på macOS                               | [macos-update-uninstall.md](setup-guides/macos-update-uninstall.md)            |
| Finne kommandoer etter oppgave                                      | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Raskt sjekke konfigurasjonsstandarder og nøkler                     | [config-reference.md](reference/api/config-reference.md)                       |
| Konfigurere egendefinerte leverandører/endepunkter                  | [custom-providers.md](contributing/custom-providers.md)                        |
| Konfigurere Z.AI / GLM-leverandøren                                | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                             |
| Bruke LangGraph-integrasjonsmønstre                                | [langgraph-integration.md](contributing/langgraph-integration.md)              |
| Drifte kjøretidsmiljøet (dag 2-runbook)                             | [operations-runbook.md](ops/operations-runbook.md)                             |
| Feilsøke installasjon/kjøretid/kanal-problemer                     | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Kjøre Matrix-kryptert rom-oppsett og diagnostikk                   | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                          |
| Bla gjennom dokumentasjon etter kategori                            | [SUMMARY.md](SUMMARY.md)                                                      |
| Se prosjektets PR/issue-dokumentasjonsøyeblikksbilde                | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Raskt beslutningstre (10 sekunder)

- Trenger førstegangsoppsett eller installasjon? → [setup-guides/README.md](setup-guides/README.md)
- Trenger nøyaktige CLI/konfigurasjonsnøkler? → [reference/README.md](reference/README.md)
- Trenger produksjons-/tjenestedrift? → [ops/README.md](ops/README.md)
- Ser du feil eller regresjoner? → [troubleshooting.md](ops/troubleshooting.md)
- Jobber med sikkerhetsherding eller veikart? → [security/README.md](security/README.md)
- Jobber med kort/periferiutstyr? → [hardware/README.md](hardware/README.md)
- Bidrag/gjennomgang/CI-arbeidsflyt? → [contributing/README.md](contributing/README.md)
- Vil du ha det fullstendige kartet? → [SUMMARY.md](SUMMARY.md)

## Samlinger (Anbefalt)

- Kom i gang: [setup-guides/README.md](setup-guides/README.md)
- Referansekataloger: [reference/README.md](reference/README.md)
- Drift og utrulling: [ops/README.md](ops/README.md)
- Sikkerhetsdokumentasjon: [security/README.md](security/README.md)
- Maskinvare/periferiutstyr: [hardware/README.md](hardware/README.md)
- Bidrag/CI: [contributing/README.md](contributing/README.md)
- Prosjektøyeblikksbilder: [maintainers/README.md](maintainers/README.md)

## Etter målgruppe

### Brukere / Operatører

- [commands-reference.md](reference/cli/commands-reference.md) — kommandooppslag etter arbeidsflyt
- [providers-reference.md](reference/api/providers-reference.md) — leverandør-IDer, aliaser, legitimasjonsmiljøvariabler
- [channels-reference.md](reference/api/channels-reference.md) — kanalegenskaper og oppsettstier
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — Matrix kryptert rom (E2EE)-oppsett og diagnostikk for manglende svar
- [config-reference.md](reference/api/config-reference.md) — viktige konfigurasjonsnøkler og sikre standardverdier
- [custom-providers.md](contributing/custom-providers.md) — maler for egendefinert leverandør/basis-URL-integrasjon
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM-oppsett og endepunktmatrise
- [langgraph-integration.md](contributing/langgraph-integration.md) — reserveintegrasjon for modell/verktøykall-grensetilfeller
- [operations-runbook.md](ops/operations-runbook.md) — dag 2 kjøretidsdrift og tilbakestillingsflyt
- [troubleshooting.md](ops/troubleshooting.md) — vanlige feilsignaturer og gjenopprettingstrinn

### Bidragsytere / Vedlikeholdere

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Sikkerhet / Pålitelighet

> Merk: dette området inkluderer forslags-/veikartdokumenter. For nåværende oppførsel, start med [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) og [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Systemnavigasjon og styring

- Samlet innholdsfortegnelse: [SUMMARY.md](SUMMARY.md)
- Dokumentasjonsstrukturkart (språk/del/funksjon): [structure/README.md](maintainers/structure-README.md)
- Dokumentasjonsinventar/klassifisering: [docs-inventory.md](maintainers/docs-inventory.md)
- i18n-dokumentasjonsindeks: [i18n/README.md](i18n/README.md)
- i18n-dekningskart: [i18n-coverage.md](maintainers/i18n-coverage.md)
- Prosjekttriageringsøyeblikksbilde: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Andre språk

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
