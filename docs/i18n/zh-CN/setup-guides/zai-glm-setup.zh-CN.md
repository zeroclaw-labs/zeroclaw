# Z.AI GLM（智谱大模型）安装指南

ZeroClaw 通过兼容 OpenAI 的端点支持 Z.AI 的 GLM 模型。
本指南介绍与当前 ZeroClaw 提供商行为匹配的实用安装选项。

## 概述

ZeroClaw 开箱即用支持以下 Z.AI 别名和端点：

| 别名 | 端点 | 说明 |
|-------|----------|-------|
| `zai` | `https://api.z.ai/api/coding/paas/v4` | 全球端点 |
| `zai-cn` | `https://open.bigmodel.cn/api/paas/v4` | 中国区端点 |

如果你需要自定义基础 URL，请查看 [`../contributing/custom-providers.zh-CN.md`](../contributing/custom-providers.zh-CN.md)。

## 安装

### 快速开始

```bash
zeroclaw onboard \
  --provider \"zai\" \
  --api-key \"YOUR_ZAI_API_KEY\"
```

### 手动配置

编辑 `~/.zeroclaw/config.toml`：

```toml
api_key = \"YOUR_ZAI_API_KEY\"
default_provider = \"zai\"
default_model = \"glm-5\"
default_temperature = 0.7
```

## 可用模型

| 模型 | 描述 |
|-------|-------------|
| `glm-5` | 引导流程默认模型；最强推理能力 |
| `glm-4.7` | 强大的通用质量 |
| `glm-4.6` | 平衡基线 |
| `glm-4.5-air` | 低延迟选项 |

模型可用性可能因账户/地区而异，如有疑问请使用 `/models` API 查询。

## 验证安装

### 使用 curl 测试

```bash
# 测试兼容 OpenAI 的端点
curl -X POST \"https://api.z.ai/api/coding/paas/v4/chat/completions\" \
  -H \"Authorization: Bearer YOUR_ZAI_API_KEY\" \
  -H \"Content-Type: application/json\" \
  -d '{
    \"model\": \"glm-5\",
    \"messages\": [{\"role\": \"user\", \"content\": \"Hello\"}]
  }'
```

预期响应：
```json
{
  \"choices\": [{
    \"message\": {
      \"content\": \"Hello! How can I help you today?\",
      \"role\": \"assistant\"
    }
  }]
}
```

### 使用 ZeroClaw CLI 测试

```bash
# 直接测试代理
echo \"Hello\" | zeroclaw agent

# 检查状态
zeroclaw status
```

## 环境变量

添加到你的 `.env` 文件：

```bash
# Z.AI API 密钥
ZAI_API_KEY=your-id.secret

# 可选通用密钥（许多提供商使用）
# API_KEY=your-id.secret
```

密钥格式为 `id.secret`（例如：`abc123.xyz789`）。

## 故障排除

### 速率限制

**症状：** `rate_limited` 错误

**解决方案：**
- 等待并重试
- 检查你的 Z.AI 套餐限制
- 尝试使用 `glm-4.5-air` 以获得更低延迟和更高配额容忍度

### 认证错误

**症状：** 401 或 403 错误

**解决方案：**
- 验证你的 API 密钥格式为 `id.secret`
- 检查密钥是否未过期
- 确保密钥中没有额外空格

### 模型未找到

**症状：** 模型不可用错误

**解决方案：**
- 列出可用模型：
```bash
curl -s \"https://api.z.ai/api/coding/paas/v4/models\" \
  -H \"Authorization: Bearer YOUR_ZAI_API_KEY\" | jq '.data[].id'
```

## 获取 API 密钥

1. 前往 [Z.AI](https://z.ai)
2. 注册编码计划
3. 从控制台生成 API 密钥
4. 密钥格式：`id.secret`（例如：`abc123.xyz789`）

## 相关文档

- [ZeroClaw 说明文档](../../../README.zh-CN.md)
- [自定义提供商端点](../contributing/custom-providers.zh-CN.md)
- [贡献指南](../../../../CONTRIBUTING.md)
