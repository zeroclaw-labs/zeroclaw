# Hướng dẫn Tích hợp Mattermost

ZeroClaw hỗ trợ tích hợp native với Mattermost thông qua REST API v4. Tích hợp này lý tưởng cho các môi trường self-hosted, riêng tư hoặc air-gapped nơi giao tiếp nội bộ là yêu cầu bắt buộc.

## Điều kiện tiên quyết

1. **Mattermost Server**: Một instance Mattermost đang chạy (self-hosted hoặc cloud).
2. **Tài khoản Bot**:
    - Vào **Main Menu > Integrations > Bot Accounts**.
    - Nhấn **Add Bot Account**.
    - Đặt username (ví dụ: `zeroclaw-bot`).
    - Bật quyền **post:all** và **channel:read** (hoặc các scope phù hợp).
    - Lưu **Access Token**.
3. **Channel ID**:
    - Mở channel Mattermost mà bạn muốn bot theo dõi.
    - Nhấn vào header channel và chọn **View Info**.
    - Sao chép **ID** (ví dụ: `7j8k9l...`).

## Cấu hình

Thêm phần sau vào `config.toml` của bạn trong phần `[channels_config]`:

```toml
[channels_config.mattermost]
url = "https://mm.your-domain.com"
bot_token = "your-bot-access-token"
channel_id = "your-channel-id"
allowed_users = ["user-id-1", "user-id-2"]
thread_replies = true
mention_only = true
```

### Các trường cấu hình

| Trường | Mô tả |
|---|---|
| `url` | Base URL của Mattermost server của bạn. |
| `bot_token` | Personal Access Token của tài khoản bot. |
| `channel_id` | (Tùy chọn) ID của channel cần lắng nghe. Bắt buộc ở chế độ `listen`. |
| `allowed_users` | (Tùy chọn) Danh sách Mattermost User ID được phép tương tác với bot. Dùng `["*"]` để cho phép tất cả mọi người. |
| `thread_replies` | (Tùy chọn) Tin nhắn người dùng ở top-level có được trả lời trong thread không. Mặc định: `true`. Các phản hồi trong thread hiện có luôn ở lại trong thread đó. |
| `mention_only` | (Tùy chọn) Khi `true`, chỉ các tin nhắn đề cập rõ ràng username bot (ví dụ `@zeroclaw-bot`) mới được xử lý. Mặc định: `false`. |

## Cuộc hội thoại dạng Thread

ZeroClaw hỗ trợ Mattermost thread ở cả hai chế độ:
- Nếu người dùng gửi tin nhắn trong một thread hiện có, ZeroClaw luôn phản hồi trong cùng thread đó.
- Nếu `thread_replies = true` (mặc định), tin nhắn top-level được trả lời bằng cách tạo thread trên bài đăng đó.
- Nếu `thread_replies = false`, tin nhắn top-level được trả lời ở cấp độ gốc của channel.

## Chế độ Mention-Only

Khi `mention_only = true`, ZeroClaw áp dụng bộ lọc bổ sung sau khi xác thực `allowed_users`:

- Tin nhắn không đề cập rõ ràng đến bot sẽ bị bỏ qua.
- Tin nhắn có `@bot_username` sẽ được xử lý.
- Token `@bot_username` được loại bỏ trước khi gửi nội dung đến model.

Chế độ này hữu ích trong các channel chia sẻ bận rộn để giảm các lần gọi model không cần thiết.

## Ghi chú Bảo mật

Tích hợp Mattermost được thiết kế cho **giao tiếp nội bộ**. Bằng cách tự host Mattermost server, toàn bộ lịch sử giao tiếp của agent vẫn nằm trong hạ tầng của bạn, tránh việc bên thứ ba ghi lại log.
