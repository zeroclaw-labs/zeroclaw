# Triển khai mạng — ZeroClaw trên Raspberry Pi và mạng nội bộ

Tài liệu này hướng dẫn triển khai ZeroClaw trên Raspberry Pi hoặc host khác trong mạng nội bộ, với các channel Telegram và webhook tùy chọn.

---

## 1. Tổng quan

| Chế độ | Cần cổng đến? | Trường hợp dùng |
|------|----------------------|----------|
| **Telegram polling** | Không | ZeroClaw poll Telegram API; hoạt động từ bất kỳ đâu |
| **Matrix sync (kể cả E2EE)** | Không | ZeroClaw sync qua Matrix client API; không cần webhook đến |
| **Discord/Slack** | Không | Tương tự — chỉ outbound |
| **Gateway webhook** | Có | POST /webhook, WhatsApp, v.v. cần public URL |
| **Gateway pairing** | Có | Nếu bạn pair client qua gateway |

**Lưu ý:** Telegram, Discord và Slack dùng **long-polling** — ZeroClaw thực hiện các request ra ngoài. Không cần port forwarding hoặc public IP.

---

## 2. ZeroClaw trên Raspberry Pi

### 2.1 Điều kiện tiên quyết

- Raspberry Pi (3/4/5) với Raspberry Pi OS
- Thiết bị ngoại vi USB (Arduino, Nucleo) nếu dùng serial transport
- Tùy chọn: `rppal` cho native GPIO (`peripheral-rpi` feature)

### 2.2 Cài đặt

```bash
# Build for RPi (or cross-compile from host)
cargo build --release --features hardware

# Or install via your preferred method
```

### 2.3 Cấu hình

Chỉnh sửa `~/.zeroclaw/config.toml`:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"

# Or Arduino over USB
[[peripherals.boards]]
board = "arduino-uno"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = []

[gateway]
host = "127.0.0.1"
port = 3000
allow_public_bind = false
```

### 2.4 Chạy Daemon (chỉ cục bộ)

```bash
zeroclaw daemon --host 127.0.0.1 --port 3000
```

- Gateway bind vào `127.0.0.1` — không tiếp cận được từ máy khác
- Channel Telegram hoạt động: ZeroClaw poll Telegram API (outbound)
- Không cần tường lửa hay port forwarding

---

## 3. Bind vào 0.0.0.0 (mạng nội bộ)

Để cho phép các thiết bị khác trong LAN của bạn truy cập gateway (ví dụ: để pairing hoặc webhook):

### 3.1 Tùy chọn A: Opt-in rõ ràng

```toml
[gateway]
host = "0.0.0.0"
port = 3000
allow_public_bind = true
```

```bash
zeroclaw daemon --host 0.0.0.0 --port 3000
```

**Bảo mật:** `allow_public_bind = true` phơi bày gateway với mạng nội bộ của bạn. Chỉ dùng trên mạng LAN tin cậy.

### 3.2 Tùy chọn B: Tunnel (khuyến nghị cho Webhook)

Nếu bạn cần **public URL** (ví dụ: webhook WhatsApp, client bên ngoài):

1. Chạy gateway trên localhost:
   ```bash
   zeroclaw daemon --host 127.0.0.1 --port 3000
   ```

2. Khởi động tunnel:
   ```toml
   [tunnel]
   provider = "tailscale"   # or "ngrok", "cloudflare"
   ```
   Hoặc dùng `zeroclaw tunnel` (xem tài liệu tunnel).

3. ZeroClaw sẽ từ chối `0.0.0.0` trừ khi `allow_public_bind = true` hoặc có tunnel đang hoạt động.

---

## 4. Telegram Polling (Không cần cổng đến)

Telegram dùng **long-polling** theo mặc định:

- ZeroClaw gọi `https://api.telegram.org/bot{token}/getUpdates`
- Không cần cổng đến hoặc public IP
- Hoạt động sau NAT, trên RPi, trong home lab

**Cấu hình:**

```toml
[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = []            # deny-by-default, bind identities explicitly
```

Chạy `zeroclaw daemon` — channel Telegram khởi động tự động.

Để cho phép một tài khoản Telegram lúc runtime:

```bash
zeroclaw channel bind-telegram <IDENTITY>
```

`<IDENTITY>` có thể là Telegram user ID dạng số hoặc username (không có `@`).

### 4.1 Quy tắc Single Poller (Quan trọng)

Telegram Bot API `getUpdates` chỉ hỗ trợ một poller hoạt động cho mỗi bot token.

- Chỉ chạy một instance runtime cho cùng token (khuyến nghị: service `zeroclaw daemon`).
- Không chạy `cargo run -- channel start` hay tiến trình bot khác cùng lúc.

Nếu gặp lỗi này:

`Conflict: terminated by other getUpdates request`

bạn đang có xung đột polling. Dừng các instance thừa và chỉ khởi động lại một daemon duy nhất.

---

## 5. Webhook Channel (WhatsApp, Tùy chỉnh)

Các channel dựa trên webhook cần **public URL** để Meta (WhatsApp) hoặc client của bạn có thể POST sự kiện.

### 5.1 Tailscale Funnel

```toml
[tunnel]
provider = "tailscale"
```

Tailscale Funnel phơi bày gateway của bạn qua URL `*.ts.net`. Không cần port forwarding.

### 5.2 ngrok

```toml
[tunnel]
provider = "ngrok"
```

Hoặc chạy ngrok thủ công:
```bash
ngrok http 3000
# Use the HTTPS URL for your webhook
```

### 5.3 Cloudflare Tunnel

Cấu hình Cloudflare Tunnel để forward đến `127.0.0.1:3000`, sau đó đặt webhook URL của bạn về hostname công khai của tunnel.

---

## 6. Checklist: Triển khai RPi

- [ ] Build với `--features hardware` (và `peripheral-rpi` nếu dùng native GPIO)
- [ ] Cấu hình `[peripherals]` và `[channels_config.telegram]`
- [ ] Chạy `zeroclaw daemon --host 127.0.0.1 --port 3000` (Telegram hoạt động không cần 0.0.0.0)
- [ ] Để truy cập LAN: `--host 0.0.0.0` + `allow_public_bind = true` trong config
- [ ] Để dùng webhook: dùng Tailscale, ngrok hoặc Cloudflare tunnel

---

## 7. Tham khảo

- [channels-reference.md](./channels-reference.md) — Tổng quan cấu hình channel
- [matrix-e2ee-guide.md](./matrix-e2ee-guide.md) — Thiết lập Matrix và xử lý sự cố phòng mã hóa
- [hardware-peripherals-design.md](hardware-peripherals-design.md) — Thiết kế peripherals
- [adding-boards-and-tools.md](adding-boards-and-tools.md) — Thiết lập phần cứng và thêm board
