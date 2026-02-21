# Thiết lập Z.AI GLM

ZeroClaw hỗ trợ các model GLM của Z.AI thông qua các endpoint tương thích OpenAI.
Hướng dẫn cấu hình thực tế theo provider hiện tại của ZeroClaw.

## Tổng quan

ZeroClaw hỗ trợ sẵn các alias và endpoint Z.AI sau đây:

| Alias | Endpoint | Ghi chú |
|-------|----------|---------|
| `zai` | `https://api.z.ai/api/coding/paas/v4` | Endpoint toàn cầu |
| `zai-cn` | `https://open.bigmodel.cn/api/paas/v4` | Endpoint Trung Quốc |

Nếu bạn cần base URL tùy chỉnh, xem `docs/custom-providers.md`.

## Thiết lập

### Bắt đầu nhanh

```bash
zeroclaw onboard \
  --provider "zai" \
  --api-key "YOUR_ZAI_API_KEY"
```

### Cấu hình thủ công

Chỉnh sửa `~/.zeroclaw/config.toml`:

```toml
api_key = "YOUR_ZAI_API_KEY"
default_provider = "zai"
default_model = "glm-5"
default_temperature = 0.7
```

## Các model hiện có

| Model | Mô tả |
|-------|-------|
| `glm-5` | Mặc định khi onboarding; khả năng suy luận mạnh nhất |
| `glm-4.7` | Chất lượng đa năng cao |
| `glm-4.6` | Mức cơ bản cân bằng |
| `glm-4.5-air` | Tùy chọn độ trễ thấp hơn |

Khả năng khả dụng của model có thể thay đổi theo tài khoản/khu vực, hãy dùng API `/models` khi không chắc chắn.

## Xác minh thiết lập

### Kiểm tra bằng curl

```bash
# Test OpenAI-compatible endpoint
curl -X POST "https://api.z.ai/api/coding/paas/v4/chat/completions" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "glm-5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

Phản hồi mong đợi:
```json
{
  "choices": [{
    "message": {
      "content": "Hello! How can I help you today?",
      "role": "assistant"
    }
  }]
}
```

### Kiểm tra bằng ZeroClaw CLI

```bash
# Test agent directly
echo "Hello" | zeroclaw agent

# Check status
zeroclaw status
```

## Biến môi trường

Thêm vào file `.env` của bạn:

```bash
# Z.AI API Key
ZAI_API_KEY=your-id.secret

# Optional generic key (used by many providers)
# API_KEY=your-id.secret
```

Định dạng key là `id.secret` (ví dụ: `abc123.xyz789`).

## Xử lý sự cố

### Rate Limiting

**Triệu chứng:** Lỗi `rate_limited`

**Giải pháp:**
- Chờ và thử lại
- Kiểm tra giới hạn gói Z.AI của bạn
- Thử `glm-4.5-air` để có độ trễ thấp hơn và khả năng chịu đựng quota cao hơn

### Lỗi xác thực

**Triệu chứng:** Lỗi 401 hoặc 403

**Giải pháp:**
- Xác minh định dạng API key là `id.secret`
- Kiểm tra key chưa hết hạn
- Đảm bảo không có khoảng trắng thừa trong key

### Model không tìm thấy

**Triệu chứng:** Lỗi model không khả dụng

**Giải pháp:**
- Liệt kê các model có sẵn:
```bash
curl -s "https://api.z.ai/api/coding/paas/v4/models" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" | jq '.data[].id'
```

## Lấy API Key

1. Truy cập [Z.AI](https://z.ai)
2. Đăng ký Coding Plan
3. Tạo API key từ dashboard
4. Định dạng key: `id.secret` (ví dụ: `abc123.xyz789`)

## Tài liệu liên quan

- [ZeroClaw README](README.md)
- [Custom Provider Endpoints](./custom-providers.md)
- [Contributing Guide](../../../CONTRIBUTING.md)
