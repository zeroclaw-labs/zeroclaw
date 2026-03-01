# Справочник провайдеров (Русский)

Это первичная локализация Wave 1 для проверки provider ID, алиасов и переменных окружения.

Оригинал на английском:

- [../../providers-reference.md](../../providers-reference.md)

## Когда использовать

- Выбор провайдера и модели
- Проверка ID/alias/credential env vars
- Диагностика ошибок аутентификации и конфигурации

## Правило

- Provider ID и имена env переменных не переводятся.
- Нормативное описание поведения — в английском оригинале.

## Обновления

- 2026-03-01: добавлена поддержка провайдера StepFun (`stepfun`, алиасы `step`, `step-ai`, `step_ai`).

## StepFun (Кратко)

- Provider ID: `stepfun`
- Алиасы: `step`, `step-ai`, `step_ai`
- Base API URL: `https://api.stepfun.com/v1`
- Эндпоинты: `POST /v1/chat/completions`, `GET /v1/models`
- Переменная авторизации: `STEP_API_KEY` (fallback: `STEPFUN_API_KEY`)
- Модель по умолчанию: `step-3.5-flash`

Быстрая проверка:

```bash
export STEP_API_KEY="your-stepfun-api-key"
zeroclaw models refresh --provider stepfun
zeroclaw agent --provider stepfun --model step-3.5-flash -m "ping"
```
