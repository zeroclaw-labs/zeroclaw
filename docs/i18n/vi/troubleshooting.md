# Khắc phục sự cố ZeroClaw

Các lỗi thường gặp khi cài đặt và chạy, kèm cách khắc phục.

Xác minh lần cuối: **2026-02-20**.

## Cài đặt / Bootstrap

### Không tìm thấy `cargo`

Triệu chứng:

- bootstrap thoát với lỗi `cargo is not installed`

Khắc phục:

```bash
./bootstrap.sh --install-rust
```

Hoặc cài từ <https://rustup.rs/>.

### Thiếu thư viện hệ thống để build

Triệu chứng:

- build thất bại do lỗi trình biên dịch hoặc `pkg-config`

Khắc phục:

```bash
./bootstrap.sh --install-system-deps
```

### Build thất bại trên máy ít RAM / ít dung lượng

Triệu chứng:

- `cargo build --release` bị kill (`signal: 9`, OOM killer, hoặc `cannot allocate memory`)
- Build vẫn lỗi sau khi thêm swap vì hết dung lượng ổ đĩa

Nguyên nhân:

- RAM lúc chạy (<5MB) khác xa RAM lúc biên dịch.
- Build đầy đủ từ mã nguồn có thể cần **2 GB RAM + swap** và **6+ GB dung lượng trống**.
- Bật swap trên ổ nhỏ có thể tránh OOM RAM nhưng vẫn lỗi vì hết dung lượng.

Cách tốt nhất cho máy hạn chế tài nguyên:

```bash
./bootstrap.sh --prefer-prebuilt
```

Chế độ chỉ dùng binary (không build từ nguồn):

```bash
./bootstrap.sh --prebuilt-only
```

Nếu bắt buộc phải build từ nguồn trên máy yếu:

1. Chỉ thêm swap nếu còn đủ dung lượng cho cả swap lẫn kết quả build.
1. Giới hạn số luồng build:

```bash
CARGO_BUILD_JOBS=1 cargo build --release --locked
```

1. Bỏ bớt feature nặng khi không cần Matrix:

```bash
cargo build --release --locked --no-default-features --features hardware
```

1. Cross-compile trên máy mạnh hơn rồi copy binary sang máy đích.

### Build rất chậm hoặc có vẻ bị treo

Triệu chứng:

- `cargo check` / `cargo build` dừng lâu ở `Checking zeroclaw`
- Lặp lại thông báo `Blocking waiting for file lock on package cache` hoặc `build directory`

Nguyên nhân:

- Thư viện Matrix E2EE (`matrix-sdk`, `ruma`, `vodozemac`) lớn và tốn thời gian kiểm tra kiểu.
- TLS + crypto native build script (`aws-lc-sys`, `ring`) tăng thời gian biên dịch đáng kể.
- `rusqlite` với SQLite tích hợp biên dịch mã C cục bộ.
- Chạy nhiều cargo job/worktree song song gây tranh chấp file lock.

Kiểm tra nhanh:

```bash
cargo check --timings
cargo tree -d
```

Báo cáo thời gian được ghi tại `target/cargo-timings/cargo-timing.html`.

Lặp nhanh hơn khi không cần kênh Matrix:

```bash
cargo check --no-default-features --features hardware
```

Lệnh này bỏ qua `channel-matrix` và giảm đáng kể thời gian biên dịch.

Build với Matrix:

```bash
cargo check --no-default-features --features hardware,channel-matrix
```

Giảm tranh chấp lock:

```bash
pgrep -af "cargo (check|build|test)|cargo check|cargo build|cargo test"
```

Dừng các cargo job không liên quan trước khi build.

### Không tìm thấy lệnh `zeroclaw` sau cài đặt

Triệu chứng:

- Cài đặt thành công nhưng shell không tìm thấy `zeroclaw`

Khắc phục:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
which zeroclaw
```

Thêm vào shell profile nếu cần giữ lâu dài.

## Runtime / Gateway

### Không kết nối được gateway

Kiểm tra:

```bash
zeroclaw status
zeroclaw doctor
```

Xác minh `~/.zeroclaw/config.toml`:

- `[gateway].host` (mặc định `127.0.0.1`)
- `[gateway].port` (mặc định `3000`)
- `allow_public_bind` chỉ bật khi cố ý mở truy cập LAN/public

### Lỗi ghép nối / xác thực webhook

Kiểm tra:

1. Đảm bảo đã hoàn tất ghép nối (luồng `/pair`)
2. Đảm bảo bearer token còn hiệu lực
3. Chạy lại chẩn đoán:

```bash
zeroclaw doctor
```

## Sự cố kênh

### Telegram xung đột: `terminated by other getUpdates request`

Nguyên nhân:

- Nhiều poller dùng chung bot token

Khắc phục:

- Chỉ giữ một runtime đang chạy cho token đó
- Dừng các tiến trình `zeroclaw daemon` / `zeroclaw channel start` thừa

### Kênh không khỏe trong `channel doctor`

Kiểm tra:

```bash
zeroclaw channel doctor
```

Sau đó xác minh thông tin xác thực và trường allowlist cho từng kênh trong config.

## Chế độ dịch vụ

### Dịch vụ đã cài nhưng không chạy

Kiểm tra:

```bash
zeroclaw service status
```

Khôi phục:

```bash
zeroclaw service stop
zeroclaw service start
```

Xem log trên Linux:

```bash
journalctl --user -u zeroclaw.service -f
```

## Tương thích cài đặt cũ

Cả hai cách vẫn hoạt động:

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/install.sh | bash
```

`install.sh` là điểm vào tương thích, chuyển tiếp/dự phòng về hành vi bootstrap.

## Vẫn chưa giải quyết được?

Thu thập và đính kèm các thông tin sau khi tạo issue:

```bash
zeroclaw --version
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

Kèm thêm: hệ điều hành, cách cài đặt, và đoạn config đã ẩn bí mật.

## Tài liệu liên quan

- [operations-runbook.md](operations-runbook.md)
- [one-click-bootstrap.md](one-click-bootstrap.md)
- [channels-reference.md](channels-reference.md)
- [network-deployment.md](network-deployment.md)
