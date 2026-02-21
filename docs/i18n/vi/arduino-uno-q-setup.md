# ZeroClaw trên Arduino Uno Q — Hướng dẫn từng bước

Chạy ZeroClaw trên phía Linux của Arduino Uno Q. Telegram hoạt động qua WiFi; điều khiển GPIO dùng Bridge (yêu cầu một ứng dụng App Lab tối giản).

---

## Những gì đã có sẵn (Không cần thay đổi code)

ZeroClaw bao gồm mọi thứ cần thiết cho Arduino Uno Q. **Clone repo và làm theo hướng dẫn này — không cần patch hay code tùy chỉnh nào.**

| Thành phần | Vị trí | Mục đích |
|------------|--------|---------|
| Bridge app | `firmware/zeroclaw-uno-q-bridge/` | MCU sketch + Python socket server (port 9999) cho GPIO |
| Bridge tools | `src/peripherals/uno_q_bridge.rs` | Tool `gpio_read` / `gpio_write` giao tiếp với Bridge qua TCP |
| Setup command | `src/peripherals/uno_q_setup.rs` | `zeroclaw peripheral setup-uno-q` triển khai Bridge qua scp + arduino-app-cli |
| Config schema | `board = "arduino-uno-q"`, `transport = "bridge"` | Được hỗ trợ trong `config.toml` |

Build với `--features hardware` (hoặc features mặc định) để bao gồm hỗ trợ Uno Q.

---

## Yêu cầu trước khi bắt đầu

- Arduino Uno Q đã cấu hình WiFi
- Arduino App Lab đã cài trên Mac (để thiết lập và triển khai lần đầu)
- API key cho LLM (OpenRouter, v.v.)

---

## Phase 1: Thiết lập Uno Q lần đầu (Một lần duy nhất)

### 1.1 Cấu hình Uno Q qua App Lab

1. Tải [Arduino App Lab](https://docs.arduino.cc/software/app-lab/) (AppImage trên Linux).
2. Kết nối Uno Q qua USB, bật nguồn.
3. Mở App Lab, kết nối với board.
4. Làm theo hướng dẫn cài đặt:
   - Đặt username và password (cho SSH)
   - Cấu hình WiFi (SSID, password)
   - Áp dụng các bản cập nhật firmware nếu có
5. Ghi lại địa chỉ IP hiển thị (ví dụ: `arduino@192.168.1.42`) hoặc tìm sau qua `ip addr show` trong terminal của App Lab.

### 1.2 Xác nhận truy cập SSH

```bash
ssh arduino@<UNO_Q_IP>
# Nhập password đã đặt
```

---

## Phase 2: Cài đặt ZeroClaw trên Uno Q

### Phương án A: Build trực tiếp trên thiết bị (Đơn giản hơn, ~20–40 phút)

```bash
# SSH vào Uno Q
ssh arduino@<UNO_Q_IP>

# Cài Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# Cài các gói phụ thuộc build (Debian)
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev

# Clone zeroclaw (hoặc scp project của bạn)
git clone https://github.com/theonlyhennygod/zeroclaw.git
cd zeroclaw

# Build (~15–30 phút trên Uno Q)
cargo build --release

# Cài đặt
sudo cp target/release/zeroclaw /usr/local/bin/
```

### Phương án B: Cross-Compile trên Mac (Nhanh hơn)

```bash
# Trên Mac — thêm target aarch64
rustup target add aarch64-unknown-linux-gnu

# Cài cross-compiler (macOS; cần cho linking)
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu

# Build
CC_aarch64_unknown_linux_gnu=aarch64-unknown-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu

# Copy sang Uno Q
scp target/aarch64-unknown-linux-gnu/release/zeroclaw arduino@<UNO_Q_IP>:~/
ssh arduino@<UNO_Q_IP> "sudo mv ~/zeroclaw /usr/local/bin/"
```

Nếu cross-compile thất bại, dùng Phương án A và build trực tiếp trên thiết bị.

---

## Phase 3: Cấu hình ZeroClaw

### 3.1 Chạy Onboard (hoặc tạo Config thủ công)

```bash
ssh arduino@<UNO_Q_IP>

# Cấu hình nhanh
zeroclaw onboard --api-key YOUR_OPENROUTER_KEY --provider openrouter

# Hoặc tạo config thủ công
mkdir -p ~/.zeroclaw/workspace
nano ~/.zeroclaw/config.toml
```

### 3.2 config.toml tối giản

```toml
api_key = "YOUR_OPENROUTER_API_KEY"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"

[peripherals]
enabled = false
# GPIO qua Bridge yêu cầu Phase 4

[channels_config.telegram]
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
allowed_users = ["*"]

[gateway]
host = "127.0.0.1"
port = 3000
allow_public_bind = false

[agent]
compact_context = true
```

---

## Phase 4: Chạy ZeroClaw Daemon

```bash
ssh arduino@<UNO_Q_IP>

# Chạy daemon (Telegram polling hoạt động qua WiFi)
zeroclaw daemon --host 127.0.0.1 --port 3000
```

**Tại bước này:** Telegram chat hoạt động. Gửi tin nhắn tới bot — ZeroClaw phản hồi. Chưa có GPIO.

---

## Phase 5: GPIO qua Bridge (ZeroClaw xử lý tự động)

ZeroClaw bao gồm Bridge app và setup command.

### 5.1 Triển khai Bridge App

**Từ Mac** (với repo zeroclaw):
```bash
zeroclaw peripheral setup-uno-q --host 192.168.0.48
```

**Từ Uno Q** (đã SSH vào):
```bash
zeroclaw peripheral setup-uno-q
```

Lệnh này copy Bridge app vào `~/ArduinoApps/zeroclaw-uno-q-bridge` và khởi động nó.

### 5.2 Thêm vào config.toml

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "arduino-uno-q"
transport = "bridge"
```

### 5.3 Chạy ZeroClaw

```bash
zeroclaw daemon --host 127.0.0.1 --port 3000
```

Giờ khi bạn nhắn tin cho Telegram bot *"Turn on the LED"* hoặc *"Set pin 13 high"*, ZeroClaw dùng `gpio_write` qua Bridge.

---

## Tóm tắt: Các lệnh từ đầu đến cuối

| Bước | Lệnh |
|------|------|
| 1 | Cấu hình Uno Q trong App Lab (WiFi, SSH) |
| 2 | `ssh arduino@<IP>` |
| 3 | `curl -sSf https://sh.rustup.rs \| sh -s -- -y && source ~/.cargo/env` |
| 4 | `sudo apt-get install -y pkg-config libssl-dev` |
| 5 | `git clone https://github.com/theonlyhennygod/zeroclaw.git && cd zeroclaw` |
| 6 | `cargo build --release --no-default-features` |
| 7 | `zeroclaw onboard --api-key KEY --provider openrouter` |
| 8 | Chỉnh sửa `~/.zeroclaw/config.toml` (thêm Telegram bot_token) |
| 9 | `zeroclaw daemon --host 127.0.0.1 --port 3000` |
| 10 | Nhắn tin cho Telegram bot — nó phản hồi |

---

## Xử lý sự cố

- **"command not found: zeroclaw"** — Dùng đường dẫn đầy đủ: `/usr/local/bin/zeroclaw` hoặc đảm bảo `~/.cargo/bin` nằm trong PATH.
- **Telegram không phản hồi** — Kiểm tra bot_token, allowed_users, và Uno Q có kết nối internet (WiFi).
- **Hết bộ nhớ** — Dùng `--no-default-features` để giảm kích thước binary; cân nhắc `compact_context = true`.
- **Lệnh GPIO bị bỏ qua** — Đảm bảo Bridge app đang chạy (`zeroclaw peripheral setup-uno-q` triển khai và khởi động nó). Config phải có `board = "arduino-uno-q"` và `transport = "bridge"`.
- **LLM provider (GLM/Zhipu)** — Dùng `default_provider = "glm"` hoặc `"zhipu"` với `GLM_API_KEY` trong env hoặc config. ZeroClaw dùng endpoint v4 chính xác.
