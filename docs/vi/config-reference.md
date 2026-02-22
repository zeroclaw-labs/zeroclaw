# Tham khảo cấu hình ZeroClaw

Các mục cấu hình thường dùng và giá trị mặc định.

Xác minh lần cuối: **2026-02-19**.

Thứ tự tìm config khi khởi động:

1. Biến `ZEROCLAW_WORKSPACE` (nếu được đặt)
2. Marker `~/.zeroclaw/active_workspace.toml` (nếu có)
3. Mặc định `~/.zeroclaw/config.toml`

ZeroClaw ghi log đường dẫn config đã giải quyết khi khởi động ở mức `INFO`:

- `Config loaded` với các trường: `path`, `workspace`, `source`, `initialized`

Lệnh xuất schema:

- `zeroclaw config schema` (xuất JSON Schema draft 2020-12 ra stdout)

## Khóa chính

| Khóa | Mặc định | Ghi chú |
|---|---|---|
| `default_provider` | `openrouter` | ID hoặc bí danh provider |
| `default_model` | `anthropic/claude-sonnet-4-6` | Model định tuyến qua provider đã chọn |
| `default_temperature` | `0.7` | Nhiệt độ model |
| `model_support_vision` | chưa đặt (`None`) | Ghi đè hỗ trợ vision cho provider/model đang dùng |

Lưu ý:

- `model_support_vision = true` bật vision (ví dụ Ollama chạy `llava`).
- `model_support_vision = false` tắt vision.
- Để trống giữ mặc định của provider.
- Biến môi trường: `ZEROCLAW_MODEL_SUPPORT_VISION` hoặc `MODEL_SUPPORT_VISION` (giá trị: `true`/`false`/`1`/`0`/`yes`/`no`/`on`/`off`).

## `[observability]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `backend` | `none` | Backend quan sát: `none`, `noop`, `log`, `prometheus`, `otel`, `opentelemetry` hoặc `otlp` |
| `otel_endpoint` | `http://localhost:4318` | Endpoint OTLP HTTP khi backend là `otel` |
| `otel_service_name` | `zeroclaw` | Tên dịch vụ gửi đến OTLP collector |

Lưu ý:

- `backend = "otel"` dùng OTLP HTTP export với blocking exporter client để span và metric có thể được gửi an toàn từ context ngoài Tokio.
- Bí danh `opentelemetry` và `otlp` trỏ đến cùng backend OTel.

Ví dụ:

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
```

## Ghi đè provider qua biến môi trường

Provider cũng có thể chọn qua biến môi trường. Thứ tự ưu tiên:

1. `ZEROCLAW_PROVIDER` (ghi đè tường minh, luôn thắng khi có giá trị)
2. `PROVIDER` (dự phòng kiểu cũ, chỉ áp dụng khi provider trong config chưa đặt hoặc vẫn là `openrouter`)
3. `default_provider` trong `config.toml`

Lưu ý cho người dùng container:

- Nếu `config.toml` đặt provider tùy chỉnh như `custom:https://.../v1`, biến `PROVIDER=openrouter` mặc định từ Docker/container sẽ không thay thế nó.
- Dùng `ZEROCLAW_PROVIDER` khi cố ý muốn biến môi trường ghi đè provider đã cấu hình.

## `[agent]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `compact_context` | `false` | Khi bật: bootstrap_max_chars=6000, rag_chunk_limit=2. Dùng cho model 13B trở xuống |
| `max_tool_iterations` | `10` | Số vòng lặp tool-call tối đa mỗi tin nhắn trên CLI, gateway và channels |
| `max_history_messages` | `50` | Số tin nhắn lịch sử tối đa giữ lại mỗi phiên |
| `parallel_tools` | `false` | Bật thực thi tool song song trong một lượt |
| `tool_dispatcher` | `auto` | Chiến lược dispatch tool |

Lưu ý:

- Đặt `max_tool_iterations = 0` sẽ dùng giá trị mặc định an toàn `10`.
- Nếu tin nhắn kênh vượt giá trị này, runtime trả về: `Agent exceeded maximum tool iterations (<value>)`.
- Trong vòng lặp tool của CLI, gateway và channel, các lời gọi tool độc lập được thực thi đồng thời mặc định khi không cần phê duyệt; thứ tự kết quả giữ ổn định.
- `parallel_tools` áp dụng cho API `Agent::turn()`. Không ảnh hưởng đến vòng lặp runtime của CLI, gateway hay channel.

## `[agents.<name>]`

Cấu hình agent phụ (sub-agent). Mỗi khóa dưới `[agents]` định nghĩa một agent phụ có tên mà agent chính có thể ủy quyền.

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `provider` | _bắt buộc_ | Tên provider (ví dụ `"ollama"`, `"openrouter"`, `"anthropic"`) |
| `model` | _bắt buộc_ | Tên model cho agent phụ |
| `system_prompt` | chưa đặt | System prompt tùy chỉnh cho agent phụ (tùy chọn) |
| `api_key` | chưa đặt | API key tùy chỉnh (mã hóa khi `secrets.encrypt = true`) |
| `temperature` | chưa đặt | Temperature tùy chỉnh cho agent phụ |
| `max_depth` | `3` | Độ sâu đệ quy tối đa cho ủy quyền lồng nhau |
| `agentic` | `false` | Bật chế độ vòng lặp tool-call nhiều lượt cho agent phụ |
| `allowed_tools` | `[]` | Danh sách tool được phép ở chế độ agentic |
| `max_iterations` | `10` | Số vòng tool-call tối đa cho chế độ agentic |

Lưu ý:

- `agentic = false` giữ nguyên hành vi ủy quyền prompt→response đơn lượt.
- `agentic = true` yêu cầu ít nhất một mục khớp trong `allowed_tools`.
- Tool `delegate` bị loại khỏi allowlist của agent phụ để tránh vòng lặp ủy quyền.

```toml
[agents.researcher]
provider = "openrouter"
model = "anthropic/claude-sonnet-4-6"
system_prompt = "You are a research assistant."
max_depth = 2
agentic = true
allowed_tools = ["web_search", "http_request", "file_read"]
max_iterations = 8

[agents.coder]
provider = "ollama"
model = "qwen2.5-coder:32b"
temperature = 0.2
```

## `[runtime]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `reasoning_enabled` | chưa đặt (`None`) | Ghi đè toàn cục cho reasoning/thinking trên provider hỗ trợ |

Lưu ý:

- `reasoning_enabled = false` tắt tường minh reasoning phía provider cho provider hỗ trợ (hiện tại `ollama`, qua trường `think: false`).
- `reasoning_enabled = true` yêu cầu reasoning tường minh (`think: true` trên `ollama`).
- Để trống giữ mặc định của provider.

## `[skills]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `open_skills_enabled` | `false` | Cho phép tải/đồng bộ kho `open-skills` cộng đồng |
| `open_skills_dir` | chưa đặt | Đường dẫn cục bộ cho `open-skills` (mặc định `$HOME/open-skills` khi bật) |

Lưu ý:

- Mặc định an toàn: ZeroClaw **không** clone hay đồng bộ `open-skills` trừ khi `open_skills_enabled = true`.
- Ghi đè qua biến môi trường:
  - `ZEROCLAW_OPEN_SKILLS_ENABLED` chấp nhận `1/0`, `true/false`, `yes/no`, `on/off`.
  - `ZEROCLAW_OPEN_SKILLS_DIR` ghi đè đường dẫn kho khi có giá trị.
- Thứ tự ưu tiên: `ZEROCLAW_OPEN_SKILLS_ENABLED` → `skills.open_skills_enabled` trong `config.toml` → mặc định `false`.

## `[composio]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `enabled` | `false` | Bật công cụ OAuth do Composio quản lý |
| `api_key` | chưa đặt | API key Composio cho tool `composio` |
| `entity_id` | `default` | `user_id` mặc định gửi khi gọi connect/execute |

Lưu ý:

- Tương thích ngược: `enable = true` kiểu cũ được chấp nhận như bí danh cho `enabled = true`.
- Nếu `enabled = false` hoặc thiếu `api_key`, tool `composio` không được đăng ký.
- ZeroClaw yêu cầu Composio v3 tools với `toolkit_versions=latest` và thực thi với `version="latest"` để tránh bản tool mặc định cũ.
- Luồng thông thường: gọi `connect`, hoàn tất OAuth trên trình duyệt, rồi chạy `execute` cho hành động mong muốn.
- Nếu Composio trả lỗi thiếu connected-account, gọi `list_accounts` (tùy chọn với `app`) và truyền `connected_account_id` trả về cho `execute`.

## `[cost]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `enabled` | `false` | Bật theo dõi chi phí |
| `daily_limit_usd` | `10.00` | Giới hạn chi tiêu hàng ngày (USD) |
| `monthly_limit_usd` | `100.00` | Giới hạn chi tiêu hàng tháng (USD) |
| `warn_at_percent` | `80` | Cảnh báo khi chi tiêu đạt tỷ lệ phần trăm này |
| `allow_override` | `false` | Cho phép vượt ngân sách khi dùng cờ `--override` |

Lưu ý:

- Khi `enabled = true`, runtime theo dõi ước tính chi phí mỗi yêu cầu và áp dụng giới hạn ngày/tháng.
- Tại ngưỡng `warn_at_percent`, cảnh báo được gửi nhưng yêu cầu vẫn tiếp tục.
- Khi đạt giới hạn, yêu cầu bị từ chối trừ khi `allow_override = true` và cờ `--override` được truyền.

## `[identity]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `format` | `openclaw` | Định dạng danh tính: `"openclaw"` (mặc định) hoặc `"aieos"` |
| `aieos_path` | chưa đặt | Đường dẫn file AIEOS JSON (tương đối với workspace) |
| `aieos_inline` | chưa đặt | AIEOS JSON nội tuyến (thay thế cho đường dẫn file) |

Lưu ý:

- Dùng `format = "aieos"` với `aieos_path` hoặc `aieos_inline` để tải tài liệu danh tính AIEOS / OpenClaw.
- Chỉ nên đặt một trong hai `aieos_path` hoặc `aieos_inline`; `aieos_path` được ưu tiên.

## `[multimodal]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `max_images` | `4` | Số marker ảnh tối đa mỗi yêu cầu |
| `max_image_size_mb` | `5` | Giới hạn kích thước ảnh trước khi mã hóa base64 |
| `allow_remote_fetch` | `false` | Cho phép tải ảnh từ URL `http(s)` trong marker |

Lưu ý:

- Runtime chấp nhận marker ảnh trong tin nhắn với cú pháp: ``[IMAGE:<source>]``.
- Nguồn hỗ trợ:
  - Đường dẫn file cục bộ (ví dụ ``[IMAGE:/tmp/screenshot.png]``)
- Data URI (ví dụ ``[IMAGE:data:image/png;base64,...]``)
- URL từ xa chỉ khi `allow_remote_fetch = true`
- Kiểu MIME cho phép: `image/png`, `image/jpeg`, `image/webp`, `image/gif`, `image/bmp`.
- Khi provider đang dùng không hỗ trợ vision, yêu cầu thất bại với lỗi capability có cấu trúc (`capability=vision`) thay vì bỏ qua ảnh.

## `[browser]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `enabled` | `false` | Bật tool `browser_open` (mở URL trong trình duyệt mặc định hệ thống, không thu thập dữ liệu) |
| `allowed_domains` | `[]` | Tên miền cho phép cho `browser_open` (khớp chính xác hoặc subdomain) |
| `session_name` | chưa đặt | Tên phiên trình duyệt (cho tự động hóa agent-browser) |
| `backend` | `agent_browser` | Backend tự động hóa: `"agent_browser"`, `"rust_native"`, `"computer_use"` hoặc `"auto"` |
| `native_headless` | `true` | Chế độ headless cho backend rust-native |
| `native_webdriver_url` | `http://127.0.0.1:9515` | URL endpoint WebDriver cho backend rust-native |
| `native_chrome_path` | chưa đặt | Đường dẫn Chrome/Chromium tùy chọn cho backend rust-native |

### `[browser.computer_use]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `endpoint` | `http://127.0.0.1:8787/v1/actions` | Endpoint sidecar cho hành động computer-use (chuột/bàn phím/screenshot cấp OS) |
| `api_key` | chưa đặt | Bearer token tùy chọn cho sidecar computer-use (mã hóa khi lưu) |
| `timeout_ms` | `15000` | Thời gian chờ mỗi hành động (mili giây) |
| `allow_remote_endpoint` | `false` | Cho phép endpoint từ xa/công khai cho sidecar |
| `window_allowlist` | `[]` | Danh sách cho phép tiêu đề cửa sổ/tiến trình gửi đến sidecar |
| `max_coordinate_x` | chưa đặt | Giới hạn trục X cho hành động dựa trên tọa độ (tùy chọn) |
| `max_coordinate_y` | chưa đặt | Giới hạn trục Y cho hành động dựa trên tọa độ (tùy chọn) |

Lưu ý:

- Khi `backend = "computer_use"`, agent ủy quyền hành động trình duyệt cho sidecar tại `computer_use.endpoint`.
- `allow_remote_endpoint = false` (mặc định) từ chối mọi endpoint không phải loopback để tránh lộ ra ngoài.
- Dùng `window_allowlist` để giới hạn cửa sổ OS mà sidecar có thể tương tác.

## `[http_request]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `enabled` | `false` | Bật tool `http_request` cho tương tác API |
| `allowed_domains` | `[]` | Tên miền cho phép (khớp chính xác hoặc subdomain) |
| `max_response_size` | `1000000` | Kích thước response tối đa (byte, mặc định: 1 MB) |
| `timeout_secs` | `30` | Thời gian chờ yêu cầu (giây) |

Lưu ý:

- Mặc định từ chối tất cả: nếu `allowed_domains` rỗng, mọi yêu cầu HTTP bị từ chối.
- Dùng khớp tên miền chính xác hoặc subdomain (ví dụ `"api.example.com"`, `"example.com"`).

## `[gateway]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `host` | `127.0.0.1` | Địa chỉ bind |
| `port` | `3000` | Cổng lắng nghe gateway |
| `require_pairing` | `true` | Yêu cầu ghép nối trước khi xác thực bearer |
| `allow_public_bind` | `false` | Chặn lộ public do vô ý |

## `[gateway.node_control]` (thử nghiệm)

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `enabled` | `false` | Bật endpoint scaffold node-control (`POST /api/node-control`) |
| `auth_token` | `null` | Shared token bổ sung, kiểm qua header `X-Node-Control-Token` |
| `allowed_node_ids` | `[]` | Allowlist cho `node.describe`/`node.invoke` (`[]` = chấp nhận mọi node) |

## `[autonomy]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `level` | `supervised` | `read_only`, `supervised` hoặc `full` |
| `workspace_only` | `true` | Giới hạn ghi/lệnh trong phạm vi workspace |
| `allowed_commands` | _bắt buộc để chạy shell_ | Danh sách lệnh được phép |
| `forbidden_paths` | `[]` | Danh sách đường dẫn bị cấm |
| `max_actions_per_hour` | `100` | Ngân sách hành động mỗi giờ |
| `max_cost_per_day_cents` | `1000` | Giới hạn chi tiêu mỗi ngày (cent) |
| `require_approval_for_medium_risk` | `true` | Yêu cầu phê duyệt cho lệnh rủi ro trung bình |
| `block_high_risk_commands` | `true` | Chặn cứng lệnh rủi ro cao |
| `auto_approve` | `[]` | Thao tác tool luôn được tự động phê duyệt |
| `always_ask` | `[]` | Thao tác tool luôn yêu cầu phê duyệt |

Lưu ý:

- `level = "full"` bỏ qua phê duyệt rủi ro trung bình cho shell execution, nhưng vẫn áp dụng guardrail đã cấu hình.
- Phân tích toán tử/dấu phân cách shell nhận biết dấu ngoặc kép. Ký tự như `;` trong đối số được trích dẫn được xử lý là ký tự, không phải dấu phân cách lệnh.
- Toán tử chuỗi shell không trích dẫn vẫn được kiểm tra bởi policy (`;`, `|`, `&&`, `||`, chạy nền và chuyển hướng).

## `[memory]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `backend` | `sqlite` | `sqlite`, `lucid`, `markdown`, `none` |
| `auto_save` | `true` | Chỉ lưu đầu vào người dùng (đầu ra assistant bị loại) |
| `embedding_provider` | `none` | `none`, `openai` hoặc endpoint tùy chỉnh |
| `embedding_model` | `text-embedding-3-small` | ID model embedding, hoặc tuyến `hint:<name>` |
| `embedding_dimensions` | `1536` | Kích thước vector mong đợi cho model embedding đã chọn |
| `vector_weight` | `0.7` | Trọng số vector trong xếp hạng kết hợp |
| `keyword_weight` | `0.3` | Trọng số từ khóa trong xếp hạng kết hợp |

Lưu ý:

- Chèn ngữ cảnh memory bỏ qua khóa auto-save `assistant_resp*` kiểu cũ để tránh tóm tắt do model tạo bị coi là sự thật.

## `[[model_routes]]` và `[[embedding_routes]]`

Route hint giúp tên tích hợp ổn định khi model ID thay đổi.

### `[[model_routes]]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `hint` | _bắt buộc_ | Tên hint tác vụ (ví dụ `"reasoning"`, `"fast"`, `"code"`, `"summarize"`) |
| `provider` | _bắt buộc_ | Provider đích (phải khớp tên provider đã biết) |
| `model` | _bắt buộc_ | Model sử dụng với provider đó |
| `api_key` | chưa đặt | API key tùy chỉnh cho provider của route này (tùy chọn) |

### `[[embedding_routes]]`

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `hint` | _bắt buộc_ | Tên route hint (ví dụ `"semantic"`, `"archive"`, `"faq"`) |
| `provider` | _bắt buộc_ | Embedding provider (`"none"`, `"openai"` hoặc `"custom:<url>"`) |
| `model` | _bắt buộc_ | Model embedding sử dụng với provider đó |
| `dimensions` | chưa đặt | Ghi đè kích thước embedding cho route này (tùy chọn) |
| `api_key` | chưa đặt | API key tùy chỉnh cho provider của route này (tùy chọn) |

```toml
[memory]
embedding_model = "hint:semantic"

[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "provider/model-id"

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536
```

Chiến lược nâng cấp:

1. Giữ hint ổn định (`hint:reasoning`, `hint:semantic`).
2. Chỉ cập nhật `model = "...phiên-bản-mới..."` trong mục route.
3. Kiểm tra bằng `zeroclaw doctor` trước khi khởi động lại/triển khai.

## `[query_classification]`

Tự động định tuyến tin nhắn đến hint `[[model_routes]]` theo mẫu nội dung.

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `enabled` | `false` | Bật phân loại truy vấn tự động |
| `rules` | `[]` | Quy tắc phân loại (đánh giá theo thứ tự ưu tiên) |

Mỗi rule trong `rules`:

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `hint` | _bắt buộc_ | Phải khớp giá trị hint trong `[[model_routes]]` |
| `keywords` | `[]` | Khớp chuỗi con không phân biệt hoa thường |
| `patterns` | `[]` | Khớp chuỗi chính xác phân biệt hoa thường (cho code fence, từ khóa như `"fn "`) |
| `min_length` | chưa đặt | Chỉ khớp nếu độ dài tin nhắn ≥ N ký tự |
| `max_length` | chưa đặt | Chỉ khớp nếu độ dài tin nhắn ≤ N ký tự |
| `priority` | `0` | Rule ưu tiên cao hơn được kiểm tra trước |

```toml
[query_classification]
enabled = true

[[query_classification.rules]]
hint = "reasoning"
keywords = ["explain", "analyze", "why"]
min_length = 200
priority = 10

[[query_classification.rules]]
hint = "fast"
keywords = ["hi", "hello", "thanks"]
max_length = 50
priority = 5
```

## `[channels_config]`

Cấu hình kênh cấp cao nằm dưới `channels_config`.

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `message_timeout_secs` | `300` | Thời gian chờ cơ bản (giây) cho xử lý tin nhắn kênh; runtime tự điều chỉnh theo độ sâu tool-loop (lên đến 4x) |

Ví dụ:

- `[channels_config.telegram]`
- `[channels_config.discord]`
- `[channels_config.whatsapp]`
- `[channels_config.email]`

Lưu ý:

- Mặc định `300s` tối ưu cho LLM chạy cục bộ (Ollama) vốn chậm hơn cloud API.
- Ngân sách timeout runtime là `message_timeout_secs * scale`, trong đó `scale = min(max_tool_iterations, 4)` và tối thiểu `1`.
- Việc điều chỉnh này tránh timeout sai khi lượt LLM đầu chậm/retry nhưng các lượt tool-loop sau vẫn cần hoàn tất.
- Nếu dùng cloud API (OpenAI, Anthropic, v.v.), có thể giảm xuống `60` hoặc thấp hơn.
- Giá trị dưới `30` bị giới hạn thành `30` để tránh timeout liên tục.
- Khi timeout xảy ra, người dùng nhận: `⚠️ Request timed out while waiting for the model. Please try again.`
- Hành vi ngắt chỉ Telegram được điều khiển bằng `channels_config.telegram.interrupt_on_new_message` (mặc định `false`).
  Khi bật, tin nhắn mới từ cùng người gửi trong cùng chat sẽ hủy yêu cầu đang xử lý và giữ ngữ cảnh người dùng bị ngắt.
- Khi `zeroclaw channel start` đang chạy, thay đổi `default_provider`, `default_model`, `default_temperature`, `api_key`, `api_url` và `reliability.*` được áp dụng nóng từ `config.toml` ở tin nhắn tiếp theo.

Xem ma trận kênh và hành vi allowlist chi tiết tại [channels-reference.md](channels-reference.md).

### `[channels_config.whatsapp]`

WhatsApp hỗ trợ hai backend dưới cùng một bảng config.

Chế độ Cloud API (webhook Meta):

| Khóa | Bắt buộc | Mục đích |
|---|---|---|
| `access_token` | Có | Bearer token Meta Cloud API |
| `phone_number_id` | Có | ID số điện thoại Meta |
| `verify_token` | Có | Token xác minh webhook |
| `app_secret` | Tùy chọn | Bật xác minh chữ ký webhook (`X-Hub-Signature-256`) |
| `allowed_numbers` | Khuyến nghị | Số điện thoại cho phép gửi đến (`[]` = từ chối tất cả, `"*"` = cho phép tất cả) |

Chế độ WhatsApp Web (client gốc):

| Khóa | Bắt buộc | Mục đích |
|---|---|---|
| `session_path` | Có | Đường dẫn phiên SQLite lưu trữ lâu dài |
| `pair_phone` | Tùy chọn | Số điện thoại cho luồng pair-code (chỉ chữ số) |
| `pair_code` | Tùy chọn | Mã pair tùy chỉnh (nếu không sẽ tự tạo) |
| `allowed_numbers` | Khuyến nghị | Số điện thoại cho phép gửi đến (`[]` = từ chối tất cả, `"*"` = cho phép tất cả) |

Lưu ý:

- WhatsApp Web yêu cầu build flag `whatsapp-web`.
- Nếu cả Cloud lẫn Web đều có cấu hình, Cloud được ưu tiên để tương thích ngược.

## `[hardware]`

Cấu hình truy cập phần cứng vật lý (STM32, probe, serial).

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `enabled` | `false` | Bật truy cập phần cứng |
| `transport` | `none` | Chế độ truyền: `"none"`, `"native"`, `"serial"` hoặc `"probe"` |
| `serial_port` | chưa đặt | Đường dẫn cổng serial (ví dụ `"/dev/ttyACM0"`) |
| `baud_rate` | `115200` | Tốc độ baud serial |
| `probe_target` | chưa đặt | Chip đích cho probe (ví dụ `"STM32F401RE"`) |
| `workspace_datasheets` | `false` | Bật RAG datasheet workspace (đánh chỉ mục PDF schematic để AI tra cứu chân) |

Lưu ý:

- Dùng `transport = "serial"` với `serial_port` cho kết nối USB-serial.
- Dùng `transport = "probe"` với `probe_target` cho nạp qua debug-probe (ví dụ ST-Link).
- Xem [hardware-peripherals-design.md](hardware-peripherals-design.md) để biết chi tiết giao thức.

## `[peripherals]`

Bo mạch ngoại vi trở thành tool agent khi được bật.

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `enabled` | `false` | Bật hỗ trợ ngoại vi (bo mạch trở thành tool agent) |
| `boards` | `[]` | Danh sách cấu hình bo mạch |
| `datasheet_dir` | chưa đặt | Đường dẫn tài liệu datasheet (tương đối workspace) cho RAG |

Mỗi mục trong `boards`:

| Khóa | Mặc định | Mục đích |
|---|---|---|
| `board` | _bắt buộc_ | Loại bo mạch: `"nucleo-f401re"`, `"rpi-gpio"`, `"esp32"`, v.v. |
| `transport` | `serial` | Kiểu truyền: `"serial"`, `"native"`, `"websocket"` |
| `path` | chưa đặt | Đường dẫn serial: `"/dev/ttyACM0"`, `"/dev/ttyUSB0"` |
| `baud` | `115200` | Tốc độ baud cho serial |

```toml
[peripherals]
enabled = true
datasheet_dir = "docs/datasheets"

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"
```

Lưu ý:

- Đặt file `.md`/`.txt` datasheet đặt tên theo bo mạch (ví dụ `nucleo-f401re.md`, `rpi-gpio.md`) trong `datasheet_dir` cho RAG.
- Xem [hardware-peripherals-design.md](hardware-peripherals-design.md) để biết giao thức bo mạch và ghi chú firmware.

## Giá trị mặc định liên quan bảo mật

- Allowlist kênh mặc định từ chối tất cả (`[]` nghĩa là từ chối tất cả)
- Gateway mặc định yêu cầu ghép nối
- Mặc định chặn public bind

## Lệnh kiểm tra

Sau khi chỉnh config:

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
zeroclaw service restart
```

## Tài liệu liên quan

- [channels-reference.md](channels-reference.md)
- [providers-reference.md](providers-reference.md)
- [operations-runbook.md](operations-runbook.md)
- [troubleshooting.md](troubleshooting.md)
