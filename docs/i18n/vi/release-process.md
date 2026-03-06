# Quy trình Release ZeroClaw

Runbook này định nghĩa quy trình release tiêu chuẩn của maintainer.

Cập nhật lần cuối: **2026-03-06**.

## Mục tiêu Release

- Giữ release có thể dự đoán và lặp lại.
- Chỉ publish từ code đã có trên `master`.
- Xác minh artifact đa target trước khi publish.
- Giữ cadence release đều đặn ngay cả khi khối lượng PR cao.

## Mô hình Release

ZeroClaw sử dụng mô hình release hai tầng:

- **Beta release** tự động — mỗi merge vào `master` kích hoạt beta pre-release (`vX.Y.Z-beta.<run_number>`).
- **Stable release** thủ công — maintainer chạy `Promote Release` qua workflow dispatch để cắt phiên bản non-pre-release.

## Hợp đồng Workflow

Tự động hóa release nằm trong:

- `.github/workflows/release.yml` — beta release tự động khi push lên `master`
- `.github/workflows/promote-release.yml` — stable release thủ công qua workflow dispatch

### Beta Release (tự động)

- Kích hoạt trên mỗi push lên `master`.
- Tính version từ `Cargo.toml`: `vX.Y.Z-beta.<run_number>`.
- Build ma trận 5 target (linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64).
- Publish GitHub pre-release với archives + SHA256SUMS.
- Build và push Docker image đa nền tảng lên GHCR (`beta` + version tag).

### Stable Release (thủ công)

- Kích hoạt qua `workflow_dispatch` với input `version` (ví dụ `0.2.0`).
- Xác thực version khớp `Cargo.toml` và tag chưa tồn tại.
- Build cùng ma trận 5 target như beta.
- Publish GitHub stable release với archives + SHA256SUMS.
- Build và push Docker image lên GHCR (`latest` + version tag).

## Quy trình Maintainer

### 1) Kiểm tra trước trên `master`

1. Đảm bảo CI xanh trên `master` mới nhất.
2. Xác nhận không có incident ưu tiên cao hoặc regression đã biết đang mở.
3. Xác nhận các beta release gần đây khỏe mạnh (kiểm tra trang GitHub Releases).

### 2) Tùy chọn: chạy CI Full Matrix

Nếu xác minh cross-compilation trước stable release:

- Chạy `CI Full Matrix` thủ công (`.github/workflows/ci-full.yml`).
- Xác nhận build thành công trên `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin` và `x86_64-pc-windows-msvc`.

### 3) Tăng version trong `Cargo.toml`

1. Tạo branch, cập nhật `version` trong `Cargo.toml` lên version mục tiêu.
2. Mở PR, merge vào `master`.
3. Merge kích hoạt beta release tự động.

### 4) Promote lên stable release

1. Chạy workflow `Promote Release` thủ công:
   - `version`: version mục tiêu (ví dụ `0.2.0`) — phải khớp `Cargo.toml`
2. Workflow xác thực version, build tất cả target và publish.

### 5) Xác thực sau release

1. Xác minh asset GitHub Release có thể tải.
2. Xác minh GHCR tag cho version đã release.
3. Xác minh đường dẫn cài đặt dựa trên release asset (ví dụ tải binary bootstrap).

## Cadence Tiêu chuẩn

- Release patch/minor: hàng tuần hoặc hai tuần một lần.
- Sửa bảo mật khẩn cấp: ngoài lịch trình.
- Beta release ship liên tục với mỗi merge.

## Đường dẫn Khẩn cấp / Khôi phục

Nếu stable release thất bại:

1. Sửa vấn đề trên `master`.
2. Chạy lại `Promote Release` với cùng version (nếu tag chưa được tạo).
3. Nếu tag đã tồn tại, tăng lên patch version tiếp theo.

## Ghi chú Vận hành

- Giữ thay đổi release nhỏ và có thể đảo ngược.
- Ưu tiên một issue/checklist release cho mỗi version để handoff rõ ràng.
- Tránh publish từ các branch tính năng tùy ý.
