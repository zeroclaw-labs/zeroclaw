# zeroclaw-tools

Gói Python đồng hành cho [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) — gọi công cụ dựa trên LangGraph cho thực thi agent LLM nhất quán.

## Tại sao cần gói này?

Một số nhà cung cấp LLM (đặc biệt là GLM-5/Zhipu và các model tương tự) có hành vi gọi công cụ không nhất quán khi dùng lời gọi dạng văn bản. Gói này cung cấp phương pháp dựa trên LangGraph mang lại:

- **Gọi công cụ nhất quán** trên mọi provider tương thích OpenAI
- **Vòng lặp công cụ tự động** — tiếp tục gọi cho đến khi hoàn tất tác vụ
- **Dễ mở rộng** — thêm công cụ mới bằng decorator `@tool`
- **Không phụ thuộc framework** — hoạt động với mọi API tương thích OpenAI

## Cài đặt

```bash
pip install zeroclaw-tools
```

Kèm tích hợp Discord:

```bash
pip install zeroclaw-tools[discord]
```

## Bắt đầu nhanh

### Agent cơ bản

```python
import asyncio
from zeroclaw_tools import create_agent, shell, file_read, file_write
from langchain_core.messages import HumanMessage

async def main():
    # Tạo agent với công cụ
    agent = create_agent(
        tools=[shell, file_read, file_write],
        model="glm-5",
        api_key="your-api-key",
        base_url="https://api.z.ai/api/coding/paas/v4"
    )

    # Thực thi tác vụ
    result = await agent.ainvoke({
        "messages": [HumanMessage(content="List files in /tmp directory")]
    })

    print(result["messages"][-1].content)

asyncio.run(main())
```

### Dùng qua CLI

```bash
# Đặt biến môi trường
export API_KEY="your-api-key"
export API_BASE="https://api.z.ai/api/coding/paas/v4"

# Chạy CLI
zeroclaw-tools "List files in the current directory"

# Chế độ tương tác (không cần tin nhắn)
zeroclaw-tools -i
```

### Bot Discord

```python
import os
from zeroclaw_tools.integrations import DiscordBot

bot = DiscordBot(
    token=os.environ["DISCORD_TOKEN"],
    guild_id=123456789,
    allowed_users=["123456789"]
)

bot.run()
```

## Công cụ có sẵn

| Công cụ | Mô tả |
|------|-------------|
| `shell` | Thực thi lệnh shell |
| `file_read` | Đọc nội dung file |
| `file_write` | Ghi nội dung vào file |
| `web_search` | Tìm kiếm web (cần Brave API key) |
| `http_request` | Gửi yêu cầu HTTP |
| `memory_store` | Lưu dữ liệu vào bộ nhớ |
| `memory_recall` | Truy xuất dữ liệu đã lưu |

## Tạo công cụ tùy chỉnh

```python
from zeroclaw_tools import tool

@tool
def my_custom_tool(query: str) -> str:
    """Mô tả công cụ này làm gì."""
    # Viết logic tại đây
    return f"Result for: {query}"

# Dùng với agent
agent = create_agent(tools=[my_custom_tool])
```

## Tương thích provider

Hoạt động với mọi provider tương thích OpenAI:

- **Z.AI / GLM-5** — `https://api.z.ai/api/coding/paas/v4`
- **OpenRouter** — `https://openrouter.ai/api/v1`
- **Groq** — `https://api.groq.com/openai/v1`
- **DeepSeek** — `https://api.deepseek.com`
- **Ollama** — `http://localhost:11434/v1`
- **Và nhiều hơn nữa...**

## Kiến trúc

```
┌─────────────────────────────────────────────┐
│              Ứng dụng của bạn               │
├─────────────────────────────────────────────┤
│           zeroclaw-tools Agent              │
│  ┌─────────────────────────────────────┐   │
│  │         LangGraph StateGraph         │   │
│  │    ┌───────────┐    ┌──────────┐    │   │
│  │    │   Agent   │───▶│   Tools  │    │   │
│  │    │   Node    │◀───│   Node   │    │   │
│  │    └───────────┘    └──────────┘    │   │
│  └─────────────────────────────────────┘   │
├─────────────────────────────────────────────┤
│     Nhà cung cấp LLM tương thích OpenAI    │
└─────────────────────────────────────────────┘
```

## So sánh với Rust ZeroClaw

| Tính năng | Rust ZeroClaw | zeroclaw-tools |
|---------|---------------|----------------|
| **Kích thước binary** | ~3.4 MB | Gói Python |
| **Bộ nhớ** | <5 MB | ~50 MB |
| **Thời gian khởi động** | <10ms | ~500ms |
| **Độ nhất quán công cụ** | Phụ thuộc model | LangGraph đảm bảo |
| **Khả năng mở rộng** | Rust traits | Python decorators |

Dùng **Rust ZeroClaw** cho triển khai biên (edge) trong sản phẩm. Dùng **zeroclaw-tools** khi cần đảm bảo tính nhất quán gọi công cụ hoặc tích hợp hệ sinh thái Python.

## Giấy phép

MIT License — xem [LICENSE](../LICENSE)
