# Provider 参考（简体中文）

这是 Wave 1 首版本地化页面，用于快速查阅 provider 标识、别名与认证变量。

英文原文：

- [../../providers-reference.md](../../providers-reference.md)

## 适用场景

- 选择 provider 与模型接入路径
- 核对 provider ID / alias / 环境变量
- 处理 provider 配置错误与鉴权问题

## 使用建议

- Provider ID 与环境变量名称保持英文。
- 规范与行为说明以英文原文为准。

## 更新记录

- 2026-03-01：新增 StepFun provider 对齐信息（`stepfun` / `step` / `step-ai` / `step_ai`）。

## StepFun 快速说明

- Provider ID：`stepfun`
- 别名：`step`、`step-ai`、`step_ai`
- Base API URL：`https://api.stepfun.com/v1`
- 模型列表端点：`GET /v1/models`
- 对话端点：`POST /v1/chat/completions`
- 鉴权变量：`STEP_API_KEY`（回退：`STEPFUN_API_KEY`）
- 默认模型：`step-3.5-flash`

快速验证：

```bash
export STEP_API_KEY="your-stepfun-api-key"
zeroclaw models refresh --provider stepfun
zeroclaw agent --provider stepfun --model step-3.5-flash -m "ping"
```
