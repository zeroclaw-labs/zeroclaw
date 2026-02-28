# Lược đồ Audit Event CI/Security (Tiếng Việt)

Trang này là bản địa hóa tối thiểu cho tài liệu lược đồ sự kiện kiểm toán.

Bản gốc tiếng Anh:

- [../../audit-event-schema.md](../../audit-event-schema.md)

## Nội dung chính

- Chuẩn envelope sự kiện: `zeroclaw.audit.v1`.
- Các trường chính: `event_type`, `generated_at`, `run_context`, `artifact`, `payload`.
- Danh sách loại sự kiện hiện tại và chính sách retention artifact theo workflow.

## Khi nào dùng

- Thiết kế/kiểm tra lane CI hoặc security mới.
- Cần xác nhận format event cho hệ thống downstream.
- Cập nhật chính sách lưu trữ artifact.

Chi tiết schema và bảng retention đầy đủ xem bản tiếng Anh.
