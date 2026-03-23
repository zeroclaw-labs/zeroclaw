# ZeroTrading Skills 知识库

> 智能体的量化决策基石。所有 `.md` 文件会被 ZeroTrading 引擎自动加载并注入 LLM 提示词。

## 目录结构

```
skills/
├── 风控/    🛡️  Risk Control  — 最高优先级，硬约束
├── 因子/    📊  Factor        — 宏观/微观信号源
├── 策略/    📈  Strategy      — 具体交易模型
└── 经验/    🧠  Experience    — 操盘哲学与认知
```

## 加载顺序（注入优先级）

1. **🛡️ 风控** → 硬约束，不可被策略覆盖
2. **📊 因子** → 环境感知，为策略提供上下文
3. **📈 策略** → 具体操作模型
4. **🧠 经验** → 认知补充，影响推理风格

## 如何新增 Skill

1. 在对应目录下创建 `.md` 文件
2. 命名建议: `{资产或主题}_{策略类型}.md`（全小写/中文均可）
3. 文件**自动被加载**，无需改代码，重启或热重载即生效

### 热重载（无需重启）

```bash
# 通过 zeroclaw API 触发热重载（如已实现 /api/trading/reload）
curl -X POST http://localhost:PORT/api/trading/reload

# 或直接重启 gateway
zeroclaw gateway restart
```

## Skill 文件模板

```markdown
# 技能名称

## 核心目标
一句话描述该技能的作用。

## 监控指标
- 指标1: 阈值 → 含义
- 指标2: 阈值 → 含义

## 触发条件
- 条件组合1 → 动作
- 条件组合2 → 动作

## 输出映射
- 信号场景描述 → `{"decision": "BUY/SELL/CLOSE/HOLD", "size": 0.05}`

## 风险约束
- 仓位/止损/条件限制说明
```

## 标准决策 JSON 格式

```json
{
  "decision": "BUY",       // BUY | SELL | CLOSE | HOLD | REQUEST_APPROVAL
  "size": 0.05,            // 0.0 ~ 1.0，占账户净值百分比
  "price": null,           // 可选：限价价格
  "stop_loss": null,       // 可选：止损价格
  "take_profit": null,     // 可选：止盈价格
  "reason": "signal_name"  // 可选：决策理由（用于审计）
}
```
