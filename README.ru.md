<p align="center">
  <img src="zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀（Русский）</h1>

<p align="center">
  <strong>Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.</strong>
</p>

<p align="center">
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://www.xiaohongshu.com/user/profile/67cbfc43000000000d008307?xsec_token=AB73VnYnGNx5y36EtnnZfGmAmS-6Wzv8WMuGpfwfkg6Yc%3D&xsec_source=pc_search"><img src="https://img.shields.io/badge/Xiaohongshu-Official-FF2442?style=flat" alt="Xiaohongshu: Official" /></a>
  <a href="https://t.me/zeroclawlabs"><img src="https://img.shields.io/badge/Telegram-%40zeroclawlabs-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram: @zeroclawlabs" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
  🌐 Языки: <a href="README.md">English</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a> · <a href="README.ru.md">Русский</a>
</p>

<p align="center">
  <a href="bootstrap.sh">Установка в 1 клик</a> |
  <a href="docs/getting-started/README.md">Быстрый старт</a> |
  <a href="docs/README.ru.md">Хаб документации</a> |
  <a href="docs/SUMMARY.md">TOC docs</a>
</p>

<p align="center">
  <strong>Быстрые маршруты:</strong>
  <a href="docs/reference/README.md">Справочники</a> ·
  <a href="docs/operations/README.md">Операции</a> ·
  <a href="docs/troubleshooting.md">Диагностика</a> ·
  <a href="docs/security/README.md">Безопасность</a> ·
  <a href="docs/hardware/README.md">Аппаратная часть</a> ·
  <a href="docs/contributing/README.md">Вклад и CI</a>
</p>

> Этот файл — выверенный перевод `README.md` с акцентом на точность и читаемость (не дословный перевод).
>
> Технические идентификаторы (команды, ключи конфигурации, API-пути, имена Trait) сохранены на английском.
>
> Последняя синхронизация: **2026-02-19**.

## 📢 Доска объявлений

Публикуйте здесь важные уведомления (breaking changes, security advisories, окна обслуживания и блокеры релиза).

| Дата (UTC) | Уровень | Объявление | Действие |
|---|---|---|---|
| 2026-02-19 | _Срочно_ | Мы **не аффилированы** с `openagen/zeroclaw` и `zeroclaw.org`. Домен `zeroclaw.org` сейчас указывает на fork `openagen/zeroclaw`, и этот домен/репозиторий выдают себя за наш официальный сайт и проект. | Не доверяйте информации, бинарникам, сборам средств и «официальным» объявлениям из этих источников. Используйте только этот репозиторий и наши верифицированные соцсети. |
| 2026-02-19 | _Важно_ | Официальный сайт пока **не запущен**, и мы уже видим попытки выдавать себя за ZeroClaw. Пожалуйста, не участвуйте в инвестициях, сборах средств или похожих активностях от имени ZeroClaw. | Ориентируйтесь только на этот репозиторий; также следите за [X (@zeroclawlabs)](https://x.com/zeroclawlabs?s=21), [Reddit (r/zeroclawlabs)](https://www.reddit.com/r/zeroclawlabs/), [Telegram (@zeroclawlabs)](https://t.me/zeroclawlabs) и [Xiaohongshu](https://www.xiaohongshu.com/user/profile/67cbfc43000000000d008307?xsec_token=AB73VnYnGNx5y36EtnnZfGmAmS-6Wzv8WMuGpfwfkg6Yc%3D&xsec_source=pc_search) для официальных обновлений. |
| 2026-02-19 | _Важно_ | Anthropic обновил раздел Authentication and Credential Use 2026-02-19. В нем указано, что OAuth authentication (Free/Pro/Max) предназначена только для Claude Code и Claude.ai; использование OAuth-токенов, полученных через Claude Free/Pro/Max, в любых других продуктах, инструментах или сервисах (включая Agent SDK), не допускается и может считаться нарушением Consumer Terms of Service. | Чтобы избежать потерь, временно не используйте Claude Code OAuth-интеграции. Оригинал: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use). |

## О проекте

ZeroClaw — это производительная и расширяемая инфраструктура автономного AI-агента:

- Нативно на Rust, единый бинарник, переносимость между ARM / x86 / RISC-V
- Архитектура на Trait (`Provider`, `Channel`, `Tool`, `Memory` и др.)
- Безопасные значения по умолчанию: pairing, явные allowlist, sandbox и scope-ограничения

## Почему выбирают ZeroClaw

- **Лёгкий runtime по умолчанию**: Повседневные CLI-операции и `status` обычно укладываются в несколько МБ памяти.
- **Оптимизирован для недорогих сред**: Подходит для бюджетных плат и небольших cloud-инстансов без тяжёлой runtime-обвязки.
- **Быстрый cold start**: Архитектура одного Rust-бинарника ускоряет запуск основных команд и daemon-режима.
- **Портативная модель деплоя**: Единый подход для ARM / x86 / RISC-V и возможность менять providers/channels/tools.

## Снимок бенчмарка (ZeroClaw vs OpenClaw, воспроизводимо)

Ниже — быстрый локальный сравнительный срез (macOS arm64, февраль 2026), нормализованный под 0.8GHz edge CPU.

| | OpenClaw | NanoBot | PicoClaw | ZeroClaw 🦀 |
|---|---|---|---|---|
| **Язык** | TypeScript | Python | Go | **Rust** |
| **RAM** | > 1GB | > 100MB | < 10MB | **< 5MB** |
| **Старт (ядро 0.8GHz)** | > 500s | > 30s | < 1s | **< 10ms** |
| **Размер бинарника** | ~28MB (dist) | N/A (скрипты) | ~8MB | **3.4 MB** |
| **Стоимость** | Mac Mini $599 | Linux SBC ~$50 | Linux-плата $10 | **Любое железо за $10** |

> Примечание: результаты ZeroClaw получены на release-сборке с помощью `/usr/bin/time -l`. OpenClaw требует Node.js runtime; только этот runtime обычно добавляет около 390MB дополнительного потребления памяти. NanoBot требует Python runtime. PicoClaw и ZeroClaw — статические бинарники.

<p align="center">
  <img src="zero-claw.jpeg" alt="Сравнение ZeroClaw и OpenClaw" width="800" />
</p>

### Локально воспроизводимое измерение

Метрики могут меняться вместе с кодом и toolchain, поэтому проверяйте результаты в своей среде:

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

Текущие примерные значения из README (macOS arm64, 2026-02-18):

- Размер release-бинарника: `8.8M`
- `zeroclaw --help`: ~`0.02s`, пик памяти ~`3.9MB`
- `zeroclaw status`: ~`0.01s`, пик памяти ~`4.1MB`

## Установка в 1 клик

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

Для полной инициализации окружения: `./bootstrap.sh --install-system-deps --install-rust` (для системных пакетов может потребоваться `sudo`).

Подробности: [`docs/one-click-bootstrap.md`](docs/one-click-bootstrap.md).

## Быстрый старт

### Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release --locked
cargo install --path . --force --locked

zeroclaw onboard --api-key sk-... --provider openrouter
zeroclaw onboard --interactive

zeroclaw agent -m "Hello, ZeroClaw!"

# default: 127.0.0.1:3000
zeroclaw gateway

zeroclaw daemon
```

## Важные security-дефолты

- Gateway по умолчанию: `127.0.0.1:3000`
- Pairing обязателен по умолчанию: `require_pairing = true`
- Публичный bind запрещён по умолчанию: `allow_public_bind = false`
- Семантика allowlist каналов:
  - `[]` => deny-by-default
  - `["*"]` => allow all (используйте осознанно)

## Пример конфигурации

```toml
api_key = "sk-..."
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
embedding_provider = "none"

[gateway]
host = "127.0.0.1"
port = 3000
require_pairing = true
allow_public_bind = false
```

## Навигация по документации

- Хаб документации (English): [`docs/README.md`](docs/README.md)
- Единый TOC docs: [`docs/SUMMARY.md`](docs/SUMMARY.md)
- Хаб документации (Русский): [`docs/README.ru.md`](docs/README.ru.md)
- Справочник команд: [`docs/commands-reference.md`](docs/commands-reference.md)
- Справочник конфигурации: [`docs/config-reference.md`](docs/config-reference.md)
- Справочник providers: [`docs/providers-reference.md`](docs/providers-reference.md)
- Справочник channels: [`docs/channels-reference.md`](docs/channels-reference.md)
- Операционный runbook: [`docs/operations-runbook.md`](docs/operations-runbook.md)
- Устранение неполадок: [`docs/troubleshooting.md`](docs/troubleshooting.md)
- Инвентарь и классификация docs: [`docs/docs-inventory.md`](docs/docs-inventory.md)
- Снимок triage проекта: [`docs/project-triage-snapshot-2026-02-18.md`](docs/project-triage-snapshot-2026-02-18.md)

## Вклад и лицензия

- Contribution guide: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- PR workflow: [`docs/pr-workflow.md`](docs/pr-workflow.md)
- Reviewer playbook: [`docs/reviewer-playbook.md`](docs/reviewer-playbook.md)
- License: MIT ([`LICENSE`](LICENSE), [`NOTICE`](NOTICE))

---

Для полной и исчерпывающей информации (архитектура, все команды, API, разработка) используйте основной английский документ: [`README.md`](README.md).
