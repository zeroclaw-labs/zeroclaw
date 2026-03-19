# Tài liệu Bắt đầu

Dành cho cài đặt lần đầu và làm quen nhanh.

## Lộ trình bắt đầu

1. Tổng quan và khởi động nhanh: [../../README.vi.md](../../README.vi.md)
2. Cài đặt một lệnh và chế độ bootstrap kép: [one-click-bootstrap.md](one-click-bootstrap.md)
3. Tìm lệnh theo tác vụ: [../reference/cli/commands-reference.md](../reference/cli/commands-reference.md)

## Chọn hướng đi

| Tình huống                      | Lệnh                                                                                    |
| ------------------------------- | --------------------------------------------------------------------------------------- |
| Có API key, muốn cài nhanh nhất | `jhedaiclaw onboard --api-key sk-... --provider openrouter`                             |
| Muốn được hướng dẫn từng bước   | `jhedaiclaw onboard`                                                                    |
| Đã có config, chỉ cần sửa kênh  | `jhedaiclaw onboard --channels-only`                                                    |
| Dùng xác thực subscription      | Xem [Subscription Auth](../../README.vi.md#subscription-auth-openai-codex--claude-code) |

## Thiết lập và kiểm tra

- Thiết lập nhanh: `jhedaiclaw onboard --api-key "sk-..." --provider openrouter`
- Thiết lập hướng dẫn: `jhedaiclaw onboard`
- Kiểm tra môi trường: `jhedaiclaw status` + `jhedaiclaw doctor`

## Tiếp theo

- Vận hành runtime: [../ops/README.md](../ops/README.md)
- Tra cứu tham khảo: [../reference/README.md](../reference/README.md)
