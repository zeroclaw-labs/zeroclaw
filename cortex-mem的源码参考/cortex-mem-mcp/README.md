# Cortex Memory MCP Server

`cortex-mem-mcp` is a server implementation based on [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) that enables AI assistants to interact with the Cortex Memory system for persistent memory storage and retrieval.

## 🧠 Overview

Cortex Memory MCP Server provides six core tools for AI assistants:

- 📝 **store_memory**: Store new memories from conversations
- 🔍 **query_memory**: Semantic vector search with L0/L1/L2 layered results
- 📋 **list_memories**: Browse stored memory entries
- 📄 **get_memory**: Retrieve complete memory content
- 🗑️ **delete_memory**: Delete specific memory entries
- 📊 **get_abstract**: Get L0 abstract summary (~100 tokens)

## 🛠️ MCP Tools

### 1. `store_memory`

Store a new memory in the Cortex Memory system.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `content` | string | ✅ | - | Memory content to store |
| `thread_id` | string | ❌ | `"default"` | Session ID for organizing related memories |
| `role` | string | ❌ | `"user"` | Message role: `"user"`, `"assistant"`, or `"system"` |

#### Example Request

```json
{
  "content": "User prefers dark theme and likes vim keybindings",
  "thread_id": "user-preferences",
  "role": "user"
}
```

#### Response

```json
{
  "success": true,
  "uri": "cortex://session/user-preferences/timeline/2024/01/15/14_30_45_abc123.md",
  "message_id": "2024-01-15T14:30:45Z-abc123"
}
```

---

### 2. `query_memory`

Search memories using semantic vector search with L0/L1/L2 layered results.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `query` | string | ✅ | - | Search query string |
| `thread_id` | string | ❌ | - | Limit search to this session |
| `limit` | number | ❌ | `10` | Maximum number of results |
| `scope` | string | ❌ | `"session"` | Search scope: `"session"`, `"user"`, or `"agent"` |

#### Scope URI Mapping

| Scope | URI Pattern |
|-------|-------------|
| `session` | `cortex://session` |
| `user` | `cortex://user` |
| `agent` | `cortex://agent` |
| (with thread_id) | `cortex://session/{thread_id}` |

#### Example Request

```json
{
  "query": "Rust OAuth implementation method",
  "thread_id": "technical-discussions",
  "limit": 5,
  "scope": "session"
}
```

#### Response

```json
{
  "success": true,
  "query": "Rust OAuth implementation method",
  "results": [
    {
      "uri": "cortex://session/tech-disc/timeline/2024/01/10/09_15_30_def456.md",
      "score": 0.92,
      "snippet": "...discussed using OAuth2 client library for authentication in Rust applications..."
    }
  ],
  "total": 1
}
```

---

### 3. `list_memories`

List memories from a specific URI path.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `uri` | string | ❌ | `"cortex://session"` | URI path to list |
| `limit` | number | ❌ | `50` | Maximum number of entries |
| `include_abstracts` | boolean | ❌ | `false` | Include L0 abstracts |

#### Supported URI Patterns

| URI Pattern | Description |
|-------------|-------------|
| `cortex://session` | List all sessions |
| `cortex://user/{user-id}` | List user memories |
| `cortex://agent/{agent-id}` | List agent memories |
| `cortex://session/{session-id}/timeline` | List session timeline |

#### Example Request

```json
{
  "uri": "cortex://session",
  "limit": 20,
  "include_abstracts": true
}
```

#### Response

```json
{
  "success": true,
  "uri": "cortex://session",
  "entries": [
    {
      "name": "user-preferences",
      "uri": "cortex://session/user-preferences",
      "is_directory": true,
      "size": 2048,
      "abstract_text": "User preference settings and options"
    }
  ],
  "total": 1
}
```

---

### 4. `get_memory`

Retrieve complete content of a specific memory.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `uri` | string | ✅ | - | Full URI of the memory |

#### Example Request

```json
{
  "uri": "cortex://session/user-preferences/timeline/2024/01/15/14_30_45_abc123.md"
}
```

#### Response

```json
{
  "success": true,
  "uri": "cortex://session/user-preferences/timeline/2024/01/15/14_30_45_abc123.md",
  "content": "# Message\n\nUser prefers dark theme and likes vim keybindings.\n\n---\n*Timestamp: 2024-01-15T14:30:45Z*\n*Role: user*"
}
```

---

### 5. `delete_memory`

Delete a specific memory entry.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `uri` | string | ✅ | - | URI of the memory to delete |

#### Example Request

```json
{
  "uri": "cortex://session/user-preferences/timeline/2024/01/15/14_30_45_abc123.md"
}
```

#### Response

```json
{
  "success": true,
  "uri": "cortex://session/user-preferences/timeline/2024/01/15/14_30_45_abc123.md"
}
```

---

### 6. `get_abstract`

Get the L0 abstract summary (~100 tokens) of a memory for quick relevance checking.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `uri` | string | ✅ | - | URI of the memory |

#### Example Request

```json
{
  "uri": "cortex://session/user-preferences/timeline/2024/01/15/14_30_45_abc123.md"
}
```

#### Response

```json
{
  "success": true,
  "uri": "cortex://session/user-preferences/timeline/2024/01/15/14_30_45_abc123.md",
  "abstract_text": "User preferences: dark theme, vim keybindings"
}
```

## 🚀 Installation & Configuration

### Build Requirements

- Rust 1.70 or later
- Cross-platform support: Linux, macOS, Windows

### Build

```bash
# Clone repository
git clone https://github.com/sopaco/cortex-mem.git
cd cortex-mem

# Build the server
cargo build --release --bin cortex-mem-mcp

# Binary location
./target/release/cortex-mem-mcp
```

### Command-line Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `--config` / `-c` | `config.toml` | Path to configuration file |
| `--tenant` | `default` | Tenant ID for memory isolation |

### Configure Claude Desktop

Edit Claude Desktop configuration file:

**macOS**:
```bash
open ~/Library/Application\ Support/Claude/claude_desktop_config.json
```

**Windows**:
```bash
notepad %APPDATA%\Claude\claude_desktop_config.json
```

Add configuration:

```json
{
  "mcpServers": {
    "cortex-memory": {
      "command": "/path/to/cortex-mem-mcp",
      "args": [
        "--config", "/path/to/config.toml",
        "--tenant", "default"
      ],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

### Configuration File (config.toml)

```toml
[cortex]
# Data directory (optional, has smart defaults)
data_dir = "/path/to/cortex-data"

[llm]
# LLM API configuration
api_base_url = "https://api.openai.com/v1"
api_key = "your-api-key"
model_efficient = "gpt-4o-mini"
temperature = 0.1
max_tokens = 4096

[embedding]
# Embedding configuration
api_base_url = "https://api.openai.com/v1"
api_key = "your-embedding-api-key"
model_name = "text-embedding-3-small"
batch_size = 10
timeout_secs = 30

[qdrant]
# Vector database configuration
url = "http://localhost:6333"
collection_name = "cortex_memories"
embedding_dim = 1536
timeout_secs = 30
```

### Data Directory Resolution

Priority order:
1. `cortex.data_dir` config value
2. `CORTEX_DATA_DIR` environment variable
3. System app data directory (e.g., `%APPDATA%/tars/cortex` on Windows)
4. Fallback: `./.cortex` in current directory

## 🔄 MCP Workflow

### Typical Memory Workflow

```javascript
// 1. Start of conversation: Query relevant memories
await query_memory({
  query: "user preferences",
  scope: "user",
  limit: 5
});

// 2. During conversation: Store new information
await store_memory({
  content: "User mentioned they are learning Rust async programming",
  thread_id: "learning-journey",
  role: "user"
});

// 3. End of conversation: Store summary
await store_memory({
  content: "Discussed Rust async/await, Pin, and Future. User understood the basics.",
  thread_id: "rust-async-discussion",
  role: "assistant"
});
```

### Advanced Search Strategy

```javascript
// 1. Search in sessions first
const sessionResults = await query_memory({
  query: "Rust error handling",
  scope: "session",
  limit: 5
});

// 2. If more context needed, search user memories
if (sessionResults.results.length < 3) {
  const userResults = await query_memory({
    query: "Rust error handling",
    scope: "user",
    limit: 5
  });
  // Merge results
  sessionResults.results.push(...userResults.results);
}

// 3. Get full content
const fullContent = await get_memory({
  uri: sessionResults.results[0].uri
});

// 4. Or get abstract for quick preview
const abstract = await get_abstract({
  uri: sessionResults.results[0].uri
});
```

## 🔧 Troubleshooting

### Common Issues

#### 1. Connection Failed

**Error**: `Failed to connect to MCP server`

**Solution**:
1. Check Claude Desktop configuration file path
2. Verify binary file path and permissions
3. View log output

```bash
# Test run
RUST_LOG=debug ./cortex-mem-mcp --config config.toml --tenant default
```

#### 2. Memory Storage Failed

**Error**: `Failed to store memory`

**Solution**:
1. Check data directory permissions
2. Verify LLM API configuration
3. Confirm Qdrant service is running
4. Check embedding configuration

```bash
# Check directory permissions
ls -la ./cortex-data
chmod 755 ./cortex-data

# Check Qdrant connection
curl http://localhost:6333/collections
```

#### 3. Empty Search Results

**Error**: `Search returned empty results`

**Solution**:
1. Check if memories exist
2. Verify search query format
3. Confirm search scope

```javascript
// Test listing
await list_memories({
  uri: "cortex://session",
  limit: 50
});
```

#### 4. Qdrant Connection Failed

**Error**: `Failed to connect to Qdrant`

**Solution**:
1. Ensure Qdrant service is running
2. Check URL configuration
3. Verify collection name exists

```bash
# Start Qdrant (Docker)
docker run -p 6333:6333 qdrant/qdrant

# Check connection
curl http://localhost:6333
```

### Debug Mode

```bash
# Enable verbose logging
RUST_LOG=debug ./cortex-mem-mcp --config config.toml --tenant default
```

## 🔗 Related Resources

- [Cortex Memory Main Documentation](../README.md)
- [Cortex Memory Core](../cortex-mem-core/README.md)
- [Cortex Memory Tools](../cortex-mem-tools/README.md)
- [Model Context Protocol](https://modelcontextprotocol.io/)
- [Claude Desktop MCP Documentation](https://docs.anthropic.com/claude/docs/mcp)

## 🤝 Contributing

Contributions are welcome! Please follow these steps:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Create a Pull Request

## 📄 License

MIT License - see the [LICENSE](../../LICENSE) file for details.

---

**Built with ❤️ using Rust and Model Context Protocol**