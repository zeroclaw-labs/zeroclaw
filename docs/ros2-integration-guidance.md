# ROS2 Integration Guidance

This note captures the recommended integration shape for ROS2/ROS1 environments.
It is intentionally architecture-focused and keeps ZeroClaw core boundaries stable.

## Recommendation

Use the plugin/adapter route first.

- Keep robotics transport in an integration crate or module that bridges ROS topics/services/actions to ZeroClaw tools/channels/runtime adapters.
- Keep high-frequency control loops in ROS-native execution contexts.
- Use ZeroClaw for planning, orchestration, policy, and guarded action dispatch.

Deep core coupling should be a last resort and only justified by measured latency limits that cannot be met with a bridge.

## Why This Is The Default

- Upgrade safety: trait-based adapters survive upstream changes better than core patches.
- Blast-radius control: transport details stay outside security/runtime core modules.
- Reproducibility: integration behavior is easier to test and rollback when isolated.
- Security posture: approval, policy, and gating remain centralized in existing ZeroClaw paths.

## Real-Time Boundary Rule

Do not route hard real-time motor/safety loops through LLM turn latency.

- ROS node graph handles tight-loop control and watchdogs.
- ZeroClaw emits intent-level commands and receives summarized state.
- Safety-critical stop paths stay local to robot runtime regardless of agent health.

## Suggested Baseline Architecture

1. ROS2 bridge node subscribes to high-rate sensor topics.
2. Bridge performs local reduction/windowing and forwards compact summaries to ZeroClaw.
3. ZeroClaw decides intent/tool calls under existing policy and approval constraints.
4. Bridge translates approved intents into ROS commands with bounded command-rate limits.
5. Telemetry and fault states flow back into ZeroClaw for reasoning and auditability.

## Escalation Criteria For Core Integration

Consider deeper ZeroClaw runtime integration only when all are true:

- Measured bridge overhead is a validated bottleneck under production-like load.
- Required latency/jitter budgets are written and reproducible.
- The proposed core change has clear rollback and subsystem ownership.
- Security and policy guarantees remain equivalent or stronger.

If those conditions are not met, stay with adapter/plugin integration.
