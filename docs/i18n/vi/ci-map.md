# Bản đồ CI Workflow

Tài liệu này giải thích từng GitHub workflow làm gì, khi nào chạy và liệu nó có nên chặn merge hay không.

Để biết hành vi phân phối theo từng sự kiện qua PR, merge, push và release, xem [`.github/workflows/master-branch-flow.md`](../../../.github/workflows/master-branch-flow.md).

## Workflows

### CI (`.github/workflows/ci.yml`)

- **Trigger:** pull request lên `master`
- **Mục đích:** chạy test và build release binary trên Linux và macOS
- **Jobs:**
    - `test` — `cargo nextest run --locked` với mold linker
    - `build` — ma trận release build (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`)
- **Merge gate:** cả `test` và `build` phải pass trước khi merge

### Beta Release (`.github/workflows/release.yml`)

- **Trigger:** push lên `master` (mỗi PR được merge)
- **Mục đích:** build, đóng gói và publish beta pre-release với Docker image
- **Jobs:**
    - `version` — tính `vX.Y.Z-beta.<run_number>` từ `Cargo.toml`
    - `build` — ma trận release 5 target (linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64)
    - `publish` — tạo GitHub pre-release với archives + SHA256SUMS
    - `docker` — build và push Docker image đa nền tảng lên GHCR (`beta` + version tag)

### CI Full Matrix (`.github/workflows/ci-full.yml`)

- **Trigger:** chỉ `workflow_dispatch` thủ công
- **Mục đích:** build release binary trên các target bổ sung không có trong PR CI
- **Jobs:**
    - `build` — ma trận 3 target (`aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`)
- **Ghi chú:** hữu ích để xác minh cross-compilation trước stable release

### Promote Release (`.github/workflows/promote-release.yml`)

- **Trigger:** `workflow_dispatch` thủ công với input `version` (ví dụ `0.2.0`)
- **Mục đích:** build, đóng gói và publish stable release (không phải pre-release) với Docker image
- **Jobs:**
    - `validate` — xác nhận version input khớp `Cargo.toml`, xác nhận tag chưa tồn tại
    - `build` — cùng ma trận 5 target như Beta Release
    - `publish` — tạo GitHub stable release với archives + SHA256SUMS
    - `docker` — build và push Docker image đa nền tảng lên GHCR (`latest` + version tag)

## Bản đồ Trigger

| Workflow | Trigger |
|----------|---------|
| CI | Pull request lên `master` |
| Beta Release | Push lên `master` |
| CI Full Matrix | Chỉ dispatch thủ công |
| Promote Release | Chỉ dispatch thủ công |

## Hướng dẫn triage nhanh

1. **CI thất bại trên PR:** kiểm tra `.github/workflows/ci.yml` — xem log job `test` và `build`.
2. **Beta release thất bại sau merge:** kiểm tra `.github/workflows/release.yml` — xem log job `version`, `build`, `publish` và `docker`.
3. **Promote release thất bại:** kiểm tra `.github/workflows/promote-release.yml` — xem job `validate` (version/tag không khớp) và các job `build`/`publish`/`docker`.
4. **Vấn đề build đa nền tảng:** chạy CI Full Matrix thủ công qua `.github/workflows/ci-full.yml` để test các target bổ sung.

## Quy tắc bảo trì

- Giữ các kiểm tra chặn merge mang tính quyết định và tái tạo được (`--locked` khi áp dụng được).
- Tuân theo `docs/release-process.md` để kiểm tra cadence release và kỷ luật version.
- Ưu tiên quyền workflow tường minh (least privilege).
- Giữ chính sách nguồn Actions hạn chế theo allowlist đã được phê duyệt (xem `docs/actions-source-policy.md`).
