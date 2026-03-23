# Quy trình Release ZeroClaw

Runbook này định nghĩa quy trình release tiêu chuẩn của maintainer.

Cập nhật lần cuối: **2026-02-20**.

## Mục tiêu release

- Đảm bảo release có thể dự đoán và lặp lại.
- Chỉ publish từ code đã có trên `master`.
- Xác minh các artifact đa nền tảng trước khi publish.
- Duy trì nhịp release đều đặn ngay cả khi PR volume cao.

## Chu kỳ tiêu chuẩn

- Release patch/minor: hàng tuần hoặc hai tuần một lần.
- Bản vá bảo mật khẩn cấp: out-of-band.
- Không bao giờ chờ tích lũy quá nhiều commit lớn.

## Hợp đồng workflow

Automation release nằm tại:

- `.github/workflows/pub-release.yml`
- `.github/workflows/pub-homebrew-core.yml` (PR formula Homebrew thủ công, do bot sở hữu)

Các chế độ:

- Tag push `v*`: chế độ publish.
- Manual dispatch: chế độ chỉ xác minh hoặc publish.
- Lịch hàng tuần: chế độ chỉ xác minh.

Các guardrail ở chế độ publish:

- Tag phải khớp định dạng semver-like `vX.Y.Z[-suffix]`.
- Tag phải đã tồn tại trên origin.
- Commit của tag phải có thể truy vết được từ `origin/master`.
- GHCR image tag tương ứng (`ghcr.io/<owner>/<repo>:<tag>`) phải sẵn sàng trước khi GitHub Release publish hoàn tất.
- Artifact được xác minh trước khi publish.

## Quy trình maintainer

### 1) Preflight trên `master`

1. Đảm bảo các required check đều xanh trên `master` mới nhất.
2. Xác nhận không có sự cố ưu tiên cao hoặc regression đã biết nào đang mở.
3. Xác nhận các workflow installer và Docker đều khoẻ mạnh trên các commit `master` gần đây.

### 2) Chạy verification build (không publish)

Chạy `Pub Release` thủ công:

- `publish_release`: `false`
- `release_ref`: `master`

Kết quả mong đợi:

- Ma trận target đầy đủ build thành công.
- `verify-artifacts` xác nhận tất cả archive mong đợi đều tồn tại.
- Không có GitHub Release nào được publish.

### 3) Cut release tag

Từ một checkout cục bộ sạch đã sync với `origin/master`:

```bash
scripts/release/cut_release_tag.sh vX.Y.Z --push
```

Script này đảm bảo:

- working tree sạch
- `HEAD == origin/master`
- tag không bị trùng lặp
- định dạng tag semver-like

### 4) Theo dõi publish run

Sau khi push tag, theo dõi:

1. Chế độ publish `Pub Release`
2. Job publish `Pub Docker Img`

Kết quả publish mong đợi:

- release archive
- `SHA256SUMS`
- SBOM `CycloneDX` và `SPDX`
- chữ ký/chứng chỉ cosign
- GitHub Release notes + asset

### 5) Xác minh sau release

1. Xác minh GitHub Release asset có thể tải xuống.
2. Xác minh GHCR tag cho phiên bản đã release (`vX.Y.Z`) và tag SHA commit release (`sha-<12>`).
3. Xác minh các đường dẫn cài đặt phụ thuộc vào release asset (ví dụ tải xuống binary bootstrap).

### 6) Publish formula Homebrew Core (do bot sở hữu)

Chạy `Pub Homebrew Core` thủ công:

- `release_tag`: `vX.Y.Z`
- `dry_run`: `true` trước, sau đó `false`

Cài đặt repository bắt buộc cho non-dry-run:

- secret: `HOMEBREW_CORE_BOT_TOKEN` (token từ tài khoản bot chuyên dụng, không phải tài khoản maintainer cá nhân)
- variable: `HOMEBREW_CORE_BOT_FORK_REPO` (ví dụ `zeroclaw-release-bot/homebrew-core`)
- variable tùy chọn: `HOMEBREW_CORE_BOT_EMAIL`

Các guardrail workflow:

- release tag phải khớp version `Cargo.toml`
- URL nguồn và SHA256 của formula được cập nhật từ tagged tarball
- license formula được chuẩn hóa thành `Apache-2.0 OR MIT`
- PR được mở từ bot fork vào `Homebrew/homebrew-core:master`

## Đường dẫn khẩn cấp / khôi phục

Nếu release push tag thất bại sau khi artifact đã được xác minh:

1. Sửa vấn đề workflow hoặc packaging trên `master`.
2. Chạy lại `Pub Release` thủ công ở chế độ publish với:
   - `publish_release=true`
   - `release_tag=<existing tag>`
   - `release_ref` tự động được pin vào `release_tag` ở chế độ publish
3. Xác minh lại asset đã release.

## Ghi chú vận hành

- Giữ các thay đổi release nhỏ và có thể đảo ngược.
- Dùng một issue/checklist release cho mỗi phiên bản để bàn giao rõ ràng.
- Tránh publish từ các feature branch ad-hoc.
