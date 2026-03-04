# Справочник конфигурации (Русский)

Это первичная локализация Wave 1 для работы с ключами конфигурации и безопасными дефолтами.

Оригинал на английском:

- [../../config-reference.md](../../config-reference.md)

## Когда использовать

- Первичная настройка окружения
- Проверка конфликтов конфигурации
- Аудит параметров, влияющих на безопасность и стабильность

## Правило

- Названия config keys не переводятся.
- Точное runtime-поведение определяется английским оригиналом.
- Добавлен ключ `observability.runtime_trace_record_http` для записи HTTP-деталей вызовов LLM (`llm_http_request` / `llm_http_response`); по умолчанию `false`; эффективен только когда `runtime_trace_mode` равен `rolling` или `full`. Payload маскирует чувствительные поля, но trace-файлы остаются чувствительными операционными данными. Запросы/ответы/заголовки обрезаются при превышении размера. Рассмотрите отключение в продакшене. Подробности в английском оригинале.

## Обновление (2026-03-03)

- В секции `[agent]` добавлены `allowed_tools` и `denied_tools`.
  - Если `allowed_tools` не пуст, основному агенту показываются только инструменты из allowlist.
  - `denied_tools` применяется после allowlist и дополнительно исключает инструменты.
- Неизвестные элементы `allowed_tools` пропускаются (с debug-логом) и не ломают запуск.
- Если одновременно заданы `allowed_tools` и `denied_tools`, и после фильтрации не остается исполняемых инструментов, запуск завершается fail-fast с явной ошибкой конфигурации.
- Полная таблица параметров и пример остаются в английском `config-reference.md` в разделе `[agent]`.

## `[observability]`

| Ключ | По умолчанию | Назначение |
|---|---|---|
| `backend` | `none` | Бакенд обсервабельности: `none`, `noop`, `log`, `prometheus`, `otel`, `opentelemetry` или `otlp` |
| `otel_endpoint` | `http://localhost:4318` | OTLP HTTP-эндпоинт, когда backend — `otel` |
| `otel_service_name` | `zeroclaw` | Имя сервиса, отправляемое в OTLP коллектор |
| `runtime_trace_mode` | `none` | Режим хранения runtime trace: `none`, `rolling` или `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | Путь JSONL runtime trace (относительно workspace, если не абсолютный) |
| `runtime_trace_max_entries` | `200` | Максимальное сохраняемое количество событий при `runtime_trace_mode = "rolling"` |
| `runtime_trace_record_http` | `false` | Запись детальных событий HTTP-запросов/ответов LLM (`llm_http_request` / `llm_http_response`) в runtime trace |

Примечания:

- `backend = "otel"` использует OTLP HTTP-экспорт с блокирующим клиентом, чтобы span и metric можно было безопасно отправлять из не-Tokio-контекстов.
- Алиасы `opentelemetry` и `otlp` указывают на тот же OTel бэкенд.
- Runtime traces предназначены для отладки сбоев tool-call и некорректных payload инструментов модели. Они могут содержать текст вывода модели, поэтому оставьте их отключенными по умолчанию на общих хостах.
- `runtime_trace_record_http` эффективен только когда `runtime_trace_mode` равен `rolling` или `full`.
  - HTTP trace payloads маскируют типичные чувствительные поля (например заголовки Authorization и поля query/body типа token), но всё равно считайте trace-файлы чувствительными операционными данными.
  - Для потоковых запросов, чтобы повысить эффективность, захват тела ответа пропускается, а тела запросов по-прежнему захватываются (с учетом ограничений размера).
  - Запросы/ответы/значения заголовков обрезаются если слишком большие. Однако LLM-трафик высокого объёма с большими ответами всё же может значительно увеличить использование памяти и размер trace-файлов.
  - Рассмотрите отключение HTTP трейсинга в продакшене.
- Запрашивайте runtime traces с помощью:
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --event llm_http_response --contains \"500\"`
  - `zeroclaw doctor traces --id <trace-id>`

Пример:

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 200
runtime_trace_record_http = true
```
