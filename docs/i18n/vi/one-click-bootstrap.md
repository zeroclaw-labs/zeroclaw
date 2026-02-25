# Cài đặt một lệnh

Cách cài đặt và khởi tạo ZeroClaw nhanh nhất.

Xác minh lần cuối: **2026-02-20**.

## Cách 0: Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

## Cách A (Khuyến nghị): Clone + chạy script cục bộ

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

Mặc định script sẽ:

1. `cargo build --release --locked`
2. `cargo install --path . --force --locked`

### Kiểm tra tài nguyên và binary dựng sẵn

Build từ mã nguồn thường yêu cầu tối thiểu:

- **2 GB RAM + swap**
- **6 GB dung lượng trống**

Khi tài nguyên hạn chế, bootstrap sẽ thử tải binary dựng sẵn trước.

```bash
./bootstrap.sh --prefer-prebuilt
```

Chỉ dùng binary dựng sẵn, báo lỗi nếu không tìm thấy bản phù hợp:

```bash
./bootstrap.sh --prebuilt-only
```

Bỏ qua binary dựng sẵn, buộc build từ mã nguồn:

```bash
./bootstrap.sh --force-source-build
```

## Bootstrap kép

Mặc định là **chỉ ứng dụng** (build/cài ZeroClaw), yêu cầu Rust toolchain sẵn có.

Với máy mới, bật bootstrap môi trường:

```bash
./bootstrap.sh --install-system-deps --install-rust
```

Lưu ý:

- `--install-system-deps` cài các thành phần biên dịch/build cần thiết (có thể cần `sudo`).
- `--install-rust` cài Rust qua `rustup` nếu chưa có.
- `--prefer-prebuilt` thử tải binary dựng sẵn trước, nếu không có thì build từ nguồn.
- `--prebuilt-only` tắt phương án build từ nguồn.
- `--force-source-build` tắt hoàn toàn phương án binary dựng sẵn.

## Cách B: Lệnh từ xa một dòng

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash
```

Với môi trường yêu cầu bảo mật cao, nên dùng Cách A để kiểm tra script trước khi chạy.

Tương thích ngược:

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/install.sh | bash
```

Endpoint cũ này ưu tiên chuyển tiếp đến `scripts/bootstrap.sh`, nếu không có thì dùng cài đặt từ nguồn kiểu cũ.

Nếu chạy Cách B ngoài thư mục repo, bootstrap script sẽ tự clone workspace tạm, build, cài đặt rồi dọn dẹp.

## Chế độ thiết lập tùy chọn

### Thiết lập trong container (Docker)

```bash
./bootstrap.sh --docker
```

Lệnh này build image ZeroClaw cục bộ và chạy thiết lập trong container, lưu config/workspace vào `./.zeroclaw-docker`.

### Thiết lập nhanh (không tương tác)

```bash
./bootstrap.sh --onboard --api-key "sk-..." --provider openrouter
```

Hoặc dùng biến môi trường:

```bash
ZEROCLAW_API_KEY="sk-..." ZEROCLAW_PROVIDER="openrouter" ./bootstrap.sh --onboard
```

### Thiết lập tương tác

```bash
./bootstrap.sh --interactive-onboard
```

## Các cờ hữu ích

- `--install-system-deps`
- `--install-rust`
- `--skip-build`
- `--skip-install`
- `--provider <id>`

Xem tất cả tùy chọn:

```bash
./bootstrap.sh --help
```

## Tài liệu liên quan

- [docs/i18n/vi/README.md](README.md)
- [commands-reference.md](commands-reference.md)
- [providers-reference.md](providers-reference.md)
- [channels-reference.md](channels-reference.md)
