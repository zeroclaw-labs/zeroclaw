# 故障排查（简体中文）

这是 Wave 1 首版本地化页面，用于快速定位常见故障与恢复路径。

英文原文：

- [../../troubleshooting.md](../../troubleshooting.md)

## 适用场景

- 安装失败、运行异常、通道故障
- 结合 `status`/`doctor` 做分层诊断
- 执行最小化回滚与恢复验证

## 使用建议

- 错误码、日志字段与命令名保持英文。
- 详细故障签名以英文原文为准。

## 更新说明

### `web_search_tool` 报 `403`/`429` 错误

**现象**：出现类似 `DuckDuckGo search failed with status: 403`（或 `429`）的报错。

**原因**：部分网络/代理屏蔽了 DuckDuckGo HTML 接口。

**修复选项**：

1. 切换至 Brave：
```toml
[web_search]
enabled = true
provider = "brave"
brave_api_key = "<SECRET>"
```

2. 切换至 Exa：
```toml
[web_search]
enabled = true
provider = "exa"
api_key = "<SECRET>"
# 可选
# api_url = "https://api.exa.ai/search"
```

3. 切换至 Tavily：
```toml
[web_search]
enabled = true
provider = "tavily"
api_key = "<SECRET>"
# 可选
# api_url = "https://api.tavily.com/search"
```

4. 切换至 Firecrawl（仅当 build 包含该功能时）：
```toml
[web_search]
enabled = true
provider = "firecrawl"
api_key = "<SECRET>"
```

### shell tool 中 `curl`/`wget` 被策略拦截

**现象**：输出包含 `Command blocked: high-risk command is disallowed by policy`。

**原因**：`curl`/`wget` 被自主性策略视为高风险命令而拦截。

**修复**：改用专用工具替代 shell fetch：
- `http_request`：直接 API/HTTP 调用
- `web_fetch`：页面内容抓取/提炼

最小配置：
```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
provider = "fast_html2md"
allowed_domains = ["*"]
```

### `web_fetch`/`http_request` — Host not allowed

**现象**：出现类似 `Host '<domain>' is not in http_request.allowed_domains` 的报错。

**修复**：在配置中添加对应域名，或使用 `"*"` 允许所有公共访问：
```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
allowed_domains = ["*"]
blocked_domains = []
```

**安全提示**：即使设置 `"*"`，本地/私有网络仍会被拦截。
