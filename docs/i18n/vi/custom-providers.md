# Cấu hình Provider Tùy chỉnh

ZeroClaw hỗ trợ endpoint API tùy chỉnh cho cả provider tương thích OpenAI lẫn Anthropic.

## Các loại Provider

### Endpoint tương thích OpenAI (`custom:`)

Dành cho các dịch vụ triển khai định dạng API của OpenAI:

```toml
default_provider = "custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

### Endpoint tương thích Anthropic (`anthropic-custom:`)

Dành cho các dịch vụ triển khai định dạng API của Anthropic:

```toml
default_provider = "anthropic-custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

## Phương thức cấu hình

### File Config

Chỉnh sửa `~/.zeroclaw/config.toml`:

```toml
api_key = "your-api-key"
default_provider = "anthropic-custom:https://api.example.com"
default_model = "claude-sonnet-4-6"
```

### Biến môi trường

Với provider `custom:` và `anthropic-custom:`, dùng biến môi trường chứa key chung:

```bash
export API_KEY="your-api-key"
# hoặc: export ZEROCLAW_API_KEY="your-api-key"
zeroclaw agent
```

## Kiểm tra cấu hình

Xác minh endpoint tùy chỉnh của bạn:

```bash
# Chế độ tương tác
zeroclaw agent

# Kiểm tra tin nhắn đơn
zeroclaw agent -m "test message"
```

## Xử lý sự cố

### Lỗi xác thực

- Kiểm tra lại API key
- Kiểm tra định dạng URL endpoint (phải bao gồm `http://` hoặc `https://`)
- Đảm bảo endpoint có thể truy cập từ mạng của bạn

### Không tìm thấy Model

- Xác nhận tên model khớp với các model mà provider cung cấp
- Kiểm tra tài liệu của provider để biết định danh model chính xác
- Đảm bảo endpoint và dòng model khớp nhau. Một số gateway tùy chỉnh chỉ cung cấp một tập con model.
- Xác minh các model có sẵn từ cùng endpoint và key đã cấu hình:

```bash
curl -sS https://your-api.com/models \
  -H "Authorization: Bearer $API_KEY"
```

- Nếu gateway không triển khai `/models`, gửi một request chat tối giản và kiểm tra thông báo lỗi model mà provider trả về.

### Sự cố kết nối

- Kiểm tra khả năng truy cập endpoint: `curl -I https://your-api.com`
- Xác minh cài đặt firewall/proxy
- Kiểm tra trang trạng thái của provider

## Ví dụ

### LLM Server cục bộ

```toml
default_provider = "custom:http://localhost:8080"
default_model = "local-model"
```

### Proxy của doanh nghiệp

```toml
default_provider = "anthropic-custom:https://llm-proxy.corp.example.com"
api_key = "internal-token"
```

### Cloud Provider Gateway

```toml
default_provider = "custom:https://gateway.cloud-provider.com/v1"
api_key = "gateway-api-key"
default_model = "gpt-4"
```
