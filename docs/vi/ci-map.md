# Bản đồ CI Workflow

Tài liệu này giải thích từng GitHub workflow làm gì, khi nào chạy và liệu nó có nên chặn merge hay không.

Để biết hành vi phân phối theo từng sự kiện qua PR, merge, push và release, xem [`.github/workflows/main-branch-flow.md`](../../.github/workflows/main-branch-flow.md).

## Chặn merge và Tùy chọn

Các kiểm tra chặn merge nên giữ nhỏ và mang tính quyết định. Các kiểm tra tùy chọn hữu ích cho tự động hóa và bảo trì, nhưng không nên chặn phát triển bình thường.

### Chặn merge

- `.github/workflows/ci-run.yml` (`CI`)
    - Mục đích: Rust validation (`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D clippy::correctness`, strict delta lint gate trên các dòng Rust thay đổi, `test`, kiểm tra smoke release build) + kiểm tra chất lượng tài liệu khi tài liệu thay đổi (`markdownlint` chỉ chặn các vấn đề trên dòng thay đổi; link check chỉ quét các link mới được thêm trên dòng thay đổi)
    - Hành vi bổ sung: các PR thay đổi `.github/workflows/**` yêu cầu ít nhất một review phê duyệt từ login trong `WORKFLOW_OWNER_LOGINS` (fallback biến repository: `theonlyhennygod,willsarg`)
    - Hành vi bổ sung: lint gate chạy trước `test`/`build`; khi lint/docs gate thất bại trên PR, CI đăng comment phản hồi hành động được với tên gate thất bại và các lệnh sửa cục bộ
    - Merge gate: `CI Required Gate`
- `.github/workflows/workflow-sanity.yml` (`Workflow Sanity`)
    - Mục đích: lint các file GitHub workflow (`actionlint`, kiểm tra tab)
    - Khuyến nghị cho các PR thay đổi workflow
- `.github/workflows/pr-intake-checks.yml` (`PR Intake Checks`)
    - Mục đích: kiểm tra PR an toàn trước CI (độ đầy đủ template, tab/trailing-whitespace/conflict marker trên dòng thêm) với comment sticky phản hồi ngay lập tức

### Quan trọng nhưng không chặn

- `.github/workflows/pub-docker-img.yml` (`Docker`)
    - Mục đích: kiểm tra Docker smoke trên PR và publish image khi push lên `main` (các đường dẫn build-input), push tag (`v*`) và khi dispatch thủ công
- `.github/workflows/sec-audit.yml` (`Security Audit`)
    - Mục đích: advisory phụ thuộc (`rustsec/audit-check`, SHA được pin) và kiểm tra chính sách/giấy phép (`cargo deny`)
- `.github/workflows/sec-codeql.yml` (`CodeQL Analysis`)
    - Mục đích: phân tích tĩnh theo lịch/thủ công để phát hiện vấn đề bảo mật
- `.github/workflows/pub-release.yml` (`Release`)
    - Mục đích: build release artifact ở chế độ xác minh (thủ công/theo lịch) và publish GitHub release khi push tag hoặc chế độ publish thủ công
- `.github/workflows/pr-label-policy-check.yml` (`Label Policy Sanity`)
    - Mục đích: xác thực chính sách bậc contributor dùng chung trong `.github/label-policy.json` và đảm bảo các label workflow sử dụng chính sách đó
- `.github/workflows/test-rust-build.yml` (`Rust Reusable Job`)
    - Mục đích: Rust setup/cache có thể tái sử dụng + trình chạy lệnh cho các workflow-call consumer

### Tự động hóa repository tùy chọn

- `.github/workflows/pr-labeler.yml` (`PR Labeler`)
    - Mục đích: nhãn phạm vi/đường dẫn + nhãn kích thước/rủi ro + nhãn module chi tiết (`<module>: <component>`)
    - Hành vi bổ sung: mô tả nhãn được quản lý tự động như tooltip khi di chuột để giải thích từng quy tắc phán đoán tự động
    - Hành vi bổ sung: từ khóa liên quan đến provider trong các thay đổi provider/config/onboard/integration được thăng cấp lên nhãn `provider:*` (ví dụ `provider:kimi`, `provider:deepseek`)
    - Hành vi bổ sung: loại bỏ trùng lặp phân cấp chỉ giữ nhãn phạm vi cụ thể nhất (ví dụ `tool:composio` triệt tiêu `tool:core` và `tool`)
    - Hành vi bổ sung: namespace module được nén gọn — một module cụ thể giữ `prefix:component`; nhiều module cụ thể thu gọn thành chỉ `prefix`
    - Hành vi bổ sung: áp dụng bậc contributor trên PR theo số PR đã merge (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50)
    - Hành vi bổ sung: bộ nhãn cuối cùng được sắp xếp theo ưu tiên (`risk:*` đầu tiên, sau đó `size:*`, rồi bậc contributor, cuối là nhãn module/đường dẫn)
    - Hành vi bổ sung: màu nhãn được quản lý theo thứ tự hiển thị để tạo gradient trái-phải mượt mà khi có nhiều nhãn
    - Quản trị thủ công: hỗ trợ `workflow_dispatch` với `mode=audit|repair` để kiểm tra/sửa metadata nhãn được quản lý drift trên toàn repository
    - Hành vi bổ sung: nhãn rủi ro + kích thước được tự sửa khi chỉnh sửa nhãn PR thủ công (sự kiện `labeled`/`unlabeled`); áp dụng `risk: manual` khi maintainer cố ý ghi đè lựa chọn rủi ro tự động
    - Đường dẫn heuristic rủi ro cao: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`
    - Bảo vệ: maintainer có thể áp dụng `risk: manual` để đóng băng tính toán lại rủi ro tự động
- `.github/workflows/pr-auto-response.yml` (`PR Auto Responder`)
    - Mục đích: giới thiệu contributor lần đầu + phân tuyến dựa trên nhãn (`r:support`, `r:needs-repro`, v.v.)
    - Hành vi bổ sung: áp dụng bậc contributor trên issue theo số PR đã merge (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50), khớp chính xác ngưỡng bậc PR
    - Hành vi bổ sung: nhãn bậc contributor được coi là do tự động hóa quản lý (thêm/xóa thủ công trên PR/issue bị tự sửa)
    - Bảo vệ: các luồng đóng dựa trên nhãn chỉ dành cho issue; PR không bao giờ bị tự đóng bởi nhãn route
- `.github/workflows/pr-check-stale.yml` (`Stale`)
    - Mục đích: tự động hóa vòng đời issue/PR stale
- `.github/dependabot.yml` (`Dependabot`)
    - Mục đích: PR cập nhật phụ thuộc được nhóm, giới hạn tốc độ (Cargo + GitHub Actions)
- `.github/workflows/pr-check-status.yml` (`PR Hygiene`)
    - Mục đích: nhắc nhở các PR stale-nhưng-còn-hoạt-động để rebase/re-run các kiểm tra bắt buộc trước khi hàng đợi bị đói

## Bản đồ Trigger

- `CI`: push lên `main`, PR lên `main`
- `Docker`: push lên `main` khi Docker build input thay đổi, push tag (`v*`), PR tương ứng, dispatch thủ công
- `Release`: push tag (`v*`), lịch hàng tuần (chỉ xác minh), dispatch thủ công (xác minh hoặc publish)
- `Security Audit`: push lên `main`, PR lên `main`, lịch hàng tuần
- `Workflow Sanity`: PR/push khi `.github/workflows/**`, `.github/*.yml` hoặc `.github/*.yaml` thay đổi
- `PR Intake Checks`: `pull_request_target` khi opened/reopened/synchronize/edited/ready_for_review
- `Label Policy Sanity`: PR/push khi `.github/label-policy.json`, `.github/workflows/pr-labeler.yml` hoặc `.github/workflows/pr-auto-response.yml` thay đổi
- `PR Labeler`: sự kiện vòng đời `pull_request_target`
- `PR Auto Responder`: issue opened/labeled, `pull_request_target` opened/labeled
- `Stale PR Check`: lịch hàng ngày, dispatch thủ công
- `Dependabot`: cửa sổ bảo trì phụ thuộc hàng tuần
- `PR Hygiene`: lịch mỗi 12 giờ, dispatch thủ công

## Hướng dẫn triage nhanh

1. `CI Required Gate` thất bại: bắt đầu với `.github/workflows/ci-run.yml`.
2. Docker thất bại trên PR: kiểm tra job `pr-smoke` trong `.github/workflows/pub-docker-img.yml`.
3. Release thất bại (tag/thủ công/theo lịch): kiểm tra `.github/workflows/pub-release.yml` và kết quả job `prepare`.
4. Security thất bại: kiểm tra `.github/workflows/sec-audit.yml` và `deny.toml`.
5. Lỗi cú pháp/lint workflow: kiểm tra `.github/workflows/workflow-sanity.yml`.
6. PR intake thất bại: kiểm tra comment sticky `.github/workflows/pr-intake-checks.yml` và run log.
7. Lỗi parity chính sách nhãn: kiểm tra `.github/workflows/pr-label-policy-check.yml`.
8. Lỗi tài liệu trong CI: kiểm tra log job `docs-quality` trong `.github/workflows/ci-run.yml`.
9. Lỗi strict delta lint trong CI: kiểm tra log job `lint-strict-delta` và so sánh với phạm vi diff `BASE_SHA`.

## Quy tắc bảo trì

- Giữ các kiểm tra chặn merge mang tính quyết định và tái tạo được (`--locked` khi áp dụng được).
- Tuân theo `docs/release-process.md` để kiểm tra trước khi publish và kỷ luật tag.
- Giữ chính sách chất lượng Rust chặn merge nhất quán giữa `.github/workflows/ci-run.yml`, `dev/ci.sh` và `.githooks/pre-push` (`./scripts/ci/rust_quality_gate.sh` + `./scripts/ci/rust_strict_delta_gate.sh`).
- Dùng `./scripts/ci/rust_strict_delta_gate.sh` (hoặc `./dev/ci.sh lint-delta`) làm merge gate nghiêm ngặt gia tăng cho các dòng Rust thay đổi.
- Chạy kiểm tra lint nghiêm ngặt đầy đủ thường xuyên qua `./scripts/ci/rust_quality_gate.sh --strict` (ví dụ qua `./dev/ci.sh lint-strict`) và theo dõi việc dọn dẹp trong các PR tập trung.
- Giữ gating markdown tài liệu theo gia tăng qua `./scripts/ci/docs_quality_gate.sh` (chặn vấn đề dòng thay đổi, báo cáo vấn đề baseline riêng).
- Giữ gating link tài liệu theo gia tăng qua `./scripts/ci/collect_changed_links.py` + lychee (chỉ kiểm tra link mới thêm trên dòng thay đổi).
- Ưu tiên quyền workflow tường minh (least privilege).
- Giữ chính sách nguồn Actions hạn chế theo allowlist đã được phê duyệt (xem `docs/actions-source-policy.md`).
- Sử dụng bộ lọc đường dẫn cho các workflow tốn kém khi thực tế.
- Giữ kiểm tra chất lượng tài liệu ít nhiễu (markdown gia tăng + kiểm tra link mới thêm gia tăng).
- Giữ khối lượng cập nhật phụ thuộc được kiểm soát (nhóm + giới hạn PR).
- Tránh kết hợp tự động hóa giới thiệu/cộng đồng với logic gating merge.

## Kiểm soát tác dụng phụ tự động hóa

- Ưu tiên tự động hóa mang tính quyết định có thể ghi đè thủ công (`risk: manual`) khi ngữ cảnh tinh tế.
- Giữ comment auto-response không trùng lặp để tránh nhiễu triage.
- Giữ hành vi tự đóng trong phạm vi issue; maintainer quyết định đóng/merge PR.
- Nếu tự động hóa sai, sửa nhãn trước, rồi tiếp tục review với lý do rõ ràng.
- Dùng nhãn `superseded` / `stale-candidate` để cắt tỉa PR trùng lặp hoặc ngủ đông trước khi review sâu.
