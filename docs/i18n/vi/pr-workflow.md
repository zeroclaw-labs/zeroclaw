# Quy trình PR ZeroClaw (Cộng tác khối lượng cao)

Tài liệu này định nghĩa cách ZeroClaw xử lý khối lượng PR lớn trong khi vẫn duy trì:

- Hiệu suất cao
- Hiệu quả cao
- Tính ổn định cao
- Khả năng mở rộng cao
- Tính bền vững cao
- Bảo mật cao

Tài liệu liên quan:

- [`docs/README.md`](README.md) — phân loại và điều hướng tài liệu.
- [`docs/ci-map.md`](ci-map.md) — quyền sở hữu từng workflow, trigger và luồng triage.
- [`docs/reviewer-playbook.md`](reviewer-playbook.md) — hướng dẫn thực thi cho reviewer hàng ngày.

## 0. Tóm tắt

- **Mục đích:** cung cấp mô hình vận hành PR mang tính quyết định và dựa trên rủi ro cho cộng tác thông lượng cao.
- **Đối tượng:** contributor, maintainer và reviewer có hỗ trợ agent.
- **Phạm vi:** cài đặt repository, vòng đời PR, hợp đồng sẵn sàng, phân tuyến rủi ro, kỷ luật hàng đợi và giao thức phục hồi.
- **Ngoài phạm vi:** thay thế cấu hình branch protection hoặc file CI workflow làm nguồn triển khai chính thức.

---

## 1. Lối tắt theo tình huống PR

Dùng phần này để phân tuyến nhanh trước khi review sâu toàn bộ.

### 1.1 Intake chưa đầy đủ

1. Yêu cầu hoàn thiện template và bằng chứng còn thiếu trong một comment dạng checklist.
2. Dừng review sâu cho đến khi các vấn đề intake được giải quyết.

Xem tiếp:

- [Mục 5.1](#51-definition-of-ready-dor-trước-khi-yêu-cầu-review)

### 1.2 `CI Required Gate` đang thất bại

1. Phân tuyến lỗi qua CI map và ưu tiên sửa các gate mang tính quyết định trước.
2. Chỉ đánh giá lại rủi ro sau khi CI trả về tín hiệu rõ ràng.

Xem tiếp:

- [docs/ci-map.md](ci-map.md)
- [Mục 4.2](#42-bước-b-validation)

### 1.3 Đụng đến đường dẫn rủi ro cao

1. Chuyển sang luồng review sâu.
2. Yêu cầu rollback rõ ràng, bằng chứng về failure mode và kiểm tra ranh giới bảo mật.

Xem tiếp:

- [Mục 9](#9-quy-tắc-bảo-mật-và-ổn-định)
- [docs/reviewer-playbook.md](reviewer-playbook.md)

### 1.4 PR bị supersede hoặc trùng lặp

1. Yêu cầu liên kết supersede rõ ràng và dọn dẹp hàng đợi.
2. Đóng PR bị supersede sau khi maintainer xác nhận.

Xem tiếp:

- [Mục 8.2](#82-kiểm-soát-áp-lực-backlog)

---

## 2. Mục tiêu quản trị và vòng kiểm soát

### 2.1 Mục tiêu quản trị

1. Giữ thông lượng merge có thể dự đoán được khi tải PR lớn.
2. Giữ chất lượng tín hiệu CI ở mức cao (phản hồi nhanh, ít false positive).
3. Giữ review bảo mật rõ ràng đối với các bề mặt rủi ro.
4. Giữ các thay đổi dễ suy luận và dễ hoàn tác.
5. Giữ các artifact trong repository không bị rò rỉ dữ liệu cá nhân/nhạy cảm.

### 2.2 Logic thiết kế quản trị (vòng kiểm soát)

Workflow này được phân lớp có chủ đích để giảm tải cho reviewer trong khi vẫn đảm bảo trách nhiệm rõ ràng:

1. **Phân loại intake:** nhãn theo đường dẫn/kích thước/rủi ro/module phân tuyến PR đến độ sâu review phù hợp.
2. **Validation mang tính quyết định:** merge gate phụ thuộc vào các kiểm tra tái tạo được, không phải comment mang tính chủ quan.
3. **Độ sâu review theo rủi ro:** đường dẫn rủi ro cao kích hoạt review sâu; đường dẫn rủi ro thấp được xử lý nhanh.
4. **Hợp đồng merge ưu tiên rollback:** mọi đường dẫn merge đều bao gồm các bước phục hồi cụ thể.

Tự động hóa hỗ trợ việc triage và bảo vệ, nhưng trách nhiệm merge cuối cùng vẫn thuộc về maintainer và tác giả PR.

---

## 3. Cài đặt repository bắt buộc

Duy trì các quy tắc branch protection sau trên `main`:

- Yêu cầu status check trước khi merge.
- Yêu cầu check `CI Required Gate`.
- Yêu cầu review pull request trước khi merge.
- Yêu cầu review CODEOWNERS cho các đường dẫn được bảo vệ.
- Với `.github/workflows/**`, yêu cầu phê duyệt từ owner qua `CI Required Gate` (`WORKFLOW_OWNER_LOGINS`) và giới hạn quyền bypass branch/ruleset cho org owner.
- Hủy bỏ approval cũ khi có commit mới được đẩy lên.
- Hạn chế force-push trên các branch được bảo vệ.

---

## 4. Sổ tay vòng đời PR

### 4.1 Bước A: Intake

- Contributor mở PR với `.github/pull_request_template.md` đầy đủ.
- `PR Labeler` áp dụng nhãn phạm vi/đường dẫn + nhãn kích thước + nhãn rủi ro + nhãn module (ví dụ `channel:telegram`, `provider:kimi`, `tool:shell`) và bậc contributor theo số PR đã merge (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50), đồng thời loại bỏ trùng lặp nhãn phạm vi ít cụ thể hơn khi đã có nhãn module cụ thể hơn.
- Đối với tất cả các tiền tố module, nhãn module được nén gọn để giảm nhiễu: một module cụ thể giữ `prefix:component`, nhưng nhiều module cụ thể thu gọn thành nhãn phạm vi cơ sở `prefix`.
- Thứ tự nhãn ưu tiên đầu tiên: `risk:*` -> `size:*` -> bậc contributor -> nhãn module/đường dẫn.
- Maintainer có thể chạy `PR Labeler` thủ công (`workflow_dispatch`) ở chế độ `audit` để kiểm tra drift hoặc chế độ `repair` để chuẩn hóa metadata nhãn được quản lý trên toàn repository.
- Di chuột qua nhãn trên GitHub hiển thị mô tả được quản lý tự động (tóm tắt quy tắc/ngưỡng).
- Màu nhãn được quản lý được sắp xếp theo thứ tự hiển thị để tạo gradient mượt mà trên các hàng nhãn dài.
- `PR Auto Responder` đăng hướng dẫn lần đầu, xử lý phân tuyến dựa trên nhãn cho các mục tín hiệu thấp và tự động áp dụng bậc contributor cho issue với cùng ngưỡng như `PR Labeler` (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50).

### 4.2 Bước B: Validation

- `CI Required Gate` là merge gate.
- PR chỉ thay đổi tài liệu sử dụng fast-path và bỏ qua các Rust job nặng.
- PR không phải tài liệu phải vượt qua lint, test và kiểm tra smoke release build.

### 4.3 Bước C: Review

- Reviewer ưu tiên theo nhãn rủi ro và kích thước.
- Các đường dẫn nhạy cảm về bảo mật (`src/security`, `src/runtime`, `src/gateway` và CI workflow) yêu cầu sự chú ý của maintainer.
- PR lớn (`size: L`/`size: XL`) nên được chia nhỏ trừ khi có lý do thuyết phục.

### 4.4 Bước D: Merge

- Ưu tiên **squash merge** để giữ lịch sử gọn gàng.
- Tiêu đề PR nên theo phong cách Conventional Commit.
- Chỉ merge khi đường dẫn rollback đã được ghi lại.

---

## 5. Hợp đồng sẵn sàng PR (DoR / DoD)

### 5.1 Definition of Ready (DoR) trước khi yêu cầu review

- Template PR đã hoàn thiện đầy đủ.
- Ranh giới phạm vi rõ ràng (những gì đã thay đổi / những gì không thay đổi).
- Bằng chứng validation đã đính kèm (không chỉ là "CI sẽ kiểm tra").
- Các trường bảo mật và rollback đã hoàn thành cho các đường dẫn rủi ro.
- Kiểm tra tính riêng tư/vệ sinh dữ liệu đã hoàn thành và ngôn ngữ test trung lập/theo phạm vi dự án.
- Nếu có ngôn ngữ giống danh tính trong test/ví dụ, cần được chuẩn hóa về nhãn gốc ZeroClaw/dự án.

### 5.2 Definition of Done (DoD) sẵn sàng merge

- `CI Required Gate` đã xanh.
- Các reviewer bắt buộc đã phê duyệt (bao gồm các đường dẫn CODEOWNERS).
- Nhãn phân loại rủi ro khớp với các đường dẫn đã chạm.
- Tác động migration/tương thích đã được ghi lại.
- Đường dẫn rollback cụ thể và nhanh chóng.

---

## 6. Chính sách kích thước và lô PR

### 6.1 Phân loại kích thước

- `size: XS` <= 80 dòng thay đổi
- `size: S` <= 250 dòng thay đổi
- `size: M` <= 500 dòng thay đổi
- `size: L` <= 1000 dòng thay đổi
- `size: XL` > 1000 dòng thay đổi

### 6.2 Chính sách

- Mặc định hướng đến `XS/S/M`.
- PR `L/XL` cần lý do biện minh rõ ràng và bằng chứng test chặt chẽ hơn.
- Nếu tính năng lớn không thể tránh khỏi, chia thành các stacked PR.

### 6.3 Hành vi tự động hóa

- `PR Labeler` áp dụng nhãn `size:*` từ số dòng thay đổi thực tế.
- PR chỉ tài liệu/nặng lockfile được chuẩn hóa để tránh thổi phồng kích thước.

---

## 7. Chính sách đóng góp AI/Agent

PR có sự hỗ trợ AI được chào đón, và review cũng có thể được hỗ trợ bằng agent.

### 7.1 Bắt buộc

1. Tóm tắt PR rõ ràng với ranh giới phạm vi.
2. Bằng chứng test/validation cụ thể.
3. Ghi chú tác động bảo mật và rollback cho các thay đổi rủi ro.

### 7.2 Khuyến nghị

1. Ghi chú ngắn gọn về tool/workflow khi tự động hóa ảnh hưởng đáng kể đến thay đổi.
2. Đoạn prompt/kế hoạch tùy chọn để tái tạo được.

Chúng tôi **không** yêu cầu contributor định lượng quyền sở hữu dòng AI-vs-human.

### 7.3 Trọng tâm review cho PR nặng AI

- Tương thích hợp đồng.
- Ranh giới bảo mật.
- Xử lý lỗi và hành vi fallback.
- Hồi quy hiệu suất và bộ nhớ.

---

## 8. SLA review và kỷ luật hàng đợi

- Mục tiêu triage maintainer đầu tiên: trong vòng 48 giờ.
- Nếu PR bị chặn, maintainer để lại một checklist hành động được.
- Tự động hóa `stale` được dùng để giữ hàng đợi lành mạnh; maintainer có thể áp dụng `no-stale` khi cần.
- Tự động hóa `pr-hygiene` kiểm tra các PR mở mỗi 12 giờ và đăng nhắc nhở khi PR không có commit mới trong 48+ giờ và hoặc là đang tụt hậu so với `main` hoặc thiếu/thất bại `CI Required Gate` trên head commit.

### 8.1 Kiểm soát ngân sách hàng đợi

- Sử dụng ngân sách hàng đợi review: giới hạn số PR đang được review sâu đồng thời mỗi maintainer và giữ phần còn lại ở trạng thái triage.
- Đối với công việc stacked, yêu cầu `Depends on #...` rõ ràng để thứ tự review mang tính quyết định.

### 8.2 Kiểm soát áp lực backlog

- Nếu một PR mới thay thế một PR cũ đang mở, yêu cầu `Supersedes #...` và đóng PR cũ sau khi maintainer xác nhận.
- Đánh dấu các PR ngủ đông/dư thừa bằng `stale-candidate` hoặc `superseded` để giảm nỗ lực review trùng lặp.

### 8.3 Kỷ luật triage issue

- `r:needs-repro` cho báo cáo lỗi chưa đầy đủ (yêu cầu repro mang tính quyết định trước khi triage sâu).
- `r:support` cho các mục sử dụng/trợ giúp nên xử lý ngoài bug backlog.
- Nhãn `invalid` / `duplicate` kích hoạt tự động hóa đóng **chỉ issue** kèm hướng dẫn.

### 8.4 Bảo vệ tác dụng phụ của tự động hóa

- `PR Auto Responder` loại bỏ trùng lặp comment dựa trên nhãn để tránh spam.
- Các luồng đóng tự động chỉ giới hạn cho issue, không phải PR.
- Maintainer có thể đóng băng tính toán lại rủi ro tự động bằng `risk: manual` khi ngữ cảnh yêu cầu ghi đè thủ công.

---

## 9. Quy tắc bảo mật và ổn định

Các thay đổi ở những khu vực này yêu cầu review chặt chẽ hơn và bằng chứng test mạnh hơn:

- `src/security/**`
- Quản lý tiến trình runtime.
- Hành vi ingress/xác thực gateway (`src/gateway/**`).
- Ranh giới truy cập filesystem.
- Hành vi mạng/xác thực.
- GitHub workflow và pipeline release.
- Các tool có khả năng thực thi (`src/tools/**`).

### 9.1 Tối thiểu cho PR rủi ro

- Tuyên bố mối đe dọa/rủi ro.
- Ghi chú biện pháp giảm thiểu.
- Các bước rollback.

### 9.2 Khuyến nghị cho PR rủi ro cao

- Bao gồm một test tập trung chứng minh hành vi ranh giới.
- Bao gồm một kịch bản failure mode rõ ràng và sự suy giảm mong đợi.

Đối với các đóng góp có hỗ trợ agent, reviewer cũng nên xác minh rằng tác giả hiểu hành vi runtime và blast radius.

---

## 10. Giao thức phục hồi sự cố

Nếu một PR đã merge gây ra hồi quy:

1. Revert PR ngay lập tức trên `main`.
2. Mở issue theo dõi với phân tích nguyên nhân gốc.
3. Chỉ đưa lại bản sửa lỗi khi có test hồi quy.

Ưu tiên khôi phục nhanh chất lượng dịch vụ hơn là bản vá hoàn hảo nhưng chậm trễ.

---

## 11. Checklist merge của maintainer

- Phạm vi tập trung và dễ hiểu.
- CI gate đã xanh.
- Kiểm tra chất lượng tài liệu đã xanh khi tài liệu thay đổi.
- Các trường tác động bảo mật đã hoàn thành.
- Các trường tính riêng tư/vệ sinh dữ liệu đã hoàn thành và bằng chứng đã được biên tập/ẩn danh.
- Ghi chú workflow agent đủ để tái tạo (nếu tự động hóa được sử dụng).
- Kế hoạch rollback rõ ràng.
- Tiêu đề commit theo Conventional Commits.

---

## 12. Mô hình vận hành review agent

Để giữ chất lượng review ổn định khi khối lượng PR cao, sử dụng mô hình review hai làn.

### 12.1 Làn A: triage nhanh (thân thiện với agent)

- Xác nhận độ đầy đủ của template PR.
- Xác nhận tín hiệu CI gate (`CI Required Gate`).
- Xác nhận phân loại rủi ro qua nhãn và các đường dẫn đã chạm.
- Xác nhận tuyên bố rollback tồn tại.
- Xác nhận phần tính riêng tư/vệ sinh dữ liệu và các yêu cầu diễn đạt trung lập đã được thỏa mãn.
- Xác nhận bất kỳ ngôn ngữ giống danh tính nào đều sử dụng thuật ngữ gốc ZeroClaw/dự án.

### 12.2 Làn B: review sâu (dựa trên rủi ro)

Bắt buộc cho các thay đổi rủi ro cao (security/runtime/gateway/CI):

- Xác thực giả định mô hình mối đe dọa.
- Xác thực hành vi failure mode và suy giảm.
- Xác thực tương thích ngược và tác động migration.
- Xác thực tác động observability/logging.

---

## 13. Ưu tiên hàng đợi và kỷ luật nhãn

### 13.1 Khuyến nghị thứ tự triage

1. `size: XS`/`size: S` + sửa lỗi/bảo mật.
2. `size: M` thay đổi tập trung.
3. `size: L`/`size: XL` yêu cầu chia nhỏ hoặc review theo giai đoạn.

### 13.2 Kỷ luật nhãn

- Nhãn đường dẫn xác định quyền sở hữu hệ thống con nhanh chóng.
- Nhãn kích thước điều hướng chiến lược lô.
- Nhãn rủi ro điều hướng độ sâu review (`risk: low/medium/high`).
- Nhãn module (`<module>: <component>`) cải thiện phân tuyến reviewer cho các thay đổi cụ thể theo integration và các module mới được thêm vào trong tương lai.
- `risk: manual` cho phép maintainer bảo tồn phán đoán rủi ro của con người khi tự động hóa thiếu ngữ cảnh.
- `no-stale` được dành riêng cho công việc đã được chấp nhận nhưng bị chặn.

---

## 14. Hợp đồng bàn giao agent

Khi một agent bàn giao cho agent khác (hoặc cho maintainer), bao gồm:

1. Ranh giới phạm vi (những gì đã thay đổi / những gì không thay đổi).
2. Bằng chứng validation.
3. Rủi ro mở và những điều chưa biết.
4. Hành động tiếp theo được đề xuất.

Điều này giữ cho tổn thất ngữ cảnh ở mức thấp và tránh việc phải đào sâu lặp lại.

---

## 15. Tài liệu liên quan

- [README.md](README.md) — phân loại và điều hướng tài liệu.
- [ci-map.md](ci-map.md) — bản đồ quyền sở hữu và triage CI workflow.
- [reviewer-playbook.md](reviewer-playbook.md) — mô hình thực thi của reviewer.
- [actions-source-policy.md](actions-source-policy.md) — chính sách allowlist nguồn action.

---

## 16. Ghi chú bảo trì

- **Chủ sở hữu:** các maintainer chịu trách nhiệm về quản trị cộng tác và chất lượng merge.
- **Kích hoạt cập nhật:** thay đổi branch protection, thay đổi chính sách nhãn/rủi ro, cập nhật quản trị hàng đợi hoặc thay đổi quy trình review agent.
- **Lần review cuối:** 2026-02-18.
