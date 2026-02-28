# Tài liệu Bắt đầu

Dành cho cài đặt lần đầu và làm quen nhanh.

## Lộ trình bắt đầu

1. Tổng quan và khởi động nhanh: [docs/i18n/vi/README.md](../README.md)
2. Cài đặt một lệnh và chế độ bootstrap kép: [../one-click-bootstrap.md](../one-click-bootstrap.md)
3. Tìm lệnh theo tác vụ: [../commands-reference.md](../commands-reference.md)

## Chọn hướng đi

| Tình huống | Lệnh |
|----------|---------|
| Có API key, muốn cài nhanh nhất | `zeroclaw onboard --api-key sk-... --provider openrouter` |
| Muốn được hướng dẫn từng bước | `zeroclaw onboard --interactive` |
| Đã có config, chỉ cần sửa kênh | `zeroclaw onboard --channels-only` |
| Dùng xác thực subscription | Xem [Subscription Auth](../../../README.md#subscription-auth-openai-codex--claude-code) |

## Thiết lập và kiểm tra

- Thiết lập nhanh: `zeroclaw onboard --api-key "sk-..." --provider openrouter`
- Thiết lập tương tác: `zeroclaw onboard --interactive`
- Kiểm tra môi trường: `zeroclaw status` + `zeroclaw doctor`

## Tiếp theo

- Vận hành runtime: [../operations/README.md](../operations/README.md)
- Tra cứu tham khảo: [../reference/README.md](../reference/README.md)
