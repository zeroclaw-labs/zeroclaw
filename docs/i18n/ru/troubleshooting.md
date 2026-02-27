# Troubleshooting (Русский)

Это первичная локализация Wave 1 для быстрого поиска типовых неисправностей.

Оригинал на английском:

- [../../troubleshooting.md](../../troubleshooting.md)

## Когда использовать

- Ошибки установки и запуска
- Диагностика через `status` и `doctor`
- Минимальный recovery/rollback сценарий

## Правило

- Коды ошибок, ключи логов и команды не переводятся.
- Подробные сигнатуры отказов — в английском оригинале.

## Обновление

### Ошибка `403`/`429` в `web_search_tool`

**Симптом**: Появляется сообщение типа `DuckDuckGo search failed with status: 403` (или `429`).

**Причина**: Некоторые сети/прокси блокируют HTML-эндпоинт DuckDuckGo.

**Варианты исправления**:

1. Переключиться на Brave:
```toml
[web_search]
enabled = true
provider = "brave"
brave_api_key = "<SECRET>"
```

2. Переключиться на Exa:
```toml
[web_search]
enabled = true
provider = "exa"
api_key = "<SECRET>"
# опционально
# api_url = "https://api.exa.ai/search"
```

3. Переключиться на Tavily:
```toml
[web_search]
enabled = true
provider = "tavily"
api_key = "<SECRET>"
# опционально
# api_url = "https://api.tavily.com/search"
```

4. Переключиться на Firecrawl (если поддерживается в сборке):
```toml
[web_search]
enabled = true
provider = "firecrawl"
api_key = "<SECRET>"
```

### `curl`/`wget` заблокированы в shell tool

**Симптом**: В выводе содержится `Command blocked: high-risk command is disallowed by policy`.

**Причина**: `curl`/`wget` блокируются политикой автономии как высокорисковые команды.

**Решение**: Используйте специализированные инструменты вместо shell fetch:
- `http_request` — прямые API/HTTP-запросы
- `web_fetch` — извлечение и обработка содержимого страниц

Минимальная конфигурация:
```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
provider = "fast_html2md"
allowed_domains = ["*"]
```

### `web_fetch`/`http_request` — Host not allowed

**Симптом**: Появляется ошибка типа `Host '<domain>' is not in http_request.allowed_domains`.

**Решение**: Добавьте домен в список или используйте `"*"` для публичного доступа:
```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
allowed_domains = ["*"]
blocked_domains = []
```

**Примечание по безопасности**: Локальные/приватные сети остаются заблокированными даже при `"*"`.
