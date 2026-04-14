# Sổ tay Vận hành QuantClaw

Tài liệu này dành cho các operator chịu trách nhiệm duy trì tính sẵn sàng, tình trạng bảo mật và xử lý sự cố.

Cập nhật lần cuối: **2026-02-18**.

## Phạm vi

Dùng tài liệu này cho các tác vụ vận hành day-2:

- khởi động và giám sát runtime
- kiểm tra sức khoẻ và chẩn đoán hệ thống
- triển khai an toàn và rollback
- phân loại và khôi phục sau sự cố

Nếu đây là lần cài đặt đầu tiên, hãy bắt đầu từ [one-click-bootstrap.md](one-click-bootstrap.md).

## Các chế độ Runtime

| Chế độ | Lệnh | Khi nào dùng |
|---|---|---|
| Foreground runtime | `quantclaw daemon` | gỡ lỗi cục bộ, phiên ngắn |
| Foreground gateway only | `quantclaw gateway` | kiểm thử webhook endpoint |
| User service | `quantclaw service install && quantclaw service start` | runtime được quản lý liên tục bởi operator |

## Checklist Cơ bản cho Operator

1. Xác thực cấu hình:

```bash
quantclaw status
```

2. Kiểm tra chẩn đoán:

```bash
quantclaw doctor
quantclaw channel doctor
```

3. Khởi động runtime:

```bash
quantclaw daemon
```

4. Để chạy như user session service liên tục:

```bash
quantclaw service install
quantclaw service start
quantclaw service status
```

## Tín hiệu Sức khoẻ và Trạng thái

| Tín hiệu | Lệnh / File | Kỳ vọng |
|---|---|---|
| Tính hợp lệ của config | `quantclaw doctor` | không có lỗi nghiêm trọng |
| Kết nối channel | `quantclaw channel doctor` | các channel đã cấu hình đều khoẻ mạnh |
| Tóm tắt runtime | `quantclaw status` | provider/model/channels như mong đợi |
| Heartbeat/trạng thái daemon | `~/.quantclaw/daemon_state.json` | file được cập nhật định kỳ |

## Log và Chẩn đoán

### macOS / Windows (log của service wrapper)

- `~/.quantclaw/logs/daemon.stdout.log`
- `~/.quantclaw/logs/daemon.stderr.log`

### Linux (systemd user service)

```bash
journalctl --user -u quantclaw.service -f
```

## Quy trình Phân loại Sự cố (Fast Path)

1. Chụp trạng thái hệ thống:

```bash
quantclaw status
quantclaw doctor
quantclaw channel doctor
```

2. Kiểm tra trạng thái service:

```bash
quantclaw service status
```

3. Nếu service không khoẻ, khởi động lại sạch:

```bash
quantclaw service stop
quantclaw service start
```

4. Nếu các channel vẫn thất bại, kiểm tra allowlist và thông tin xác thực trong `~/.quantclaw/config.toml`.

5. Nếu liên quan đến gateway, kiểm tra cài đặt bind/auth (`[gateway]`) và khả năng tiếp cận cục bộ.

## Quy trình Thay đổi An toàn

Trước khi áp dụng thay đổi cấu hình:

1. sao lưu `~/.quantclaw/config.toml`
2. chỉ áp dụng một thay đổi logic tại một thời điểm
3. chạy `quantclaw doctor`
4. khởi động lại daemon/service
5. xác minh bằng `status` + `channel doctor`

## Quy trình Rollback

Nếu một lần triển khai gây ra suy giảm hành vi:

1. khôi phục `config.toml` trước đó
2. khởi động lại runtime (`daemon` hoặc `service`)
3. xác nhận khôi phục qua `doctor` và kiểm tra sức khoẻ channel
4. ghi lại nguyên nhân gốc rễ và biện pháp khắc phục sự cố

## Tài liệu Liên quan

- [one-click-bootstrap.md](one-click-bootstrap.md)
- [troubleshooting.md](troubleshooting.md)
- [config-reference.md](config-reference.md)
- [commands-reference.md](commands-reference.md)
