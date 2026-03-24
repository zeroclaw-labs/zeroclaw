# ZeroClaw 文档清单

本清单按意图对文档进行分类，以便读者快速区分运行时契约指南与设计提案。

最后审核时间：**2026 年 2 月 18 日**。

## 分类说明

- **当前指南/参考：** 旨在匹配当前运行时行为
- **政策/流程：** 协作或治理规则
- **提案/路线图：** 设计探索；可能包含假设的命令
- **快照：** 有时间限制的运营报告

## 文档入口点

| 文档 | 类型 | 受众 |
|---|---|---|
| `README.md` | 当前指南 | 所有读者 |
| `README.zh-CN.md` | 当前指南（本地化） | 中文读者 |
| `README.ja.md` | 当前指南（本地化） | 日文读者 |
| `README.ru.md` | 当前指南（本地化） | 俄文读者 |
| `README.vi.md` | 当前指南（本地化） | 越南文读者 |
| `docs/README.md` | 当前指南（中心） | 所有读者 |
| `docs/README.zh-CN.md` | 当前指南（本地化中心） | 中文读者 |
| `docs/README.ja.md` | 当前指南（本地化中心） | 日文读者 |
| `docs/README.ru.md` | 当前指南（本地化中心） | 俄文读者 |
| `docs/README.vi.md` | 当前指南（本地化中心） | 越南文读者 |
| `docs/SUMMARY.md` | 当前指南（统一目录） | 所有读者 |
| `docs/structure/README.md` | 当前指南（结构地图） | 所有读者 |

## 分类索引文档

| 文档 | 类型 | 受众 |
|---|---|---|
| `docs/getting-started/README.md` | 当前指南 | 新用户 |
| `docs/reference/README.md` | 当前指南 | 用户/运维人员 |
| `docs/operations/README.md` | 当前指南 | 运维人员 |
| `docs/security/README.md` | 当前指南 | 运维人员/贡献者 |
| `docs/hardware/README.md` | 当前指南 | 硬件开发者 |
| `docs/contributing/README.md` | 当前指南 | 贡献者/评审者 |
| `docs/project/README.md` | 当前指南 | 维护者 |

## 当前指南与参考

| 文档 | 类型 | 受众 |
|---|---|---|
| `docs/one-click-bootstrap.md` | 当前指南 | 用户/运维人员 |
| `docs/commands-reference.md` | 当前参考 | 用户/运维人员 |
| `docs/providers-reference.md` | 当前参考 | 用户/运维人员 |
| `docs/channels-reference.md` | 当前参考 | 用户/运维人员 |
| `docs/nextcloud-talk-setup.md` | 当前指南 | 运维人员 |
| `docs/config-reference.md` | 当前参考 | 运维人员 |
| `docs/custom-providers.md` | 当前集成指南 | 集成开发者 |
| `docs/zai-glm-setup.md` | 当前提供商设置指南 | 用户/运维人员 |
| `docs/langgraph-integration.md` | 当前集成指南 | 集成开发者 |
| `docs/operations-runbook.md` | 当前指南 | 运维人员 |
| `docs/troubleshooting.md` | 当前指南 | 用户/运维人员 |
| `docs/network-deployment.md` | 当前指南 | 运维人员 |
| `docs/mattermost-setup.md` | 当前指南 | 运维人员 |
| `docs/adding-boards-and-tools.md` | 当前指南 | 硬件开发者 |
| `docs/arduino-uno-q-setup.md` | 当前指南 | 硬件开发者 |
| `docs/nucleo-setup.md` | 当前指南 | 硬件开发者 |
| `docs/hardware-peripherals-design.md` | 当前设计规范 | 硬件贡献者 |
| `docs/datasheets/nucleo-f401re.md` | 当前硬件参考 | 硬件开发者 |
| `docs/datasheets/arduino-uno.md` | 当前硬件参考 | 硬件开发者 |
| `docs/datasheets/esp32.md` | 当前硬件参考 | 硬件开发者 |

## 政策/流程文档

| 文档 | 类型 |
|---|---|
| `docs/pr-workflow.md` | 政策 |
| `docs/reviewer-playbook.md` | 流程 |
| `docs/ci-map.md` | 流程 |
| `docs/actions-source-policy.md` | 政策 |

## 提案/路线图文档

这些是有价值的上下文，但**不是严格的运行时契约**。

| 文档 | 类型 |
|---|---|
| `docs/sandboxing.md` | 提案 |
| `docs/resource-limits.md` | 提案 |
| `docs/audit-logging.md` | 提案 |
| `docs/agnostic-security.md` | 提案 |
| `docs/frictionless-security.md` | 提案 |
| `docs/security-roadmap.md` | 路线图 |

## 快照文档

| 文档 | 类型 |
|---|---|
| `docs/project-triage-snapshot-2026-02-18.md` | 快照 |

## 维护建议

1. CLI 表面变更时更新 `commands-reference`。
2. 提供商目录/别名/环境变量变更时更新 `providers-reference`。
3. 渠道支持或白名单语义变更时更新 `channels-reference`。
4. 保持快照带日期戳且不可变。
5. 清晰标记提案文档，避免被误认为运行时契约。
6. 添加新的核心文档时，保持本地化 README/文档中心链接对齐。
7. 添加新的主要文档时，更新 `docs/SUMMARY.md` 和分类索引。
