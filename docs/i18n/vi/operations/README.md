# Tài liệu vận hành và triển khai

Dành cho operator vận hành ZeroClaw liên tục hoặc trên production.

## Vận hành cốt lõi

- Sổ tay Day-2: [../operations-runbook.md](../operations-runbook.md)
- Runbook probe kết nối provider trong CI: [connectivity-probes-runbook.md](connectivity-probes-runbook.md)
- Sổ tay Release: [../release-process.md](../release-process.md)
- Ma trận xử lý sự cố: [../troubleshooting.md](../troubleshooting.md)
- Triển khai mạng/gateway an toàn: [../network-deployment.md](../network-deployment.md)
- Thiết lập Mattermost (dành riêng cho channel): [../mattermost-setup.md](../mattermost-setup.md)

## Luồng thường gặp

1. Xác thực runtime (`status`, `doctor`, `channel doctor`)
2. Áp dụng từng thay đổi config một lần
3. Khởi động lại service/daemon
4. Xác minh tình trạng channel và gateway
5. Rollback nhanh nếu hành vi bị hồi quy

## Liên quan

- Tham chiếu config: [../config-reference.md](../config-reference.md)
- Bộ sưu tập bảo mật: [../security/README.md](../security/README.md)
