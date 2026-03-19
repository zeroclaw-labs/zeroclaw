# SOP 可观测性与审计

本页面介绍 SOP 执行证据的存储位置以及如何检查它。

## 1. 审计持久化

SOP 审计条目通过 `SopAuditLogger` 持久化到配置的内存后端的 `sop` 类别下。

常见键模式：

- `sop_run_{run_id}`：运行快照（启动 + 完成更新）
- `sop_step_{run_id}_{step_number}`：单步结果
- `sop_approval_{run_id}_{step_number}`：操作员审批记录
- `sop_timeout_approve_{run_id}_{step_number}`：超时自动审批记录
- `sop_gate_decision_{gate_id}_{timestamp_ms}`：门评估器决策记录（启用 `ampersona-gates` 时）
- `sop_phase_state`：持久化的信任阶段状态快照（启用 `ampersona-gates` 时）

## 2. 检查路径

### 2.1 定义级 CLI

```bash
jhedaiclaw sop list
jhedaiclaw sop validate [name]
jhedaiclaw sop show <name>
```

### 2.2 运行时运行状态工具

SOP 运行状态通过代理内工具查询：

- `sop_status` — 活动/已完成运行和可选指标
- 带 `include_gate_status: true` 的 `sop_status` — 信任阶段和门评估器状态（如果可用）
- `sop_approve` — 批准等待的运行步骤
- `sop_advance` — 提交步骤结果并推进运行

## 3. 指标

- 当 `[observability] backend = \"prometheus\"` 时，`/metrics` 暴露观察者指标。
- 当前导出的名称是 `jhedaiclaw_*` 系列（通用运行时指标）。
- SOP 特定的聚合可通过带 `include_metrics: true` 的 `sop_status` 获取。
