# Hướng dẫn Tích hợp LangGraph

Hướng dẫn này giải thích cách sử dụng gói Python `zeroclaw-tools` để gọi tool nhất quán với bất kỳ LLM provider nào tương thích OpenAI.

## Bối cảnh

Một số LLM provider, đặc biệt là các model Trung Quốc như GLM-5 (Zhipu AI), có hành vi gọi tool không nhất quán khi dùng phương thức text-based tool invocation. Core Rust của ZeroClaw sử dụng structured tool calling theo định dạng OpenAI API, nhưng một số model phản hồi tốt hơn với cách tiếp cận khác.

LangGraph cung cấp một stateful graph execution engine đảm bảo hành vi gọi tool nhất quán bất kể khả năng native của model nền tảng.

## Kiến trúc

```
┌─────────────────────────────────────────────────────────────┐
│                      Your Application                        │
├─────────────────────────────────────────────────────────────┤
│                   zeroclaw-tools Agent                       │
│                                                              │
│   ┌─────────────────────────────────────────────────────┐   │
│   │              LangGraph StateGraph                    │   │
│   │                                                      │   │
│   │    ┌────────────┐         ┌────────────┐            │   │
│   │    │   Agent    │ ──────▶ │   Tools    │            │   │
│   │    │   Node     │ ◀────── │   Node     │            │   │
│   │    └────────────┘         └────────────┘            │   │
│   │         │                       │                    │   │
│   │         ▼                       ▼                    │   │
│   │    [Continue?]            [Execute Tool]             │   │
│   │         │                       │                    │   │
│   │    Yes │ No                Result│                    │   │
│   │         ▼                       ▼                    │   │
│   │      [END]              [Back to Agent]              │   │
│   │                                                      │   │
│   └─────────────────────────────────────────────────────┘   │
│                                                              │
├─────────────────────────────────────────────────────────────┤
│            OpenAI-Compatible LLM Provider                    │
│   (Z.AI, OpenRouter, Groq, DeepSeek, Ollama, etc.)          │
└─────────────────────────────────────────────────────────────┘
```

## Bắt đầu nhanh

### Cài đặt

```bash
pip install zeroclaw-tools
```

### Sử dụng cơ bản

```python
import asyncio
from zeroclaw_tools import create_agent, shell, file_read, file_write
from langchain_core.messages import HumanMessage

async def main():
    agent = create_agent(
        tools=[shell, file_read, file_write],
        model="glm-5",
        api_key="your-api-key",
        base_url="https://api.z.ai/api/coding/paas/v4"
    )

    result = await agent.ainvoke({
        "messages": [HumanMessage(content="Read /etc/hostname and tell me the machine name")]
    })

    print(result["messages"][-1].content)

asyncio.run(main())
```

## Các Tool Hiện có

### Tool cốt lõi

| Tool | Mô tả |
|------|-------|
| `shell` | Thực thi lệnh shell |
| `file_read` | Đọc nội dung file |
| `file_write` | Ghi nội dung vào file |

### Tool mở rộng

| Tool | Mô tả |
|------|-------|
| `web_search` | Tìm kiếm web (yêu cầu `BRAVE_API_KEY`) |
| `http_request` | Thực hiện HTTP request |
| `memory_store` | Lưu dữ liệu vào bộ nhớ lâu dài |
| `memory_recall` | Truy xuất dữ liệu đã lưu |

## Tool tùy chỉnh

Tạo tool riêng của bạn bằng decorator `@tool`:

```python
from zeroclaw_tools import tool, create_agent

@tool
def get_weather(city: str) -> str:
    """Get the current weather for a city."""
    # Your implementation
    return f"Weather in {city}: Sunny, 25°C"

@tool
def query_database(sql: str) -> str:
    """Execute a SQL query and return results."""
    # Your implementation
    return "Query returned 5 rows"

agent = create_agent(
    tools=[get_weather, query_database],
    model="glm-5",
    api_key="your-key"
)
```

## Cấu hình Provider

### Z.AI / GLM-5

```python
agent = create_agent(
    model="glm-5",
    api_key="your-zhipu-key",
    base_url="https://api.z.ai/api/coding/paas/v4"
)
```

### OpenRouter

```python
agent = create_agent(
    model="anthropic/claude-sonnet-4-6",
    api_key="your-openrouter-key",
    base_url="https://openrouter.ai/api/v1"
)
```

### Groq

```python
agent = create_agent(
    model="llama-3.3-70b-versatile",
    api_key="your-groq-key",
    base_url="https://api.groq.com/openai/v1"
)
```

### Ollama (cục bộ)

```python
agent = create_agent(
    model="llama3.2",
    base_url="http://localhost:11434/v1"
)
```

## Tích hợp Discord Bot

```python
import os
from zeroclaw_tools.integrations import DiscordBot

bot = DiscordBot(
    token=os.environ["DISCORD_TOKEN"],
    guild_id=123456789,  # Your Discord server ID
    allowed_users=["123456789"],  # User IDs that can use the bot
    api_key=os.environ["API_KEY"],
    model="glm-5"
)

bot.run()
```

## Sử dụng qua CLI

```bash
# Set environment variables
export API_KEY="your-key"
export BRAVE_API_KEY="your-brave-key"  # Optional, for web search

# Single message
zeroclaw-tools "What is the current date?"

# Interactive mode
zeroclaw-tools -i
```

## So sánh với Rust ZeroClaw

| Khía cạnh | Rust ZeroClaw | zeroclaw-tools |
|--------|---------------|-----------------|
| **Hiệu năng** | Cực nhanh (~10ms khởi động) | Khởi động Python (~500ms) |
| **Bộ nhớ** | <5 MB | ~50 MB |
| **Kích thước binary** | ~3.4 MB | pip package |
| **Tính nhất quán của tool** | Phụ thuộc model | LangGraph đảm bảo |
| **Khả năng mở rộng** | Rust traits | Python decorators |
| **Hệ sinh thái** | Rust crates | PyPI packages |

**Khi nào dùng Rust ZeroClaw:**
- Triển khai edge cho môi trường production
- Môi trường hạn chế tài nguyên (Raspberry Pi, v.v.)
- Yêu cầu hiệu năng tối đa

**Khi nào dùng zeroclaw-tools:**
- Các model có tool calling native không nhất quán
- Phát triển trung tâm vào Python
- Prototyping nhanh
- Tích hợp với hệ sinh thái Python ML

## Xử lý sự cố

### Lỗi "API key required"

Đặt biến môi trường `API_KEY` hoặc truyền `api_key` vào `create_agent()`.

### Tool call không được thực thi

Đảm bảo model của bạn hỗ trợ function calling. Một số model cũ có thể không hỗ trợ tool.

### Rate limiting

Thêm độ trễ giữa các lần gọi hoặc tự triển khai rate limiting:

```python
import asyncio

for message in messages:
    result = await agent.ainvoke({"messages": [message]})
    await asyncio.sleep(1)  # Rate limit
```

## Dự án Liên quan

- [rs-graph-llm](https://github.com/a-agmon/rs-graph-llm) - Rust LangGraph alternative
- [langchain-rust](https://github.com/Abraxas-365/langchain-rust) - LangChain for Rust
- [llm-chain](https://github.com/sobelio/llm-chain) - LLM chains in Rust
