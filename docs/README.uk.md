# Центр документації ZeroClaw

Ця сторінка є основною точкою входу до системи документації.

Останнє оновлення: **21 лютого 2026**.

Локалізовані центри: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Почніть тут

| Я хочу…                                                             | Читати це                                                                      |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Швидко встановити та запустити ZeroClaw                               | [README.md (Швидкий старт)](../README.md#quick-start)                           |
| Налаштування однією командою                                         | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| Знайти команди за завданням                                          | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Швидко перевірити ключі конфігурації та значення за замовчуванням     | [config-reference.md](reference/api/config-reference.md)                       |
| Налаштувати власного провайдера/endpoint                             | [custom-providers.md](contributing/custom-providers.md)                         |
| Налаштувати провайдера Z.AI / GLM                                   | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| Використовувати шаблони інтеграції LangGraph                         | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| Керувати середовищем виконання (runbook 2-го дня)                    | [operations-runbook.md](ops/operations-runbook.md)                             |
| Усунути проблеми встановлення/виконання/каналів                      | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Запустити налаштування та діагностику зашифрованих кімнат Matrix      | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| Переглянути документацію за категоріями                               | [SUMMARY.md](SUMMARY.md)                                                       |
| Переглянути знімок PR/issues проекту                                 | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Дерево швидких рішень (10 секунд)

- Потрібне налаштування або початкове встановлення? → [setup-guides/README.md](setup-guides/README.md)
- Потрібні точні ключі CLI/конфігурації? → [reference/README.md](reference/README.md)
- Потрібні операції виробництва/сервісу? → [ops/README.md](ops/README.md)
- Бачите збої або регресії? → [troubleshooting.md](ops/troubleshooting.md)
- Працюєте над зміцненням безпеки або дорожньою картою? → [security/README.md](security/README.md)
- Працюєте з платами/периферією? → [hardware/README.md](hardware/README.md)
- Внесок/рецензування/робочий процес CI? → [contributing/README.md](contributing/README.md)
- Хочете повну карту? → [SUMMARY.md](SUMMARY.md)

## Колекції (Рекомендовані)

- Початок роботи: [setup-guides/README.md](setup-guides/README.md)
- Довідкові каталоги: [reference/README.md](reference/README.md)
- Операції та розгортання: [ops/README.md](ops/README.md)
- Документація з безпеки: [security/README.md](security/README.md)
- Обладнання/периферія: [hardware/README.md](hardware/README.md)
- Внесок/CI: [contributing/README.md](contributing/README.md)
- Знімки проекту: [maintainers/README.md](maintainers/README.md)

## За аудиторією

### Користувачі / Оператори

- [commands-reference.md](reference/cli/commands-reference.md) — пошук команд за робочим процесом
- [providers-reference.md](reference/api/providers-reference.md) — ідентифікатори провайдерів, псевдоніми, змінні середовища облікових даних
- [channels-reference.md](reference/api/channels-reference.md) — можливості каналів та шляхи конфігурації
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — налаштування зашифрованих кімнат Matrix (E2EE) та діагностика відсутності відповіді
- [config-reference.md](reference/api/config-reference.md) — ключові параметри конфігурації та безпечні значення за замовчуванням
- [custom-providers.md](contributing/custom-providers.md) — шаблони інтеграції власного провайдера/базової URL-адреси
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — налаштування Z.AI/GLM та матриця endpoint
- [langgraph-integration.md](contributing/langgraph-integration.md) — резервна інтеграція для крайніх випадків моделі/виклику інструментів
- [operations-runbook.md](ops/operations-runbook.md) — операції середовища виконання 2-го дня та потік відкату
- [troubleshooting.md](ops/troubleshooting.md) — типові сигнатури збоїв та кроки відновлення

### Учасники / Супровідники

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Безпека / Надійність

> Примітка: цей розділ містить документи пропозицій/дорожньої карти. Для поточної поведінки почніть з [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) та [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Навігація системою та управління

- Єдиний зміст: [SUMMARY.md](SUMMARY.md)
- Карта структури документації (мова/розділ/функція): [structure/README.md](maintainers/structure-README.md)
- Інвентаризація/класифікація документації: [docs-inventory.md](maintainers/docs-inventory.md)
- Знімок тріажу проекту: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Інші мови

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
