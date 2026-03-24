# 自定义提供商配置

ZeroClaw 支持兼容 OpenAI 和兼容 Anthropic 的自定义 API 端点。

## 提供商类型

### 兼容 OpenAI 的端点（`custom:`）

适用于实现 OpenAI API 格式的服务：

```toml
default_provider = "custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

### 兼容 Anthropic 的端点（`anthropic-custom:`）

适用于实现 Anthropic API 格式的服务：

```toml
default_provider = "anthropic-custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

## 配置方法

### 配置文件

编辑 `~/.zeroclaw/config.toml`：

```toml
api_key = "your-api-key"
default_provider = "anthropic-custom:https://api.example.com"
default_model = "claude-sonnet-4-6"
```

### 环境变量

对于 `custom:` 和 `anthropic-custom:` 提供商，使用通用密钥环境变量：

```bash
export API_KEY="your-api-key"
# 或：export ZEROCLAW_API_KEY="your-api-key"
zeroclaw agent
```

## llama.cpp 服务器（推荐本地设置）

ZeroClaw 包含 `llama-server` 的一流本地提供商支持：

- 提供商 ID：`llamacpp`（别名：`llama.cpp`）
- 默认端点：`http://localhost:8080/v1`
- API 密钥可选，除非 `llama-server` 启动时指定了 `--api-key`

启动本地服务器（示例）：

```bash
llama-server -hf ggml-org/gpt-oss-20b-GGUF --jinja -c 133000 --host 127.0.0.1 --port 8033
```

然后配置 ZeroClaw：

```toml
default_provider = "llamacpp"
api_url = "http://127.0.0.1:8033/v1"
default_model = "ggml-org/gpt-oss-20b-GGUF"
default_temperature = 0.7
```

快速验证：

```bash
zeroclaw models refresh --provider llamacpp
zeroclaw agent -m "hello"
```

此流程不需要导出 `ZEROCLAW_API_KEY=dummy`。

## SGLang 服务器

ZeroClaw 包含 [SGLang](https://github.com/sgl-project/sglang) 的一流本地提供商支持：

- 提供商 ID：`sglang`
- 默认端点：`http://localhost:30000/v1`
- API 密钥可选，除非服务器要求认证

启动本地服务器（示例）：

```bash
python -m sglang.launch_server --model meta-llama/Llama-3.1-8B-Instruct --port 30000
```

然后配置 ZeroClaw：

```toml
default_provider = "sglang"
default_model = "meta-llama/Llama-3.1-8B-Instruct"
default_temperature = 0.7
```

快速验证：

```bash
zeroclaw models refresh --provider sglang
zeroclaw agent -m "hello"
```

此流程不需要导出 `ZEROCLAW_API_KEY=dummy`。

## vLLM 服务器

ZeroClaw 包含 [vLLM](https://docs.vllm.ai/) 的一流本地提供商支持：

- 提供商 ID：`vllm`
- 默认端点：`http://localhost:8000/v1`
- API 密钥可选，除非服务器要求认证

启动本地服务器（示例）：

```bash
vllm serve meta-llama/Llama-3.1-8B-Instruct
```

然后配置 ZeroClaw：

```toml
default_provider = "vllm"
default_model = "meta-llama/Llama-3.1-8B-Instruct"
default_temperature = 0.7
```

快速验证：

```bash
zeroclaw models refresh --provider vllm
zeroclaw agent -m "hello"
```

此流程不需要导出 `ZEROCLAW_API_KEY=dummy`。

## 测试配置

验证你的自定义端点：

```bash
# 交互模式
zeroclaw agent

# 单条消息测试
zeroclaw agent -m "test message"
```

## 故障排除

### 认证错误

- 验证 API 密钥正确
- 检查端点 URL 格式（必须包含 `http://` 或 `https://`）
- 确保端点可从你的网络访问

### 模型未找到

- 确认模型名称与提供商可用模型匹配
- 查看提供商文档获取准确的模型标识符
- 确保端点和模型系列匹配。某些自定义网关仅暴露部分模型。
- 使用你配置的同一端点和密钥验证可用模型：

```bash
curl -sS https://your-api.com/models \
  -H "Authorization: Bearer $API_KEY"
```

- 如果网关未实现 `/models`，发送最小化聊天请求并检查提供商返回的模型错误文本。

### 连接问题

- 测试端点可访问性：`curl -I https://your-api.com`
- 验证防火墙/代理设置
- 检查提供商状态页面

## 示例

### 本地 LLM 服务器（通用自定义端点）

```toml
default_provider = "custom:http://localhost:8080/v1"
api_key = "your-api-key-if-required"
default_model = "local-model"
```

### 企业代理

```toml
default_provider = "anthropic-custom:https://llm-proxy.corp.example.com"
api_key = "internal-token"
```

### 云提供商网关

```toml
default_provider = "custom:https://gateway.cloud-provider.com/v1"
api_key = "gateway-api-key"
default_model = "gpt-4"
```
