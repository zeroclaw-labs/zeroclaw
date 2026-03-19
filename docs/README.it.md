# Hub della Documentazione JhedaiClaw

Questa pagina è il punto di ingresso principale del sistema di documentazione.

Ultimo aggiornamento: **21 febbraio 2026**.

Hub localizzati: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Inizia Qui

| Voglio…                                                                 | Leggi questo                                                                               |
| ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Installare ed eseguire JhedaiClaw rapidamente                           | [README.md (Avvio Rapido)](../README.md#quick-start)                                       |
| Bootstrap con un singolo comando                                        | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| Aggiornare o disinstallare su macOS                                     | [macos-update-uninstall.md](setup-guides/macos-update-uninstall.md)                        |
| Trovare comandi per attività                                            | [commands-reference.md](reference/cli/commands-reference.md)                               |
| Controllare rapidamente valori predefiniti e chiavi di configurazione   | [config-reference.md](reference/api/config-reference.md)                                   |
| Configurare provider/endpoint personalizzati                            | [custom-providers.md](contributing/custom-providers.md)                                    |
| Configurare il provider Z.AI / GLM                                      | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| Usare i pattern di integrazione LangGraph                               | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| Gestire il runtime (runbook giorno 2)                                   | [operations-runbook.md](ops/operations-runbook.md)                                         |
| Risolvere problemi di installazione/runtime/canale                      | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Eseguire configurazione e diagnostica delle stanze crittografate Matrix | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| Sfogliare la documentazione per categoria                               | [SUMMARY.md](SUMMARY.md)                                                                   |
| Vedere lo snapshot dei documenti PR/issue del progetto                  | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Albero Decisionale Rapido (10 secondi)

- Serve configurazione o installazione iniziale? → [setup-guides/README.md](setup-guides/README.md)
- Servono chiavi CLI/configurazione esatte? → [reference/README.md](reference/README.md)
- Servono operazioni di produzione/servizio? → [ops/README.md](ops/README.md)
- Si verificano errori o regressioni? → [troubleshooting.md](ops/troubleshooting.md)
- Si lavora sul rafforzamento della sicurezza o sulla roadmap? → [security/README.md](security/README.md)
- Si lavora con schede/periferiche? → [hardware/README.md](hardware/README.md)
- Contribuzione/revisione/workflow CI? → [contributing/README.md](contributing/README.md)
- Vuoi la mappa completa? → [SUMMARY.md](SUMMARY.md)

## Collezioni (Raccomandate)

- Per iniziare: [setup-guides/README.md](setup-guides/README.md)
- Cataloghi di riferimento: [reference/README.md](reference/README.md)
- Operazioni e deployment: [ops/README.md](ops/README.md)
- Documentazione sulla sicurezza: [security/README.md](security/README.md)
- Hardware/periferiche: [hardware/README.md](hardware/README.md)
- Contribuzione/CI: [contributing/README.md](contributing/README.md)
- Snapshot del progetto: [maintainers/README.md](maintainers/README.md)

## Per Pubblico

### Utenti / Operatori

- [commands-reference.md](reference/cli/commands-reference.md) — ricerca comandi per workflow
- [providers-reference.md](reference/api/providers-reference.md) — ID provider, alias, variabili d'ambiente per le credenziali
- [channels-reference.md](reference/api/channels-reference.md) — capacità dei canali e percorsi di configurazione
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — configurazione stanze crittografate Matrix (E2EE) e diagnostica mancata risposta
- [config-reference.md](reference/api/config-reference.md) — chiavi di configurazione importanti e valori predefiniti sicuri
- [custom-providers.md](contributing/custom-providers.md) — template di integrazione provider personalizzato/URL base
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — configurazione Z.AI/GLM e matrice degli endpoint
- [langgraph-integration.md](contributing/langgraph-integration.md) — integrazione di fallback per casi limite modello/chiamata strumenti
- [operations-runbook.md](ops/operations-runbook.md) — operazioni runtime giorno 2 e flusso di rollback
- [troubleshooting.md](ops/troubleshooting.md) — firme di errore comuni e passaggi di ripristino

### Contributori / Manutentori

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Sicurezza / Affidabilità

> Nota: quest'area include documenti di proposta/roadmap. Per il comportamento attuale, iniziare con [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) e [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Navigazione di Sistema e Governance

- Indice unificato: [SUMMARY.md](SUMMARY.md)
- Mappa della struttura documentale (lingua/parte/funzione): [structure/README.md](maintainers/structure-README.md)
- Inventario/classificazione della documentazione: [docs-inventory.md](maintainers/docs-inventory.md)
- Indice documentazione i18n: [i18n/README.md](i18n/README.md)
- Mappa di copertura i18n: [i18n-coverage.md](maintainers/i18n-coverage.md)
- Snapshot di triage del progetto: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Altre lingue

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
