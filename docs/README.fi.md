# JhedaiClaw-dokumentaatiokeskus

Tämä sivu on dokumentaatiojärjestelmän ensisijainen aloituspiste.

Viimeksi päivitetty: **20. helmikuuta 2026**.

Lokalisoidut keskukset: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Aloita Tästä

| Haluan…                                                     | Lue tämä                                                                                   |
| ----------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Asentaa ja ajaa JhedaiClaw nopeasti                         | [README.md (Pikaopas)](../README.md#quick-start)                                           |
| Käynnistys yhdellä komennolla                               | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| Löytää komentoja tehtävän mukaan                            | [commands-reference.md](reference/cli/commands-reference.md)                               |
| Tarkistaa nopeasti asetusavaimet ja oletusarvot             | [config-reference.md](reference/api/config-reference.md)                                   |
| Määrittää mukautettuja tarjoajia/päätepisteitä              | [custom-providers.md](contributing/custom-providers.md)                                    |
| Määrittää Z.AI / GLM -tarjoajan                             | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| Käyttää LangGraph-integrointimalleja                        | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| Käyttää ajonaikaa (päivä-2 runbook)                         | [operations-runbook.md](ops/operations-runbook.md)                                         |
| Ratkaista asennus-/ajonaika-/kanavaongelmia                 | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Ajaa Matrix-salattujen huoneiden asetukset ja diagnostiikka | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| Selata dokumentaatiota kategorioittain                      | [SUMMARY.md](SUMMARY.md)                                                                   |
| Nähdä projektin PR/issue-dokumenttien tilannekuva           | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Nopea Päätöspuu (10 sekuntia)

- Tarvitsetko alkuasennuksen tai -määrityksen? → [setup-guides/README.md](setup-guides/README.md)
- Tarvitsetko tarkat CLI/asetusavaimet? → [reference/README.md](reference/README.md)
- Tarvitsetko tuotanto-/palvelutoimintoja? → [ops/README.md](ops/README.md)
- Näetkö virheitä tai regressioita? → [troubleshooting.md](ops/troubleshooting.md)
- Työskenteletkö tietoturvan koventamisen tai tiekartan parissa? → [security/README.md](security/README.md)
- Työskenteletkö levyjen/oheislaitteiden kanssa? → [hardware/README.md](hardware/README.md)
- Osallistuminen/katselmointi/CI-työnkulku? → [contributing/README.md](contributing/README.md)
- Haluatko täydellisen kartan? → [SUMMARY.md](SUMMARY.md)

## Kokoelmat (Suositellut)

- Aloitus: [setup-guides/README.md](setup-guides/README.md)
- Viiteluettelot: [reference/README.md](reference/README.md)
- Toiminta ja käyttöönotto: [ops/README.md](ops/README.md)
- Tietoturvadokumentit: [security/README.md](security/README.md)
- Laitteisto/oheislaitteet: [hardware/README.md](hardware/README.md)
- Osallistuminen/CI: [contributing/README.md](contributing/README.md)
- Projektin tilannekuvat: [maintainers/README.md](maintainers/README.md)

## Yleisön Mukaan

### Käyttäjät / Operaattorit

- [commands-reference.md](reference/cli/commands-reference.md) — komentojen haku työnkulun mukaan
- [providers-reference.md](reference/api/providers-reference.md) — tarjoajien tunnisteet, aliakset, tunnistetietojen ympäristömuuttujat
- [channels-reference.md](reference/api/channels-reference.md) — kanavien ominaisuudet ja asetuspolut
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — Matrix-salattujen huoneiden (E2EE) asetukset ja vastaamattomuuden diagnostiikka
- [config-reference.md](reference/api/config-reference.md) — korkean signaalin asetusavaimet ja turvalliset oletusarvot
- [custom-providers.md](contributing/custom-providers.md) — mukautetun tarjoajan/perus-URL:n integrointimallit
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM-asetukset ja päätepistematriisi
- [langgraph-integration.md](contributing/langgraph-integration.md) — varaintegrointi mallin/työkalukutsun reunatapauksille
- [operations-runbook.md](ops/operations-runbook.md) — ajonaikan päivä-2 toiminnot ja palautustyönkulut
- [troubleshooting.md](ops/troubleshooting.md) — yleiset virhesignatuurit ja palautusaskeleet

### Osallistujat / Ylläpitäjät

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Tietoturva / Luotettavuus

> Huomautus: tämä alue sisältää ehdotus-/tiekartadokumentteja. Nykyisestä toiminnasta aloita kohdista [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) ja [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Järjestelmänavigaatio & Hallintotapa

- Yhtenäinen sisällysluettelo: [SUMMARY.md](SUMMARY.md)
- Dokumenttien rakennekartta (kieli/osio/toiminto): [structure/README.md](maintainers/structure-README.md)
- Dokumentaation inventaario/luokittelu: [docs-inventory.md](maintainers/docs-inventory.md)
- Projektin lajittelun tilannekuva: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Muut kielet

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
