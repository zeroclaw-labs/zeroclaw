# Tài liệu tham khảo Channels

Tài liệu này là nguồn tham khảo chính thức về cấu hình channel trong ZeroClaw.

Với các phòng Matrix được mã hóa, xem hướng dẫn chuyên biệt:
- [Hướng dẫn Matrix E2EE](matrix-e2ee-guide.md)

## Truy cập nhanh

- Cần tham khảo config đầy đủ theo từng channel: xem mục `## 4. Ví dụ cấu hình theo từng channel`.
- Cần chẩn đoán khi không nhận được phản hồi: xem mục `## 6. Danh sách kiểm tra xử lý sự cố`.
- Cần hỗ trợ phòng Matrix được mã hóa: dùng [Hướng dẫn Matrix E2EE](matrix-e2ee-guide.md).
- Cần thông tin triển khai/mạng (polling vs webhook): dùng [Network Deployment](network-deployment.md).

## FAQ: Cấu hình Matrix thành công nhưng không có phản hồi

Đây là triệu chứng phổ biến nhất (cùng loại với issue #499). Kiểm tra theo thứ tự sau:

1. **Allowlist không khớp**: `allowed_users` không bao gồm người gửi (hoặc để trống).
2. **Room đích sai**: bot chưa tham gia room được cấu hình `room_id` / alias.
3. **Token/tài khoản không khớp**: token hợp lệ nhưng thuộc tài khoản Matrix khác.
4. **Thiếu E2EE device identity**: `whoami` không trả về `device_id` và config không cung cấp giá trị này.
5. **Thiếu key sharing/trust**: các khóa room chưa được chia sẻ cho thiết bị bot, nên không thể giải mã sự kiện mã hóa.
6. **Trạng thái runtime cũ**: config đã thay đổi nhưng `zeroclaw daemon` chưa được khởi động lại.

---

## 1. Namespace cấu hình

Tất cả cài đặt channel nằm trong `channels_config` trong `~/.zeroclaw/config.toml`.

```toml
[channels_config]
cli = true
```

Mỗi channel được bật bằng cách tạo sub-table tương ứng (ví dụ: `[channels_config.telegram]`).

## Chuyển đổi model runtime trong chat (Telegram / Discord)

Khi chạy `zeroclaw channel start` (hoặc chế độ daemon), Telegram và Discord hỗ trợ chuyển đổi runtime theo phạm vi người gửi:

- `/models` — hiển thị các provider hiện có và lựa chọn hiện tại
- `/models <provider>` — chuyển provider cho phiên người gửi hiện tại
- `/model` — hiển thị model hiện tại và các model ID đã cache (nếu có)
- `/model <model-id>` — chuyển model cho phiên người gửi hiện tại

Lưu ý:

- Việc chuyển đổi chỉ xóa lịch sử hội thoại trong bộ nhớ của người gửi đó, tránh ô nhiễm ngữ cảnh giữa các model.
- Xem trước bộ nhớ cache model từ `zeroclaw models refresh --provider <ID>`.
- Đây là lệnh chat runtime, không phải lệnh con CLI.

## Giao thức marker hình ảnh đầu vào

ZeroClaw hỗ trợ đầu vào multimodal qua các marker nội tuyến trong tin nhắn:

- Cú pháp: ``[IMAGE:<source>]``
- `<source>` có thể là:
  - Đường dẫn file cục bộ
  - Data URI (`data:image/...;base64,...`)
  - URL từ xa chỉ khi `[multimodal].allow_remote_fetch = true`

Lưu ý vận hành:

- Marker được phân tích trong các tin nhắn người dùng trước khi gọi provider.
- Capability của provider được kiểm tra tại runtime: nếu provider không hỗ trợ vision, request thất bại với lỗi capability có cấu trúc (`capability=vision`).
- Các phần `media` của Linq webhook có MIME type `image/*` được tự động chuyển đổi sang định dạng marker này.

## Channel Matrix

### Tùy chọn Build Feature (`channel-matrix`, `channel-lark`)

Hỗ trợ Matrix và Lark/Feishu được kiểm soát tại thời điểm biên dịch bằng Cargo features.

- Bản build mặc định bao gồm Lark/Feishu (`default = ["channel-lark"]`), còn Matrix là opt-in.
- Để lặp lại nhanh hơn khi không cần Matrix/Lark:

```bash
cargo check --no-default-features --features hardware
```

- Để bật tường minh hỗ trợ Matrix trong feature set tùy chỉnh:

```bash
cargo check --no-default-features --features hardware,channel-matrix
```

- Để bật tường minh hỗ trợ Lark/Feishu trong feature set tùy chỉnh:

```bash
cargo check --no-default-features --features hardware,channel-lark
```

Nếu `[channels_config.matrix]`, `[channels_config.lark]`, hoặc `[channels_config.feishu]` có mặt nhưng binary được build mà không có feature tương ứng, các lệnh `zeroclaw channel list`, `zeroclaw channel doctor`, và `zeroclaw channel start` sẽ ghi log rằng channel đó bị bỏ qua có chủ ý trong bản build này.

---

## 2. Chế độ phân phối tóm tắt

| Channel | Chế độ nhận | Cần cổng inbound công khai? |
|---|---|---|
| CLI | local stdin/stdout | Không |
| Telegram | polling | Không |
| Discord | gateway/websocket | Không |
| Slack | events API | Không (luồng token-based) |
| Mattermost | polling | Không |
| Matrix | sync API (hỗ trợ E2EE) | Không |
| Signal | signal-cli HTTP bridge | Không (endpoint bridge cục bộ) |
| WhatsApp | webhook (Cloud API) hoặc websocket (Web mode) | Cloud API: Có (HTTPS callback công khai), Web mode: Không |
| Webhook | gateway endpoint (`/webhook`) | Thường là có |
| Email | IMAP polling + SMTP send | Không |
| IRC | IRC socket | Không |
| Lark/Feishu | websocket (mặc định) hoặc webhook | Chỉ ở chế độ Webhook |
| DingTalk | stream mode | Không |
| QQ | bot gateway | Không |
| iMessage | tích hợp cục bộ | Không |

---

## 3. Ngữ nghĩa allowlist

Với các channel có allowlist người gửi:

- Allowlist trống: từ chối tất cả tin nhắn đầu vào.
- `"*"`: cho phép tất cả người gửi (chỉ dùng để xác minh tạm thời).
- Danh sách tường minh: chỉ cho phép những người gửi được liệt kê.

Tên trường khác nhau theo channel:

- `allowed_users` (Telegram/Discord/Slack/Mattermost/Matrix/IRC/Lark/DingTalk/QQ)
- `allowed_from` (Signal)
- `allowed_numbers` (WhatsApp)
- `allowed_senders` (Email)
- `allowed_contacts` (iMessage)

---

## 4. Ví dụ cấu hình theo từng channel

### 4.1 Telegram

```toml
[channels_config.telegram]
bot_token = "123456:telegram-token"
allowed_users = ["*"]
stream_mode = "off"               # tùy chọn: off | partial
draft_update_interval_ms = 1000   # tùy chọn: giới hạn tần suất chỉnh sửa khi streaming một phần
mention_only = false              # tùy chọn: yêu cầu @mention trong nhóm
interrupt_on_new_message = false  # tùy chọn: hủy yêu cầu đang xử lý cùng người gửi cùng chat
```

Lưu ý về Telegram:

- `interrupt_on_new_message = true` giữ lại các lượt người dùng bị gián đoạn trong lịch sử hội thoại, sau đó khởi động lại việc tạo nội dung với tin nhắn mới nhất.
- Phạm vi gián đoạn rất chặt chẽ: cùng người gửi trong cùng chat. Tin nhắn từ các chat khác nhau được xử lý độc lập.

### 4.2 Discord

```toml
[channels_config.discord]
bot_token = "discord-bot-token"
guild_id = "123456789012345678"   # tùy chọn
allowed_users = ["*"]
listen_to_bots = false
mention_only = false
```

### 4.3 Slack

```toml
[channels_config.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."             # tùy chọn
channel_id = "C1234567890"         # tùy chọn
allowed_users = ["*"]
```

### 4.4 Mattermost

```toml
[channels_config.mattermost]
url = "https://mm.example.com"
bot_token = "mattermost-token"
channel_id = "channel-id"          # bắt buộc để lắng nghe
allowed_users = ["*"]
```

### 4.5 Matrix

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_..."
user_id = "@zeroclaw:matrix.example.com"   # tùy chọn, khuyến nghị cho E2EE
device_id = "DEVICEID123"                  # tùy chọn, khuyến nghị cho E2EE
room_id = "!room:matrix.example.com"       # hoặc room alias (#ops:matrix.example.com)
allowed_users = ["*"]
```

Xem [Hướng dẫn Matrix E2EE](matrix-e2ee-guide.md) để xử lý sự cố phòng mã hóa.

### 4.6 Signal

```toml
[channels_config.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
group_id = "dm"                    # tùy chọn: "dm" / group id / bỏ qua
allowed_from = ["*"]
ignore_attachments = false
ignore_stories = true
```

### 4.7 WhatsApp

ZeroClaw hỗ trợ hai backend WhatsApp:

- **Chế độ Cloud API** (`phone_number_id` + `access_token` + `verify_token`)
- **Chế độ WhatsApp Web** (`session_path`, yêu cầu build flag `--features whatsapp-web`)

Chế độ Cloud API:

```toml
[channels_config.whatsapp]
access_token = "EAAB..."
phone_number_id = "123456789012345"
verify_token = "your-verify-token"
app_secret = "your-app-secret"     # tùy chọn nhưng được khuyến nghị
allowed_numbers = ["*"]
```

Chế độ WhatsApp Web:

```toml
[channels_config.whatsapp]
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
pair_phone = "15551234567"         # tùy chọn; bỏ qua để dùng QR flow
pair_code = ""                     # tùy chọn pair code tùy chỉnh
allowed_numbers = ["*"]
```

Lưu ý:

- Build với `cargo build --features whatsapp-web` (hoặc lệnh run tương đương).
- Giữ `session_path` trên bộ nhớ lưu trữ bền vững để tránh phải liên kết lại sau khi khởi động lại.
- Định tuyến trả lời sử dụng JID của chat nguồn, vì vậy cả trả lời trực tiếp và nhóm đều hoạt động đúng.

### 4.8 Cấu hình Webhook Channel (Gateway)

`channels_config.webhook` bật hành vi gateway đặc thù cho webhook.

```toml
[channels_config.webhook]
port = 8080
secret = "optional-shared-secret"
```

Chạy với gateway/daemon và xác minh `/health`.

### 4.9 Email

```toml
[channels_config.email]
imap_host = "imap.example.com"
imap_port = 993
imap_folder = "INBOX"
smtp_host = "smtp.example.com"
smtp_port = 465
smtp_tls = true
username = "bot@example.com"
password = "email-password"
from_address = "bot@example.com"
poll_interval_secs = 60
allowed_senders = ["*"]
```

### 4.10 IRC

```toml
[channels_config.irc]
server = "irc.libera.chat"
port = 6697
nickname = "zeroclaw-bot"
username = "zeroclaw"              # tùy chọn
channels = ["#zeroclaw"]
allowed_users = ["*"]
server_password = ""                # tùy chọn
nickserv_password = ""              # tùy chọn
sasl_password = ""                  # tùy chọn
verify_tls = true
```

### 4.11 Lark / Feishu

```toml
[channels_config.lark]
app_id = "your_lark_app_id"
app_secret = "your_lark_app_secret"
encrypt_key = ""                    # tùy chọn
verification_token = ""             # tùy chọn
allowed_users = ["*"]
use_feishu = false
receive_mode = "websocket"          # hoặc "webhook"
port = 8081                          # bắt buộc ở chế độ webhook
```

Hỗ trợ onboarding tương tác:

```bash
zeroclaw onboard --interactive
```

Trình hướng dẫn bao gồm bước **Lark/Feishu** chuyên biệt với:

- Chọn khu vực (`Feishu (CN)` hoặc `Lark (International)`)
- Xác minh thông tin xác thực với endpoint auth của Open Platform chính thức
- Chọn chế độ nhận (`websocket` hoặc `webhook`)
- Tùy chọn nhập verification token webhook (khuyến nghị để tăng cường kiểm tra tính xác thực của callback)

Hành vi token runtime:

- `tenant_access_token` được cache với thời hạn làm mới dựa trên `expire`/`expires_in` từ phản hồi xác thực.
- Các yêu cầu gửi tự động thử lại một lần sau khi token bị vô hiệu hóa khi Feishu/Lark trả về HTTP `401` hoặc mã lỗi nghiệp vụ `99991663` (`Invalid access token`).
- Nếu lần thử lại vẫn trả về phản hồi token không hợp lệ, lời gọi gửi sẽ thất bại với trạng thái/nội dung upstream để dễ xử lý sự cố hơn.

### 4.12 DingTalk

```toml
[channels_config.dingtalk]
client_id = "ding-app-key"
client_secret = "ding-app-secret"
allowed_users = ["*"]
```

### 4.13 QQ

```toml
[channels_config.qq]
app_id = "qq-app-id"
app_secret = "qq-app-secret"
allowed_users = ["*"]
receive_mode = "webhook" # webhook (mặc định) hoặc websocket (legacy fallback)
```

Ghi chú:

- `webhook` hiện là chế độ mặc định, nhận callback tại `POST /qq`.
- Gói xác thực QQ (`op = 13`) được ký tự động bằng `app_secret`.
- Nếu có header `X-Bot-Appid`, giá trị phải khớp `app_id`.
- Đặt `receive_mode = "websocket"` nếu cần giữ đường nhận sự kiện WS cũ.

### 4.14 iMessage

```toml
[channels_config.imessage]
allowed_contacts = ["*"]
```

---

## 5. Quy trình xác thực

1. Cấu hình một channel với allowlist rộng (`"*"`) để xác minh ban đầu.
2. Chạy:

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

1. Gửi tin nhắn từ người gửi dự kiến.
2. Xác nhận nhận được phản hồi.
3. Siết chặt allowlist từ `"*"` thành các ID cụ thể.

---

## 6. Danh sách kiểm tra xử lý sự cố

Nếu channel có vẻ đã kết nối nhưng không phản hồi:

1. Xác nhận danh tính người gửi được cho phép bởi trường allowlist đúng.
2. Xác nhận tài khoản bot đã là thành viên/có quyền trong room/channel đích.
3. Xác nhận token/secret hợp lệ (và chưa hết hạn/bị thu hồi).
4. Xác nhận giả định về chế độ truyền tải:
   - Các channel polling/websocket không cần HTTP inbound công khai
   - Các channel webhook cần HTTPS callback có thể truy cập được
5. Khởi động lại `zeroclaw daemon` sau khi thay đổi config.

Đặc biệt với các phòng Matrix mã hóa, dùng:
- [Hướng dẫn Matrix E2EE](matrix-e2ee-guide.md)

---

## 7. Phụ lục vận hành: bảng từ khóa log

Dùng phụ lục này để phân loại sự cố nhanh. Khớp từ khóa log trước, sau đó thực hiện các bước xử lý sự cố ở trên.

### 7.1 Lệnh capture được khuyến nghị

```bash
RUST_LOG=info zeroclaw daemon 2>&1 | tee /tmp/zeroclaw.log
```

Sau đó lọc các sự kiện channel/gateway:

```bash
rg -n "Matrix|Telegram|Discord|Slack|Mattermost|Signal|WhatsApp|Email|IRC|Lark|DingTalk|QQ|iMessage|Webhook|Channel" /tmp/zeroclaw.log
```

### 7.2 Bảng từ khóa

| Thành phần | Tín hiệu khởi động / hoạt động bình thường | Tín hiệu ủy quyền / chính sách | Tín hiệu truyền tải / lỗi |
|---|---|---|---|
| Telegram | `Telegram channel listening for messages...` | `Telegram: ignoring message from unauthorized user:` | `Telegram poll error:` / `Telegram parse error:` / `Telegram polling conflict (409):` |
| Discord | `Discord: connected and identified` | `Discord: ignoring message from unauthorized user:` | `Discord: received Reconnect (op 7)` / `Discord: received Invalid Session (op 9)` |
| Slack | `Slack channel listening on #` | `Slack: ignoring message from unauthorized user:` | `Slack poll error:` / `Slack parse error:` |
| Mattermost | `Mattermost channel listening on` | `Mattermost: ignoring message from unauthorized user:` | `Mattermost poll error:` / `Mattermost parse error:` |
| Matrix | `Matrix channel listening on room` / `Matrix room ... is encrypted; E2EE decryption is enabled via matrix-sdk.` | `Matrix whoami failed; falling back to configured session hints for E2EE session restore:` / `Matrix whoami failed while resolving listener user_id; using configured user_id hint:` | `Matrix sync error: ... retrying...` |
| Signal | `Signal channel listening via SSE on` | (kiểm tra allowlist được thực thi bởi `allowed_from`) | `Signal SSE returned ...` / `Signal SSE connect error:` |
| WhatsApp (channel) | `WhatsApp channel active (webhook mode).` / `WhatsApp Web connected successfully` | `WhatsApp: ignoring message from unauthorized number:` / `WhatsApp Web: message from ... not in allowed list` | `WhatsApp send failed:` / `WhatsApp Web stream error:` |
| Webhook / WhatsApp (gateway) | `WhatsApp webhook verified successfully` | `Webhook: rejected — not paired / invalid bearer token` / `Webhook: rejected request — invalid or missing X-Webhook-Secret` / `WhatsApp webhook verification failed — token mismatch` | `Webhook JSON parse error:` |
| Email | `Email polling every ...` / `Email sent to ...` | `Blocked email from ...` | `Email poll failed:` / `Email poll task panicked:` |
| IRC | `IRC channel connecting to ...` / `IRC registered as ...` | (kiểm tra allowlist được thực thi bởi `allowed_users`) | `IRC SASL authentication failed (...)` / `IRC server does not support SASL...` / `IRC nickname ... is in use, trying ...` |
| Lark / Feishu | `Lark: WS connected` / `Lark event callback server listening on` | `Lark WS: ignoring ... (not in allowed_users)` / `Lark: ignoring message from unauthorized user:` | `Lark: ping failed, reconnecting` / `Lark: heartbeat timeout, reconnecting` / `Lark: WS read error:` |
| DingTalk | `DingTalk: connected and listening for messages...` | `DingTalk: ignoring message from unauthorized user:` | `DingTalk WebSocket error:` / `DingTalk: message channel closed` |
| QQ | `QQ: connected and identified` | `QQ: ignoring C2C message from unauthorized user:` / `QQ: ignoring group message from unauthorized user:` | `QQ: received Reconnect (op 7)` / `QQ: received Invalid Session (op 9)` / `QQ: message channel closed` |
| iMessage | `iMessage channel listening (AppleScript bridge)...` | (allowlist liên hệ được thực thi bởi `allowed_contacts`) | `iMessage poll error:` |

### 7.3 Từ khóa của runtime supervisor

Nếu một channel task cụ thể bị crash hoặc thoát, channel supervisor trong `channels/mod.rs` phát ra:

- `Channel <name> exited unexpectedly; restarting`
- `Channel <name> error: ...; restarting`
- `Channel message worker crashed:`

Các thông báo này xác nhận cơ chế tự restart đang hoạt động. Kiểm tra log trước đó để tìm nguyên nhân gốc rễ.
