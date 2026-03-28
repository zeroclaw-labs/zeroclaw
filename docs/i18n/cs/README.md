# Dokumentační hub ZeroClaw

Tato stránka je hlavním vstupním bodem do dokumentačního systému.

Poslední aktualizace: **20. února 2026**.

Lokalizované huby: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Začněte zde

| Chci…                                                                | Přečtěte si toto                                                               |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Rychle nainstalovat a spustit ZeroClaw                              | [README.md (Rychlý start)](../README.md#quick-start)                           |
| Bootstrap jedním příkazem                                           | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| Najít příkazy podle úkolu                                           | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Rychle ověřit konfigurační klíče a výchozí hodnoty                  | [config-reference.md](reference/api/config-reference.md)                       |
| Nastavit vlastní poskytovatele/endpointy                            | [custom-providers.md](contributing/custom-providers.md)                         |
| Nastavit poskytovatele Z.AI / GLM                                   | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| Použít integrační vzory LangGraph                                   | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| Provozovat runtime (provozní příručka)                              | [operations-runbook.md](ops/operations-runbook.md)                             |
| Řešit problémy s instalací/runtime/kanály                           | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Spustit nastavení a diagnostiku šifrovaných místností Matrix        | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| Procházet dokumentaci podle kategorie                               | [SUMMARY.md](SUMMARY.md)                                                       |
| Zobrazit snapshot dokumentace PR/issues projektu                    | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Rychlý rozhodovací strom (10 sekund)

- Potřebujete nastavení nebo počáteční instalaci? → [setup-guides/README.md](setup-guides/README.md)
- Potřebujete přesné CLI/konfigurační klíče? → [reference/README.md](reference/README.md)
- Potřebujete produkční/servisní operace? → [ops/README.md](ops/README.md)
- Vidíte selhání nebo regrese? → [troubleshooting.md](ops/troubleshooting.md)
- Pracujete na posílení zabezpečení nebo roadmapě? → [security/README.md](security/README.md)
- Pracujete s deskami/periferiemi? → [hardware/README.md](hardware/README.md)
- Přispívání/revize/CI workflow? → [contributing/README.md](contributing/README.md)
- Chcete kompletní mapu? → [SUMMARY.md](SUMMARY.md)

## Kolekce (doporučené)

- Začínáme: [setup-guides/README.md](setup-guides/README.md)
- Referenční katalogy: [reference/README.md](reference/README.md)
- Provoz a nasazení: [ops/README.md](ops/README.md)
- Dokumentace zabezpečení: [security/README.md](security/README.md)
- Hardware/periferie: [hardware/README.md](hardware/README.md)
- Přispívání/CI: [contributing/README.md](contributing/README.md)
- Snapshoty projektu: [maintainers/README.md](maintainers/README.md)

## Podle publika

### Uživatelé / Operátoři

- [commands-reference.md](reference/cli/commands-reference.md) — vyhledávání příkazů podle workflow
- [providers-reference.md](reference/api/providers-reference.md) — ID poskytovatelů, aliasy, proměnné prostředí pro přihlašovací údaje
- [channels-reference.md](reference/api/channels-reference.md) — schopnosti kanálů a konfigurační cesty
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — nastavení šifrovaných místností Matrix (E2EE) a diagnostika nereagování
- [config-reference.md](reference/api/config-reference.md) — klíčové konfigurační hodnoty a bezpečné výchozí nastavení
- [custom-providers.md](contributing/custom-providers.md) — vzory integrace vlastního poskytovatele/base URL
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — nastavení Z.AI/GLM a matice endpointů
- [langgraph-integration.md](contributing/langgraph-integration.md) — záložní integrace pro okrajové případy modelu/volání nástrojů
- [operations-runbook.md](ops/operations-runbook.md) — každodenní runtime operace a postupy rollbacku
- [troubleshooting.md](ops/troubleshooting.md) — běžné signatury selhání a kroky obnovy

### Přispěvatelé / Správci

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Zabezpečení / Spolehlivost

> Poznámka: tato sekce zahrnuje dokumenty návrhů/roadmapy. Pro aktuální chování začněte s [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) a [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Systémová navigace a správa

- Jednotný obsah: [SUMMARY.md](SUMMARY.md)
- Mapa struktury dokumentace (jazyk/část/funkce): [structure/README.md](maintainers/structure-README.md)
- Inventář/klasifikace dokumentace: [docs-inventory.md](maintainers/docs-inventory.md)
- Snapshot třídění projektu: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Další jazyky

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
