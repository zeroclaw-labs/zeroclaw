# Hướng dẫn Matrix E2EE

Hướng dẫn này giải thích cách chạy ZeroClaw ổn định trong các phòng Matrix, bao gồm các phòng mã hóa đầu cuối (E2EE).

Tài liệu tập trung vào lỗi phổ biến mà người dùng báo cáo:

> "Matrix đã cấu hình đúng, kiểm tra thành công, nhưng bot không phản hồi."

## 0. FAQ nhanh (triệu chứng lớp #499)

Nếu Matrix có vẻ đã kết nối nhưng không có phản hồi, hãy xác minh những điều sau trước:

1. Người gửi được cho phép bởi `allowed_users` (khi kiểm tra: `["*"]`).
2. Tài khoản bot đã tham gia đúng phòng mục tiêu.
3. Token thuộc về cùng tài khoản bot (kiểm tra bằng `whoami`).
4. Phòng mã hóa có identity thiết bị (`device_id`) và chia sẻ key hợp lệ.
5. Daemon đã được khởi động lại sau khi thay đổi cấu hình.

---

## 1. Yêu cầu

Trước khi kiểm tra luồng tin nhắn, hãy đảm bảo tất cả các điều sau đều đúng:

1. Tài khoản bot đã tham gia phòng mục tiêu.
2. Access token thuộc về cùng tài khoản bot.
3. `room_id` chính xác:
   - ưu tiên: canonical room ID (`!room:server`)
   - được hỗ trợ: room alias (`#alias:server`) và ZeroClaw sẽ tự resolve
4. `allowed_users` cho phép người gửi (`["*"]` để kiểm tra mở).
5. Với phòng E2EE, thiết bị bot đã nhận được encryption key cho phòng.

---

## 2. Cấu hình

Dùng `~/.zeroclaw/config.toml`:

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_your_token"

# Optional but recommended for E2EE stability:
user_id = "@zeroclaw:matrix.example.com"
device_id = "DEVICEID123"

# Room ID or alias
room_id = "!xtHhdHIIVEZbDPvTvZ:matrix.example.com"
# room_id = "#ops:matrix.example.com"

# Use ["*"] during initial verification, then tighten.
allowed_users = ["*"]
```

### Về `user_id` và `device_id`

- ZeroClaw cố đọc identity từ Matrix `/_matrix/client/v3/account/whoami`.
- Nếu `whoami` không trả về `device_id`, hãy đặt `device_id` thủ công.
- Các gợi ý này đặc biệt quan trọng để khôi phục phiên E2EE.

---

## 3. Quy trình Xác minh Nhanh

1. Chạy thiết lập channel và daemon:

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

1. Gửi một tin nhắn văn bản thuần trong phòng Matrix đã cấu hình.

2. Xác nhận log ZeroClaw có thông tin khởi động Matrix listener và không có lỗi sync/auth lặp lại.

3. Trong phòng mã hóa, xác minh bot có thể đọc và phản hồi tin nhắn mã hóa từ các người dùng được phép.

---

## 4. Xử lý sự cố "Không có Phản hồi"

Dùng checklist này theo thứ tự.

### A. Phòng và tư cách thành viên

- Đảm bảo tài khoản bot đã tham gia phòng.
- Nếu dùng alias (`#...`), xác minh nó resolve về đúng canonical room.

### B. Allowlist người gửi

- Nếu `allowed_users = []`, tất cả tin nhắn đến đều bị từ chối.
- Để chẩn đoán, tạm thời đặt `allowed_users = ["*"]`.

### C. Token và identity

- Xác thực token bằng:

```bash
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://matrix.example.com/_matrix/client/v3/account/whoami"
```

- Kiểm tra `user_id` trả về khớp với tài khoản bot.
- Nếu `device_id` bị thiếu, đặt `channels_config.matrix.device_id` thủ công.

### D. Kiểm tra dành riêng cho E2EE

- Thiết bị bot phải nhận được room key từ các thiết bị tin cậy.
- Nếu key không được chia sẻ tới thiết bị này, các sự kiện mã hóa không thể giải mã.
- Xác minh độ tin cậy thiết bị và chia sẻ key trong quy trình Matrix client/admin của bạn.
- Nếu log hiện `matrix_sdk_crypto::backups: Trying to backup room keys but no backup key was found`, quá trình khôi phục key backup chưa được bật trên thiết bị này. Cảnh báo này thường không gây lỗi nghiêm trọng cho luồng tin nhắn trực tiếp, nhưng bạn vẫn nên hoàn thiện thiết lập key backup/recovery.
- Nếu người nhận thấy tin nhắn bot là "unverified", hãy xác minh/ký thiết bị bot từ một phiên Matrix tin cậy và giữ `channels_config.matrix.device_id` ổn định qua các lần khởi động lại.

### E. Định dạng tin nhắn (Markdown)

- ZeroClaw gửi phản hồi văn bản Matrix dưới dạng nội dung `m.room.message` hỗ trợ markdown.
- Các Matrix client hỗ trợ `formatted_body` sẽ render in đậm, danh sách và code block.
- Nếu định dạng hiển thị dưới dạng văn bản thuần, kiểm tra khả năng của client trước, sau đó xác nhận ZeroClaw đang chạy bản build bao gồm Matrix output hỗ trợ markdown.

### F. Kiểm tra fresh start

Sau khi cập nhật cấu hình, khởi động lại daemon và gửi tin nhắn mới (không chỉ xem lại lịch sử cũ).

---

## 5. Ghi chú Vận hành

- Giữ Matrix token tránh khỏi log và ảnh chụp màn hình.
- Bắt đầu với `allowed_users` thoáng, sau đó thu hẹp về các user ID cụ thể.
- Ưu tiên dùng canonical room ID trong production để tránh alias drift.

---

## 6. Tài liệu Liên quan

- [Channels Reference](./channels-reference.md)
- [Phụ lục từ khoá log vận hành](./channels-reference.md#7-operations-appendix-log-keywords-matrix)
- [Network Deployment](./network-deployment.md)
- [Agnostic Security](agnostic-security.md)
- [Reviewer Playbook](reviewer-playbook.md)
