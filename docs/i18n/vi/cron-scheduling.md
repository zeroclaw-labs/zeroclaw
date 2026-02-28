# Hệ thống Cron & Lập lịch

ZeroClaw bao gồm hệ thống lập lịch công việc đầy đủ tính năng để chạy các tác vụ theo lịch trình, tại thời điểm cụ thể, hoặc theo khoảng thời gian đều đặn.

## Bắt đầu nhanh

```bash
# Thêm cron job (chạy mỗi ngày lúc 9 giờ sáng)
zeroclaw cron add '0 9 * * *' 'echo "Chào buổi sáng!"'

# Thêm nhắc nhở một lần (chạy sau 30 phút)
zeroclaw cron once 30m 'notify-send "Hết giờ!"'

# Thêm job theo khoảng thời gian (chạy mỗi 5 phút)
zeroclaw cron add-every 300000 'curl -s http://api.example.com/health'

# Liệt kê tất cả jobs
zeroclaw cron list

# Xóa một job
zeroclaw cron remove <job-id>
```

## Loại lịch trình

### Biểu thức Cron (`kind: "cron"`)

Biểu thức cron tiêu chuẩn với hỗ trợ múi giờ tùy chọn.

```bash
# Mỗi ngày làm việc lúc 9 giờ sáng giờ Pacific
zeroclaw cron add '0 9 * * 1-5' --tz 'America/Los_Angeles' 'echo "Giờ làm việc"'

# Mỗi giờ
zeroclaw cron add '0 * * * *' 'echo "Kiểm tra hàng giờ"'

# Mỗi 15 phút
zeroclaw cron add '*/15 * * * *' 'curl http://localhost:8080/ping'
```

**Định dạng:** `phút giờ ngày-trong-tháng tháng ngày-trong-tuần`

| Trường | Giá trị |
|--------|---------|
| phút | 0-59 |
| giờ | 0-23 |
| ngày-trong-tháng | 1-31 |
| tháng | 1-12 |
| ngày-trong-tuần | 0-6 (CN-T7) |

### Chạy một lần (`kind: "at"`)

Chạy đúng một lần tại thời điểm cụ thể.

```bash
# Tại thời điểm ISO cụ thể
zeroclaw cron add-at '2026-03-15T14:30:00Z' 'echo "Cuộc họp bắt đầu!"'

# Độ trễ tương đối (thân thiện với người dùng)
zeroclaw cron once 2h 'echo "Hai giờ sau"'
zeroclaw cron once 30m 'echo "Nhắc nhở nửa giờ"'
zeroclaw cron once 1d 'echo "Ngày mai"'
```

**Đơn vị độ trễ:** `s` (giây), `m` (phút), `h` (giờ), `d` (ngày)

### Khoảng thời gian (`kind: "every"`)

Chạy lặp lại theo khoảng thời gian cố định.

```bash
# Mỗi 5 phút (300000 ms)
zeroclaw cron add-every 300000 'echo "Ping"'

# Mỗi giờ (3600000 ms)
zeroclaw cron add-every 3600000 'curl http://api.example.com/sync'
```

## Loại công việc

### Shell Jobs

Thực thi lệnh shell trực tiếp:

```bash
zeroclaw cron add '0 6 * * *' 'backup.sh && notify-send "Sao lưu xong"'
```

### Agent Jobs

Gửi prompt đến AI agent:

```toml
# Trong zeroclaw.toml
[[cron.jobs]]
schedule = { kind = "cron", expr = "0 9 * * *", tz = "America/Los_Angeles" }
job_type = "agent"
prompt = "Kiểm tra lịch của tôi và tóm tắt các sự kiện hôm nay"
session_target = "main"  # hoặc "isolated"
```

## Nhắm mục tiêu phiên

Kiểm soát nơi agent jobs chạy:

| Mục tiêu | Hành vi |
|----------|---------|
| `isolated` (mặc định) | Tạo phiên mới, không có lịch sử |
| `main` | Chạy trong phiên chính với ngữ cảnh đầy đủ |

```toml
[[cron.jobs]]
schedule = { kind = "every", every_ms = 1800000 }  # 30 phút
job_type = "agent"
prompt = "Kiểm tra email mới và tóm tắt những email khẩn cấp"
session_target = "main"  # Có quyền truy cập lịch sử hội thoại
```

## Cấu hình gửi kết quả

Định tuyến output của job đến các kênh:

```toml
[[cron.jobs]]
schedule = { kind = "cron", expr = "0 8 * * *" }
job_type = "agent"
prompt = "Tạo bản tóm tắt buổi sáng"
session_target = "isolated"

[cron.jobs.delivery]
mode = "channel"
channel = "telegram"
to = "123456789"  # Telegram chat ID
best_effort = true  # Không thất bại nếu gửi thất bại
```

**Các chế độ gửi:**
- `none` - Không gửi output (mặc định)
- `channel` - Gửi đến kênh cụ thể
- `notify` - Thông báo hệ thống

## Lệnh CLI

| Lệnh | Mô tả |
|------|-------|
| `zeroclaw cron list` | Hiển thị tất cả jobs đã lập lịch |
| `zeroclaw cron add <expr> <cmd>` | Thêm job với biểu thức cron |
| `zeroclaw cron add-at <time> <cmd>` | Thêm job chạy một lần tại thời điểm |
| `zeroclaw cron add-every <ms> <cmd>` | Thêm job theo khoảng thời gian |
| `zeroclaw cron once <delay> <cmd>` | Thêm job chạy một lần với độ trễ |
| `zeroclaw cron update <id> [opts]` | Cập nhật cài đặt job |
| `zeroclaw cron remove <id>` | Xóa một job |
| `zeroclaw cron pause <id>` | Tạm dừng (vô hiệu hóa) job |
| `zeroclaw cron resume <id>` | Tiếp tục (kích hoạt) job |

## Tệp cấu hình

Định nghĩa jobs trong `zeroclaw.toml`:

```toml
[[cron.jobs]]
name = "morning-briefing"
schedule = { kind = "cron", expr = "0 8 * * 1-5", tz = "America/New_York" }
job_type = "agent"
prompt = "Chào buổi sáng! Kiểm tra lịch, email và thời tiết của tôi."
session_target = "main"
enabled = true

[[cron.jobs]]
name = "health-check"
schedule = { kind = "every", every_ms = 60000 }
job_type = "shell"
command = "curl -sf http://localhost:8080/health || notify-send 'Dịch vụ ngừng hoạt động!'"
enabled = true

[[cron.jobs]]
name = "daily-backup"
schedule = { kind = "cron", expr = "0 2 * * *" }
job_type = "shell"
command = "/home/user/scripts/backup.sh"
enabled = true
```

## Tích hợp công cụ

Hệ thống cron cũng có sẵn dưới dạng agent tools:

| Tool | Mô tả |
|------|-------|
| `cron_add` | Tạo cron job mới |
| `cron_list` | Liệt kê tất cả jobs |
| `cron_remove` | Xóa một job |
| `cron_update` | Sửa đổi một job |
| `cron_run` | Chạy ngay một job |
| `cron_runs` | Hiển thị lịch sử chạy gần đây |

### Ví dụ: Agent tạo nhắc nhở

```
Người dùng: Nhắc tôi gọi điện cho mẹ sau 2 giờ
Agent: [sử dụng cron_add với kind="at" và delay="2h"]
Xong! Tôi sẽ nhắc bạn gọi điện cho mẹ lúc 4:30 chiều.
```

## Di chuyển từ OpenClaw

Hệ thống cron của ZeroClaw tương thích với lập lịch của OpenClaw:

| OpenClaw | ZeroClaw |
|----------|----------|
| `kind: "cron"` | `kind = "cron"` ✅ |
| `kind: "every"` | `kind = "every"` ✅ |
| `kind: "at"` | `kind = "at"` ✅ |
| `sessionTarget: "main"` | `session_target = "main"` ✅ |
| `sessionTarget: "isolated"` | `session_target = "isolated"` ✅ |
| `payload.kind: "systemEvent"` | `job_type = "agent"` |
| `payload.kind: "agentTurn"` | `job_type = "agent"` |

**Khác biệt chính:** ZeroClaw sử dụng định dạng TOML, OpenClaw sử dụng JSON.

## Thực hành tốt nhất

1. **Sử dụng múi giờ** cho lịch trình hướng đến người dùng (cuộc họp, nhắc nhở)
2. **Sử dụng khoảng thời gian** cho các tác vụ nền (kiểm tra sức khỏe, đồng bộ)
3. **Sử dụng chạy một lần** cho nhắc nhở và hành động trì hoãn
4. **Đặt `session_target = "main"`** khi agent cần ngữ cảnh hội thoại
5. **Sử dụng `delivery`** để định tuyến output đến kênh phù hợp

## Xử lý sự cố

**Job không chạy?**
- Kiểm tra `zeroclaw cron list` - nó có được bật không?
- Xác minh biểu thức cron đúng
- Kiểm tra cài đặt múi giờ

**Agent job không có ngữ cảnh?**
- Thay đổi `session_target` từ `"isolated"` sang `"main"`

**Output không được gửi?**
- Xác minh `delivery.channel` đã được cấu hình
- Kiểm tra kênh đích đang hoạt động
