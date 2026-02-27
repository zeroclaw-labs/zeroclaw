# Chính sách nguồn Actions (Giai đoạn 1)

Tài liệu này định nghĩa chính sách kiểm soát nguồn GitHub Actions hiện tại cho repository này.

Mục tiêu Giai đoạn 1: khóa nguồn action với ít gián đoạn nhất, trước khi pin SHA đầy đủ.

## Chính sách hiện tại

- Quyền Actions repository: được bật
- Chế độ action cho phép: đã chọn
- Yêu cầu pin SHA: false (hoãn đến Giai đoạn 2)

Các mẫu allowlist được chọn:

- `actions/*` (bao gồm `actions/cache`, `actions/checkout`, `actions/upload-artifact`, `actions/download-artifact` và các first-party action khác)
- `docker/*`
- `dtolnay/rust-toolchain@*`
- `DavidAnson/markdownlint-cli2-action@*`
- `lycheeverse/lychee-action@*`
- `EmbarkStudios/cargo-deny-action@*`
- `rustsec/audit-check@*`
- `rhysd/actionlint@*`
- `softprops/action-gh-release@*`
- `sigstore/cosign-installer@*`
- `useblacksmith/*` (cơ sở hạ tầng self-hosted runner Blacksmith)

## Xuất kiểm soát thay đổi

Dùng các lệnh sau để xuất chính sách hiệu lực hiện tại phục vụ kiểm toán/kiểm soát thay đổi:

```bash
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

Ghi lại mỗi thay đổi chính sách với:

- ngày/giờ thay đổi (UTC)
- tác nhân
- lý do
- delta allowlist (mẫu được thêm/xóa)
- ghi chú rollback

## Lý do giai đoạn này

- Giảm rủi ro chuỗi cung ứng từ các marketplace action chưa được review.
- Bảo tồn chức năng CI/CD hiện tại với chi phí migration thấp.
- Chuẩn bị cho Giai đoạn 2 pin SHA đầy đủ mà không chặn phát triển đang diễn ra.

## Bảo vệ workflow agentic

Vì repository này có khối lượng thay đổi do agent tạo ra cao:

- Mọi PR thêm hoặc thay đổi nguồn action `uses:` phải bao gồm ghi chú tác động allowlist.
- Các action bên thứ ba mới yêu cầu review maintainer tường minh trước khi đưa vào allowlist.
- Chỉ mở rộng allowlist cho các action bị thiếu đã được xác minh; tránh các ngoại lệ wildcard rộng.
- Giữ hướng dẫn rollback trong mô tả PR cho các thay đổi chính sách Actions.

## Checklist xác thực

Sau khi thay đổi allowlist, xác thực:

1. `CI`
2. `Docker`
3. `Security Audit`
4. `Workflow Sanity`
5. `Release` (khi an toàn để chạy)

Failure mode cần chú ý:

- `action is not allowed by policy`

Nếu gặp phải, chỉ thêm action tin cậy còn thiếu cụ thể đó, chạy lại và ghi lại lý do.

Ghi chú quét gần đây nhất:

- 2026-02-17: Cache phụ thuộc Rust được migrate từ `Swatinem/rust-cache` sang `useblacksmith/rust-cache`
    - Không cần mẫu allowlist mới (`useblacksmith/*` đã có trong allowlist)
- 2026-02-16: Phụ thuộc ẩn được phát hiện trong `release.yml`: `sigstore/cosign-installer@...`
    - Đã thêm mẫu allowlist: `sigstore/cosign-installer@*`
- 2026-02-16: Migration Blacksmith chặn thực thi workflow
    - Đã thêm mẫu allowlist: `useblacksmith/*` cho cơ sở hạ tầng self-hosted runner
    - Actions: `useblacksmith/setup-docker-builder@v1`, `useblacksmith/build-push-action@v2`
- 2026-02-17: Cập nhật cân bằng tính tái tạo/độ tươi của security audit
    - Đã thêm mẫu allowlist: `rustsec/audit-check@*`
    - Thay thế thực thi nội tuyến `cargo install cargo-audit` bằng `rustsec/audit-check@69366f33c96575abad1ee0dba8212993eecbe998` được pin trong `security.yml`
    - Supersedes đề xuất phiên bản nổi trong #588 trong khi giữ chính sách nguồn action rõ ràng

## Rollback

Đường dẫn bỏ chặn khẩn cấp:

1. Tạm thời đặt chính sách Actions trở về `all`.
2. Khôi phục allowlist đã chọn sau khi xác định các mục còn thiếu.
3. Ghi lại sự cố và delta allowlist cuối cùng.
