# Centrum Dokumentacji JhedaiClaw

Ta strona jest głównym punktem wejścia do systemu dokumentacji.

Ostatnia aktualizacja: **20 lutego 2026**.

Zlokalizowane centra: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Zacznij tutaj

| Chcę…                                                    | Przeczytaj to                                                                              |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Szybko zainstalować i uruchomić JhedaiClaw               | [README.md (Szybki Start)](../README.md#quick-start)                                       |
| Bootstrap jednym poleceniem                              | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| Znaleźć polecenia według zadania                         | [commands-reference.md](reference/cli/commands-reference.md)                               |
| Szybko sprawdzić klucze konfiguracji i wartości domyślne | [config-reference.md](reference/api/config-reference.md)                                   |
| Skonfigurować niestandardowych dostawców/endpointy       | [custom-providers.md](contributing/custom-providers.md)                                    |
| Skonfigurować dostawcę Z.AI / GLM                        | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| Użyć wzorców integracji LangGraph                        | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| Zarządzać środowiskiem uruchomieniowym (runbook dzień-2) | [operations-runbook.md](ops/operations-runbook.md)                                         |
| Rozwiązać problemy z instalacją/runtime/kanałami         | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Skonfigurować i zdiagnozować szyfrowane pokoje Matrix    | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| Przeglądać dokumentację według kategorii                 | [SUMMARY.md](SUMMARY.md)                                                                   |
| Zobaczyć migawkę dokumentacji PR-ów/issues projektu      | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Szybkie Drzewo Decyzyjne (10 sekund)

- Potrzebujesz pierwszej instalacji lub konfiguracji? → [setup-guides/README.md](setup-guides/README.md)
- Potrzebujesz dokładnych kluczy CLI/konfiguracji? → [reference/README.md](reference/README.md)
- Potrzebujesz operacji produkcyjnych/serwisowych? → [ops/README.md](ops/README.md)
- Widzisz błędy lub regresje? → [troubleshooting.md](ops/troubleshooting.md)
- Pracujesz nad wzmocnieniem bezpieczeństwa lub mapą drogową? → [security/README.md](security/README.md)
- Pracujesz z płytkami/peryferiami? → [hardware/README.md](hardware/README.md)
- Kontrybuowanie/recenzja/workflow CI? → [contributing/README.md](contributing/README.md)
- Chcesz zobaczyć pełną mapę? → [SUMMARY.md](SUMMARY.md)

## Kolekcje (Zalecane)

- Rozpoczęcie pracy: [setup-guides/README.md](setup-guides/README.md)
- Katalogi referencyjne: [reference/README.md](reference/README.md)
- Operacje i wdrożenie: [ops/README.md](ops/README.md)
- Dokumentacja bezpieczeństwa: [security/README.md](security/README.md)
- Hardware/peryferia: [hardware/README.md](hardware/README.md)
- Kontrybuowanie/CI: [contributing/README.md](contributing/README.md)
- Migawki projektu: [maintainers/README.md](maintainers/README.md)

## Według Odbiorców

### Użytkownicy / Operatorzy

- [commands-reference.md](reference/cli/commands-reference.md) — wyszukiwanie poleceń według workflow
- [providers-reference.md](reference/api/providers-reference.md) — ID dostawców, aliasy, zmienne środowiskowe uwierzytelniania
- [channels-reference.md](reference/api/channels-reference.md) — możliwości kanałów i ścieżki konfiguracji
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — konfiguracja szyfrowanych pokojów Matrix (E2EE) i diagnostyka braku odpowiedzi
- [config-reference.md](reference/api/config-reference.md) — klucze konfiguracji o wysokim znaczeniu i bezpieczne wartości domyślne
- [custom-providers.md](contributing/custom-providers.md) — wzorce integracji niestandardowych dostawców/bazowego URL
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — konfiguracja Z.AI/GLM i matryca endpointów
- [langgraph-integration.md](contributing/langgraph-integration.md) — integracja awaryjna dla przypadków brzegowych modelu/wywołania narzędzi
- [operations-runbook.md](ops/operations-runbook.md) — operacje runtime dzień-2 i przepływy rollbacku
- [troubleshooting.md](ops/troubleshooting.md) — typowe sygnatury błędów i kroki odzyskiwania

### Kontrybutorzy / Opiekunowie

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Bezpieczeństwo / Niezawodność

> Uwaga: ta sekcja zawiera dokumenty propozycji/mapy drogowej. Dla aktualnego zachowania zacznij od [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) i [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Nawigacja Systemowa i Zarządzanie

- Ujednolicony spis treści: [SUMMARY.md](SUMMARY.md)
- Mapa struktury dokumentacji (język/część/funkcja): [structure/README.md](maintainers/structure-README.md)
- Inwentarz/klasyfikacja dokumentacji: [docs-inventory.md](maintainers/docs-inventory.md)
- Migawka triażu projektu: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Inne języki

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
