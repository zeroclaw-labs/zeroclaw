# Tham khảo lệnh JhedaiClaw

Dựa trên CLI hiện tại (`jhedaiclaw --help`).

Xác minh lần cuối: **2026-02-20**.

## Lệnh cấp cao nhất

| Lệnh           | Mục đích                                                                          |
| -------------- | --------------------------------------------------------------------------------- |
| `onboard`      | Khởi tạo workspace/config nhanh hoặc tương tác                                    |
| `agent`        | Chạy chat tương tác hoặc chế độ gửi tin nhắn đơn                                  |
| `gateway`      | Khởi động gateway webhook và HTTP WhatsApp                                        |
| `daemon`       | Khởi động runtime có giám sát (gateway + channels + heartbeat/scheduler tùy chọn) |
| `service`      | Quản lý vòng đời dịch vụ cấp hệ điều hành                                         |
| `doctor`       | Chạy chẩn đoán và kiểm tra trạng thái                                             |
| `status`       | Hiển thị cấu hình và tóm tắt hệ thống                                             |
| `cron`         | Quản lý tác vụ định kỳ                                                            |
| `models`       | Làm mới danh mục model của provider                                               |
| `providers`    | Liệt kê ID provider, bí danh và provider đang dùng                                |
| `channel`      | Quản lý kênh và kiểm tra sức khỏe kênh                                            |
| `integrations` | Kiểm tra chi tiết tích hợp                                                        |
| `skills`       | Liệt kê/cài đặt/gỡ bỏ skills                                                      |
| `migrate`      | Nhập dữ liệu từ runtime khác (hiện hỗ trợ OpenClaw)                               |
| `config`       | Xuất schema cấu hình dạng máy đọc được                                            |
| `completions`  | Tạo script tự hoàn thành cho shell ra stdout                                      |
| `hardware`     | Phát hiện và kiểm tra phần cứng USB                                               |
| `peripheral`   | Cấu hình và nạp firmware thiết bị ngoại vi                                        |

## Nhóm lệnh

### `onboard`

- `jhedaiclaw onboard`
- `jhedaiclaw onboard --channels-only`
- `jhedaiclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `jhedaiclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`

### `agent`

- `jhedaiclaw agent`
- `jhedaiclaw agent -m "Hello"`
- `jhedaiclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `jhedaiclaw agent --peripheral <board:path>`

### `gateway` / `daemon`

- `jhedaiclaw gateway [--host <HOST>] [--port <PORT>]`
- `jhedaiclaw daemon [--host <HOST>] [--port <PORT>]`

### `service`

- `jhedaiclaw service install`
- `jhedaiclaw service start`
- `jhedaiclaw service stop`
- `jhedaiclaw service restart`
- `jhedaiclaw service status`
- `jhedaiclaw service uninstall`

### `cron`

- `jhedaiclaw cron list`
- `jhedaiclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `jhedaiclaw cron add-at <rfc3339_timestamp> <command>`
- `jhedaiclaw cron add-every <every_ms> <command>`
- `jhedaiclaw cron once <delay> <command>`
- `jhedaiclaw cron remove <id>`
- `jhedaiclaw cron pause <id>`
- `jhedaiclaw cron resume <id>`

### `models`

- `jhedaiclaw models refresh`
- `jhedaiclaw models refresh --provider <ID>`
- `jhedaiclaw models refresh --force`

`models refresh` hiện hỗ trợ làm mới danh mục trực tiếp cho các provider: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen` và `nvidia`.

### `channel`

- `jhedaiclaw channel list`
- `jhedaiclaw channel start`
- `jhedaiclaw channel doctor`
- `jhedaiclaw channel bind-telegram <IDENTITY>`
- `jhedaiclaw channel add <type> <json>`
- `jhedaiclaw channel remove <name>`

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

- `jhedaiclaw integrations info <name>`

### `skills`

- `jhedaiclaw skills list`
- `jhedaiclaw skills install <source>`
- `jhedaiclaw skills remove <name>`

`<source>` chấp nhận git remote (`https://...`, `http://...`, `ssh://...` và `git@host:owner/repo.git`) hoặc đường dẫn cục bộ.

Skill manifest (`SKILL.toml`) hỗ trợ `prompts` và `[[tools]]`; cả hai được đưa vào system prompt của agent khi chạy, giúp model có thể tuân theo hướng dẫn skill mà không cần đọc thủ công.

### `migrate`

- `jhedaiclaw migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `jhedaiclaw config schema`

`config schema` xuất JSON Schema (draft 2020-12) cho toàn bộ hợp đồng `config.toml` ra stdout.

### `completions`

- `jhedaiclaw completions bash`
- `jhedaiclaw completions fish`
- `jhedaiclaw completions zsh`
- `jhedaiclaw completions powershell`
- `jhedaiclaw completions elvish`

`completions` chỉ xuất ra stdout để script có thể được source trực tiếp mà không bị lẫn log/cảnh báo.

### `hardware`

- `jhedaiclaw hardware discover`
- `jhedaiclaw hardware introspect <path>`
- `jhedaiclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `jhedaiclaw peripheral list`
- `jhedaiclaw peripheral add <board> <path>`
- `jhedaiclaw peripheral flash [--port <serial_port>]`
- `jhedaiclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `jhedaiclaw peripheral flash-nucleo`

## Kiểm tra nhanh

Để xác minh nhanh tài liệu với binary hiện tại:

```bash
jhedaiclaw --help
jhedaiclaw <command> --help
```
