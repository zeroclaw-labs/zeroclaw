# JhedaiClaw Dokumentations-Hub

Diese Seite ist der zentrale Einstiegspunkt in das Dokumentationssystem.

Zuletzt aktualisiert: **20. Februar 2026**.

Lokalisierte Hubs: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Hier starten

| Ich möchte…                                                   | Dies lesen                                                                                 |
| ------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| JhedaiClaw schnell installieren und starten                   | [README.md (Schnellstart)](../README.md#quick-start)                                       |
| Bootstrap mit einem Befehl                                    | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| Befehle nach Aufgabe finden                                   | [commands-reference.md](reference/cli/commands-reference.md)                               |
| Schnell Konfigurationsschlüssel und Standardwerte prüfen      | [config-reference.md](reference/api/config-reference.md)                                   |
| Benutzerdefinierte Anbieter/Endpunkte einrichten              | [custom-providers.md](contributing/custom-providers.md)                                    |
| Den Z.AI / GLM-Anbieter einrichten                            | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| LangGraph-Integrationsmuster verwenden                        | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| Die Laufzeitumgebung betreiben (Betriebshandbuch)             | [operations-runbook.md](ops/operations-runbook.md)                                         |
| Installations-/Laufzeit-/Kanalprobleme beheben                | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Matrix-verschlüsselte-Raum-Einrichtung und Diagnose ausführen | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| Dokumentation nach Kategorie durchsuchen                      | [SUMMARY.md](SUMMARY.md)                                                                   |
| Projekt-PR/Issue-Dokumentations-Snapshot ansehen              | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Schneller Entscheidungsbaum (10 Sekunden)

- Einrichtung oder Erstinstallation nötig? → [setup-guides/README.md](setup-guides/README.md)
- Genaue CLI-/Konfigurationsschlüssel benötigt? → [reference/README.md](reference/README.md)
- Produktions-/Servicebetrieb benötigt? → [ops/README.md](ops/README.md)
- Fehler oder Regressionen sichtbar? → [troubleshooting.md](ops/troubleshooting.md)
- Arbeiten an Sicherheitshärtung oder Roadmap? → [security/README.md](security/README.md)
- Arbeiten mit Boards/Peripheriegeräten? → [hardware/README.md](hardware/README.md)
- Beitragen/Review/CI-Workflow? → [contributing/README.md](contributing/README.md)
- Vollständige Karte gewünscht? → [SUMMARY.md](SUMMARY.md)

## Sammlungen (empfohlen)

- Einstieg: [setup-guides/README.md](setup-guides/README.md)
- Referenzkataloge: [reference/README.md](reference/README.md)
- Betrieb und Bereitstellung: [ops/README.md](ops/README.md)
- Sicherheitsdokumentation: [security/README.md](security/README.md)
- Hardware/Peripheriegeräte: [hardware/README.md](hardware/README.md)
- Beitragen/CI: [contributing/README.md](contributing/README.md)
- Projekt-Snapshots: [maintainers/README.md](maintainers/README.md)

## Nach Zielgruppe

### Benutzer / Betreiber

- [commands-reference.md](reference/cli/commands-reference.md) — Befehlssuche nach Workflow
- [providers-reference.md](reference/api/providers-reference.md) — Anbieter-IDs, Aliase, Umgebungsvariablen für Anmeldedaten
- [channels-reference.md](reference/api/channels-reference.md) — Kanalfähigkeiten und Konfigurationspfade
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — Matrix-verschlüsselter-Raum-Einrichtung (E2EE) und Diagnose bei ausbleibender Antwort
- [config-reference.md](reference/api/config-reference.md) — wichtige Konfigurationsschlüssel und sichere Standardwerte
- [custom-providers.md](contributing/custom-providers.md) — Integrationsmuster für benutzerdefinierte Anbieter/Basis-URL
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM-Einrichtung und Endpunkt-Matrix
- [langgraph-integration.md](contributing/langgraph-integration.md) — Fallback-Integration für Modell-/Tool-Call-Grenzfälle
- [operations-runbook.md](ops/operations-runbook.md) — täglicher Laufzeitbetrieb und Rollback-Abläufe
- [troubleshooting.md](ops/troubleshooting.md) — häufige Fehlersignaturen und Wiederherstellungsschritte

### Mitwirkende / Betreuer

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Sicherheit / Zuverlässigkeit

> Hinweis: Dieser Bereich enthält Vorschlags-/Roadmap-Dokumente. Für das aktuelle Verhalten beginnen Sie mit [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) und [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Systemnavigation und Governance

- Einheitliches Inhaltsverzeichnis: [SUMMARY.md](SUMMARY.md)
- Dokumentationsstrukturkarte (Sprache/Teil/Funktion): [structure/README.md](maintainers/structure-README.md)
- Dokumentationsinventar/-klassifizierung: [docs-inventory.md](maintainers/docs-inventory.md)
- Projekt-Triage-Snapshot: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Andere Sprachen

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
