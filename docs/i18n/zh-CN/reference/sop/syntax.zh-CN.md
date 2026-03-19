# SOP 语法参考

SOP 定义从 `sops_dir`（默认：`<workspace>/sops`）下的子目录加载。

## 1. 目录布局

```text
<workspace>/sops/
  deploy-prod/
    SOP.toml
    SOP.md
```

每个 SOP 必须有 `SOP.toml`。`SOP.md` 是可选的，但没有解析步骤的运行会验证失败。

## 2. `SOP.toml`

```toml
[sop]
name = \"deploy-prod\"
description = \"将服务部署到生产环境\"
version = \"1.0.0\"
priority = \"high\"              # low | normal | high | critical
execution_mode = \"supervised\"  # auto | supervised | step_by_step | priority_based
cooldown_secs = 300
max_concurrent = 1

[[triggers]]
type = \"webhook\"
path = \"/sop/deploy\"

[[triggers]]
type = \"manual\"

[[triggers]]
type = \"mqtt\"
topic = \"ops/deploy\"
condition = \"$.env == \\\"prod\\\"\"
```

## 3. `SOP.md` 步骤格式

步骤从 `## Steps` 部分解析。

```md
## 步骤

1. **预检** — 检查服务健康状态和发布窗口。
   - 工具：http_request

2. **部署** — 运行部署命令。
   - 工具：shell
   - 需要确认：true
```

解析器行为：

- 编号项（`1.`、`2.`、...）定义步骤顺序。
- 开头的粗体文本（`**标题**`）成为步骤标题。
- `- tools:` 映射到 `suggested_tools`。
- `- requires_confirmation: true` 强制该步骤需要审批。

## 4. 触发器类型

| 类型 | 字段 | 说明 |
|---|---|---|
| `manual` | 无 | 通过工具 `sop_execute` 触发（不是 `zeroclaw sop run` CLI 命令）。 |
| `webhook` | `path` | 与请求路径精确匹配（`/sop/...` 或 `/webhook`）。 |
| `mqtt` | `topic`，可选 `condition` | MQTT 主题支持 `+` 和 `#` 通配符。 |
| `cron` | `expression` | 支持 5、6 或 7 个字段（5 字段会在内部前置秒数）。 |
| `peripheral` | `board`、`signal`，可选 `condition` | 匹配 `\"{board}/{signal}\"`。 |

## 5. 条件语法

`condition` 评估为失败关闭（无效条件/payload => 不匹配）。

- JSON 路径比较：`$.value > 85`、`$.status == \"critical\"`
- 直接数值比较：`> 0`（适用于简单 payload）
- 运算符：`>=`、`<=`、`!=`、`>`、`<`、`==`

## 6. 验证

使用：

```bash
zeroclaw sop validate
zeroclaw sop validate <name>
```

验证会对空名称/描述、缺少触发器、缺少步骤和步骤编号间隙发出警告。
