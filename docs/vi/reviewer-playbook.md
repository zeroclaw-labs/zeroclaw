# Sổ tay Reviewer

Tài liệu này là người bạn đồng hành vận hành của [`docs/pr-workflow.md`](pr-workflow.md).
Để điều hướng tài liệu rộng hơn, xem [`docs/README.md`](README.md).

## 0. Tóm tắt

- **Mục đích:** định nghĩa mô hình vận hành reviewer mang tính quyết định, duy trì chất lượng review cao khi khối lượng PR lớn.
- **Đối tượng:** maintainer, reviewer và reviewer có hỗ trợ agent.
- **Phạm vi:** triage intake, phân tuyến rủi ro-sang-độ-sâu, kiểm tra review sâu, ghi đè tự động hóa và giao thức bàn giao.
- **Ngoài phạm vi:** thay thế thẩm quyền chính sách PR trong `CONTRIBUTING.md` hoặc thẩm quyền workflow trong các file CI.

---

## 1. Lối tắt theo tình huống review

Dùng phần này để phân tuyến nhanh trước khi đọc chi tiết đầy đủ.

### 1.1 Intake thất bại trong 5 phút đầu

1. Để lại một comment dạng checklist hành động được.
2. Dừng review sâu cho đến khi các vấn đề intake được sửa.

Xem tiếp:

- [Mục 3.1](#31-triage-intake-năm-phút)

### 1.2 Rủi ro cao hoặc không rõ ràng

1. Mặc định coi là `risk: high`.
2. Yêu cầu review sâu và bằng chứng rollback rõ ràng.

Xem tiếp:

- [Mục 2](#2-ma-trận-quyết-định-độ-sâu-review)
- [Mục 3.3](#33-checklist-review-sâu-rủi-ro-cao)

### 1.3 Kết quả tự động hóa sai/ồn ào

1. Áp dụng giao thức ghi đè (`risk: manual`, loại bỏ trùng lặp comment/nhãn).
2. Tiếp tục review với lý do rõ ràng.

Xem tiếp:

- [Mục 5](#5-giao-thức-ghi-đè-tự-động-hóa)

### 1.4 Cần bàn giao review

1. Bàn giao với phạm vi/rủi ro/validation/vấn đề chặn.
2. Giao hành động tiếp theo cụ thể.

Xem tiếp:

- [Mục 6](#6-giao-thức-bàn-giao)

---

## 2. Ma trận quyết định độ sâu review

| Nhãn rủi ro | Đường dẫn thường gặp | Độ sâu review tối thiểu | Bằng chứng bắt buộc |
|---|---|---|---|
| `risk: low` | docs/tests/chore, thay đổi không ảnh hưởng runtime | 1 reviewer + CI gate | validation cục bộ nhất quán + không mơ hồ hành vi |
| `risk: medium` | `src/providers/**`, `src/channels/**`, `src/memory/**`, `src/config/**` | 1 reviewer có hiểu biết về hệ thống con + xác minh hành vi | bằng chứng kịch bản tập trung + tác dụng phụ rõ ràng |
| `risk: high` | `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**` | triage nhanh + review sâu + sẵn sàng rollback | kiểm tra bảo mật/failure mode + rõ ràng về rollback |

Khi không chắc chắn, coi là `risk: high`.

Nếu việc gán nhãn rủi ro tự động không đúng ngữ cảnh, maintainer có thể áp dụng `risk: manual` và đặt nhãn `risk:*` cuối cùng một cách tường minh.

---

## 3. Quy trình review tiêu chuẩn

### 3.1 Triage intake năm phút

Cho mỗi PR mới:

1. Xác nhận độ đầy đủ template (`summary`, `validation`, `security`, `rollback`).
2. Xác nhận nhãn hiện diện và hợp lý:
   - `size:*`, `risk:*`
   - nhãn phạm vi (ví dụ `provider`, `channel`, `security`)
   - nhãn có phạm vi module (`channel:*`, `provider:*`, `tool:*`)
   - nhãn bậc contributor khi áp dụng được
3. Xác nhận trạng thái tín hiệu CI (`CI Required Gate`).
4. Xác nhận phạm vi là một mối quan tâm (từ chối mega-PR hỗn hợp trừ khi có lý do).
5. Xác nhận các yêu cầu tính riêng tư/vệ sinh dữ liệu và diễn đạt test trung lập đã được thỏa mãn.

Nếu bất kỳ yêu cầu intake nào thất bại, để lại một comment dạng checklist hành động được thay vì review sâu.

### 3.2 Checklist fast-lane (tất cả PR)

- Ranh giới phạm vi rõ ràng và đáng tin cậy.
- Các lệnh validation hiện diện và kết quả nhất quán.
- Các thay đổi hành vi hướng người dùng đã được ghi lại.
- Tác giả thể hiện hiểu biết về hành vi và blast radius (đặc biệt với PR có hỗ trợ agent).
- Đường dẫn rollback cụ thể (không chỉ là "revert").
- Tác động tương thích/migration rõ ràng.
- Không có rò rỉ dữ liệu cá nhân/nhạy cảm trong diff artifact; ví dụ/test giữ trung lập và theo phạm vi dự án.
- Nếu có ngôn ngữ giống danh tính, nó sử dụng vai trò gốc ZeroClaw/dự án (không phải danh tính cá nhân hay thực tế).
- Quy ước đặt tên và ranh giới kiến trúc tuân theo hợp đồng dự án (`AGENTS.md`, `CONTRIBUTING.md`).

### 3.3 Checklist review sâu (rủi ro cao)

Với PR rủi ro cao, xác minh ít nhất một ví dụ cụ thể trong mỗi hạng mục:

- **Ranh giới bảo mật:** hành vi deny-by-default được bảo tồn, không mở rộng phạm vi ngẫu nhiên.
- **Failure mode:** xử lý lỗi rõ ràng và suy giảm an toàn.
- **Ổn định hợp đồng:** tương thích CLI/config/API được bảo tồn hoặc migration được ghi lại.
- **Observability:** lỗi có thể chẩn đoán mà không rò rỉ secret.
- **An toàn rollback:** đường dẫn revert và blast radius rõ ràng.

### 3.4 Phong cách kết quả comment review

Ưu tiên comment dạng checklist với một kết quả rõ ràng:

- **Sẵn sàng merge** (giải thích lý do).
- **Cần tác giả hành động** (danh sách vấn đề chặn có thứ tự).
- **Cần review bảo mật/runtime sâu hơn** (nêu rõ rủi ro và bằng chứng yêu cầu).

Tránh comment mơ hồ tạo ra độ trễ qua lại không cần thiết.

---

## 4. Triage issue và quản trị backlog

### 4.1 Sổ tay nhãn triage issue

Dùng nhãn để giữ backlog có thể hành động:

- `r:needs-repro` cho báo cáo lỗi chưa đầy đủ.
- `r:support` cho câu hỏi sử dụng/hỗ trợ nên chuyển hướng ngoài bug backlog.
- `duplicate` / `invalid` cho trùng lặp/nhiễu không thể hành động.
- `no-stale` cho công việc đã được chấp nhận đang chờ vấn đề chặn bên ngoài.
- Yêu cầu biên tập khi log/payload chứa định danh cá nhân hoặc dữ liệu nhạy cảm.

### 4.2 Giao thức cắt tỉa backlog PR

Khi nhu cầu review vượt quá năng lực, áp dụng thứ tự này:

1. Giữ PR bug/security đang hoạt động (`size: XS/S`) ở đầu hàng đợi.
2. Yêu cầu các PR chồng chéo hợp nhất; đóng các PR cũ hơn là `superseded` sau khi xác nhận.
3. Đánh dấu PR ngủ đông là `stale-candidate` trước khi cửa sổ đóng stale bắt đầu.
4. Yêu cầu rebase + validation mới trước khi mở lại công việc kỹ thuật stale/superseded.

---

## 5. Giao thức ghi đè tự động hóa

Dùng khi kết quả tự động hóa tạo ra tác dụng phụ cho review:

1. **Nhãn rủi ro sai:** thêm `risk: manual`, rồi đặt nhãn `risk:*` mong muốn.
2. **Tự đóng sai trên triage issue:** mở lại issue, xóa nhãn route, để lại một comment làm rõ.
3. **Spam/nhiễu nhãn:** giữ một comment maintainer chuẩn tắc và xóa nhãn route dư thừa.
4. **Phạm vi PR mơ hồ:** yêu cầu chia nhỏ trước khi review sâu.

---

## 6. Giao thức bàn giao

Nếu bàn giao review cho maintainer/agent khác, bao gồm:

1. Tóm tắt phạm vi.
2. Phân loại rủi ro hiện tại và lý do.
3. Những gì đã được validate.
4. Các vấn đề chặn mở.
5. Hành động tiếp theo được đề xuất.

---

## 7. Vệ sinh hàng đợi hàng tuần

- Review hàng đợi stale và chỉ áp dụng `no-stale` cho công việc đã được chấp nhận nhưng bị chặn.
- Ưu tiên PR bug/security `size: XS/S` trước.
- Chuyển đổi các issue hỗ trợ tái diễn thành cập nhật tài liệu và hướng dẫn auto-response.

---

## 8. Tài liệu liên quan

- [README.md](README.md) — phân loại và điều hướng tài liệu.
- [pr-workflow.md](pr-workflow.md) — workflow quản trị và hợp đồng merge.
- [ci-map.md](ci-map.md) — bản đồ quyền sở hữu và triage CI.
- [actions-source-policy.md](actions-source-policy.md) — chính sách allowlist nguồn action.

---

## 9. Ghi chú bảo trì

- **Chủ sở hữu:** các maintainer chịu trách nhiệm về chất lượng review và thông lượng hàng đợi.
- **Kích hoạt cập nhật:** thay đổi chính sách PR, thay đổi mô hình phân tuyến rủi ro hoặc thay đổi hành vi ghi đè tự động hóa.
- **Lần review cuối:** 2026-02-18.
