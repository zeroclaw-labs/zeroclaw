# Документация JhedaiClaw (Русский)

Эта страница — русскоязычная точка входа в документацию.

Последняя синхронизация: **2026-02-18**.

> Примечание: команды, ключи конфигурации и API-пути сохраняются на английском. Для первоисточника ориентируйтесь на англоязычные документы.

## Быстрые ссылки

| Что нужно                                | Куда смотреть                                                                              |
| ---------------------------------------- | ------------------------------------------------------------------------------------------ |
| Быстро установить и запустить            | [../README.ru.md](../README.ru.md) / [../README.md](../README.md)                          |
| Установить одной командой                | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| Найти команды по задаче                  | [commands-reference.md](reference/cli/commands-reference.md)                               |
| Проверить ключи конфигурации и дефолты   | [config-reference.md](reference/api/config-reference.md)                                   |
| Подключить кастомный provider / endpoint | [custom-providers.md](contributing/custom-providers.md)                                    |
| Настроить provider Z.AI / GLM            | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| Использовать интеграцию LangGraph        | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| Операционный runbook (day-2)             | [operations-runbook.md](ops/operations-runbook.md)                                         |
| Быстро устранить типовые проблемы        | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Открыть общий TOC docs                   | [SUMMARY.md](SUMMARY.md)                                                                   |
| Посмотреть snapshot PR/Issue             | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Дерево решений на 10 секунд

- Нужна первая установка и быстрый старт → [setup-guides/README.md](setup-guides/README.md)
- Нужны точные команды и ключи конфигурации → [reference/README.md](reference/README.md)
- Нужны операции/сервисный режим/деплой → [ops/README.md](ops/README.md)
- Есть ошибки, сбои или регрессии → [troubleshooting.md](ops/troubleshooting.md)
- Нужны материалы по безопасности и roadmap → [security/README.md](security/README.md)
- Работаете с платами и периферией → [hardware/README.md](hardware/README.md)
- Нужны процессы вклада, ревью и CI → [contributing/README.md](contributing/README.md)
- Нужна полная карта docs → [SUMMARY.md](SUMMARY.md)

## Навигация по категориям (рекомендуется)

- Старт и установка: [setup-guides/README.md](setup-guides/README.md)
- Справочники: [reference/README.md](reference/README.md)
- Операции и деплой: [ops/README.md](ops/README.md)
- Безопасность: [security/README.md](security/README.md)
- Аппаратная часть: [hardware/README.md](hardware/README.md)
- Вклад и CI: [contributing/README.md](contributing/README.md)
- Снимки проекта: [maintainers/README.md](maintainers/README.md)

## По ролям

### Пользователи / Операторы

- [commands-reference.md](reference/cli/commands-reference.md)
- [providers-reference.md](reference/api/providers-reference.md)
- [channels-reference.md](reference/api/channels-reference.md)
- [config-reference.md](reference/api/config-reference.md)
- [custom-providers.md](contributing/custom-providers.md)
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md)
- [langgraph-integration.md](contributing/langgraph-integration.md)
- [operations-runbook.md](ops/operations-runbook.md)
- [troubleshooting.md](ops/troubleshooting.md)

### Контрибьюторы / Мейнтейнеры

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Безопасность / Надёжность

> Примечание: часть документов в этом разделе относится к proposal/roadmap и может содержать гипотетические команды/конфигурации. Для текущего поведения сначала смотрите [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [resource-limits.md](ops/resource-limits.md)
- [audit-logging.md](security/audit-logging.md)
- [security-roadmap.md](security/security-roadmap.md)

## Инвентаризация и структура docs

- Единый TOC: [SUMMARY.md](SUMMARY.md)
- Карта структуры docs (язык/раздел/функция): [structure/README.md](maintainers/structure-README.md)
- Инвентарь и классификация docs: [docs-inventory.md](maintainers/docs-inventory.md)

## Другие языки

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
