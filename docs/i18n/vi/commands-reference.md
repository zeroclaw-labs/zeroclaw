# Tham khảo lệnh DaemonClaw

Dựa trên CLI hiện tại (`daemonclaw --help`).

Xác minh lần cuối: **2026-02-20**.

## Lệnh cấp cao nhất

| Lệnh | Mục đích |
|---|---|
| `onboard` | Khởi tạo workspace/config nhanh hoặc tương tác |
| `agent` | Chạy chat tương tác hoặc chế độ gửi tin nhắn đơn |
| `gateway` | Khởi động gateway webhook và HTTP WhatsApp |
| `daemon` | Khởi động runtime có giám sát (gateway + channels + heartbeat/scheduler tùy chọn) |
| `service` | Quản lý vòng đời dịch vụ cấp hệ điều hành |
| `doctor` | Chạy chẩn đoán và kiểm tra trạng thái |
| `status` | Hiển thị cấu hình và tóm tắt hệ thống |
| `cron` | Quản lý tác vụ định kỳ |
| `models` | Làm mới danh mục model của provider |
| `providers` | Liệt kê ID provider, bí danh và provider đang dùng |
| `channel` | Quản lý kênh và kiểm tra sức khỏe kênh |
| `integrations` | Kiểm tra chi tiết tích hợp |
| `skills` | Liệt kê/cài đặt/gỡ bỏ skills |
| `migrate` | Nhập dữ liệu từ runtime khác (hiện hỗ trợ OpenClaw) |
| `config` | Xuất schema cấu hình dạng máy đọc được |
| `completions` | Tạo script tự hoàn thành cho shell ra stdout |
| `hardware` | Phát hiện và kiểm tra phần cứng USB |
| `peripheral` | Cấu hình và nạp firmware thiết bị ngoại vi |

## Nhóm lệnh

### `onboard`

- `daemonclaw onboard`
- `daemonclaw onboard --channels-only`
- `daemonclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `daemonclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`

### `agent`

- `daemonclaw agent`
- `daemonclaw agent -m "Hello"`
- `daemonclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `daemonclaw agent --peripheral <board:path>`

### `gateway` / `daemon`

- `daemonclaw gateway [--host <HOST>] [--port <PORT>]`
- `daemonclaw daemon [--host <HOST>] [--port <PORT>]`

### `service`

- `daemonclaw service install`
- `daemonclaw service start`
- `daemonclaw service stop`
- `daemonclaw service restart`
- `daemonclaw service status`
- `daemonclaw service uninstall`

### `cron`

- `daemonclaw cron list`
- `daemonclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `daemonclaw cron add-at <rfc3339_timestamp> <command>`
- `daemonclaw cron add-every <every_ms> <command>`
- `daemonclaw cron once <delay> <command>`
- `daemonclaw cron remove <id>`
- `daemonclaw cron pause <id>`
- `daemonclaw cron resume <id>`

### `models`

- `daemonclaw models refresh`
- `daemonclaw models refresh --provider <ID>`
- `daemonclaw models refresh --force`

`models refresh` hiện hỗ trợ làm mới danh mục trực tiếp cho các provider: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen` và `nvidia`.

### `channel`

- `daemonclaw channel list`
- `daemonclaw channel start`
- `daemonclaw channel doctor`
- `daemonclaw channel bind-telegram <IDENTITY>`
- `daemonclaw channel add <type> <json>`
- `daemonclaw channel remove <name>`

Lệnh trong chat khi runtime đang chạy (Telegram/Discord):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`

Channel runtime cũng theo dõi `config.toml` và tự động áp dụng thay đổi cho:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (cho provider mặc định)
- `reliability.*` cài đặt retry của provider

`add/remove` hiện chuyển hướng về thiết lập có hướng dẫn / cấu hình thủ công (chưa hỗ trợ đầy đủ mutator khai báo).

### `integrations`

- `daemonclaw integrations info <name>`

### `skills`

- `daemonclaw skills list`
- `daemonclaw skills install <source>`
- `daemonclaw skills remove <name>`

`<source>` chấp nhận git remote (`https://...`, `http://...`, `ssh://...` và `git@host:owner/repo.git`) hoặc đường dẫn cục bộ.

Skill manifest (`SKILL.toml`) hỗ trợ `prompts` và `[[tools]]`; cả hai được đưa vào system prompt của agent khi chạy, giúp model có thể tuân theo hướng dẫn skill mà không cần đọc thủ công.

### `migrate`

- `daemonclaw migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `daemonclaw config schema`

`config schema` xuất JSON Schema (draft 2020-12) cho toàn bộ hợp đồng `config.toml` ra stdout.

### `completions`

- `daemonclaw completions bash`
- `daemonclaw completions fish`
- `daemonclaw completions zsh`
- `daemonclaw completions powershell`
- `daemonclaw completions elvish`

`completions` chỉ xuất ra stdout để script có thể được source trực tiếp mà không bị lẫn log/cảnh báo.

### `hardware`

- `daemonclaw hardware discover`
- `daemonclaw hardware introspect <path>`
- `daemonclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `daemonclaw peripheral list`
- `daemonclaw peripheral add <board> <path>`
- `daemonclaw peripheral flash [--port <serial_port>]`
- `daemonclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `daemonclaw peripheral flash-nucleo`

## Kiểm tra nhanh

Để xác minh nhanh tài liệu với binary hiện tại:

```bash
daemonclaw --help
daemonclaw <command> --help
```
