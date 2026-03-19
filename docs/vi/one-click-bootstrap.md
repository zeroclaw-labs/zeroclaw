# Cài đặt một lệnh

Cách cài đặt và khởi tạo JhedaiClaw nhanh nhất.

Xác minh lần cuối: **2026-02-20**.

## Cách 0: Homebrew (macOS/Linuxbrew)

```bash
brew install jhedaiclaw
```

## Cách A (Khuyến nghị): Clone + chạy script cục bộ

```bash
git clone https://github.com/jhedai/jhedaiclaw.git
cd jhedaiclaw
./install.sh
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
./install.sh --prefer-prebuilt
```

Chỉ dùng binary dựng sẵn, báo lỗi nếu không tìm thấy bản phù hợp:

```bash
./install.sh --prebuilt-only
```

Bỏ qua binary dựng sẵn, buộc build từ mã nguồn:

```bash
./install.sh --force-source-build
```

## Bootstrap kép

Mặc định là **chỉ ứng dụng** (build/cài JhedaiClaw), yêu cầu Rust toolchain sẵn có.

Với máy mới, bật bootstrap môi trường:

```bash
./install.sh --install-system-deps --install-rust
```

Lưu ý:

- `--install-system-deps` cài các thành phần biên dịch/build cần thiết (có thể cần `sudo`).
- `--install-rust` cài Rust qua `rustup` nếu chưa có.
- `--prefer-prebuilt` thử tải binary dựng sẵn trước, nếu không có thì build từ nguồn.
- `--prebuilt-only` tắt phương án build từ nguồn.
- `--force-source-build` tắt hoàn toàn phương án binary dựng sẵn.

## Cách B: Lệnh từ xa một dòng

```bash
curl -fsSL https://raw.githubusercontent.com/jhedai/jhedaiclaw/master/install.sh | bash
```

Với môi trường yêu cầu bảo mật cao, nên dùng Cách A để kiểm tra script trước khi chạy.

Nếu chạy Cách B ngoài thư mục repo, bootstrap script sẽ tự clone workspace tạm, build, cài đặt rồi dọn dẹp.

## Chế độ thiết lập tùy chọn

### Thiết lập trong container (Docker)

```bash
./install.sh --docker
```

Lệnh này build image JhedaiClaw cục bộ và chạy thiết lập trong container, lưu config/workspace vào `./.jhedaiclaw-docker`.

### Thiết lập nhanh (không tương tác)

```bash
./install.sh --api-key "sk-..." --provider openrouter
```

Hoặc dùng biến môi trường:

```bash
JHEDAICLAW_API_KEY="sk-..." JHEDAICLAW_PROVIDER="openrouter" ./install.sh
```

## Các cờ hữu ích

- `--install-system-deps`
- `--install-rust`
- `--skip-build`
- `--skip-install`
- `--provider <id>`

Xem tất cả tùy chọn:

```bash
./install.sh --help
```

## Tài liệu liên quan

- [README.md](../../README.vi.md)
- [commands-reference.md](commands-reference.md)
- [providers-reference.md](providers-reference.md)
- [channels-reference.md](channels-reference.md)
