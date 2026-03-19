# JhedaiClaw 文档导航（简体中文）

这是文档系统的中文入口页。

最后对齐：**2026-03-14**。

> 说明：命令、配置键、API 路径保持英文；实现细节以英文文档为准。

## 快速入口

| 我想要…                        | 建议阅读                                                                                                    |
| ------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| 快速安装并运行                 | [../README.zh-CN.md](../README.zh-CN.md) / [../README.md](../README.md)                                     |
| macOS 平台更新与卸载           | [macos-update-uninstall.md](i18n/zh-CN/setup-guides/macos-update-uninstall.zh-CN.md)                        |
| 一键安装与初始化               | [one-click-bootstrap.md](i18n/zh-CN/setup-guides/one-click-bootstrap.zh-CN.md)                              |
| 按任务找命令                   | [commands-reference.md](i18n/zh-CN/reference/cli/commands-reference.zh-CN.md)                               |
| 快速查看配置默认值与关键项     | [config-reference.md](i18n/zh-CN/reference/api/config-reference.zh-CN.md)                                   |
| 接入自定义 Provider / endpoint | [custom-providers.md](i18n/zh-CN/contributing/custom-providers.zh-CN.md)                                    |
| 配置 Z.AI / GLM Provider       | [zai-glm-setup.md](i18n/zh-CN/setup-guides/zai-glm-setup.zh-CN.md)                                          |
| 使用 LangGraph 工具调用集成    | [langgraph-integration.md](i18n/zh-CN/contributing/langgraph-integration.zh-CN.md)                          |
| 进行日常运维（runbook）        | [operations-runbook.md](i18n/zh-CN/ops/operations-runbook.zh-CN.md)                                         |
| 快速排查安装/运行/通道问题     | [troubleshooting.md](i18n/zh-CN/ops/troubleshooting.zh-CN.md)                                               |
| Matrix 加密房间配置与诊断      | [matrix-e2ee-guide.md](i18n/zh-CN/security/matrix-e2ee-guide.zh-CN.md)                                      |
| 统一目录导航                   | [SUMMARY.md](SUMMARY.md)                                                                                    |
| 查看 PR/Issue 扫描快照         | [project-triage-snapshot-2026-02-18.md](i18n/zh-CN/maintainers/project-triage-snapshot-2026-02-18.zh-CN.md) |

## 10 秒决策树（先看这个）

- 首次安装或快速启动 → [setup-guides/README.md](i18n/zh-CN/setup-guides/README.zh-CN.md)
- 需要精确命令或配置键 → [reference/README.md](i18n/zh-CN/reference/README.zh-CN.md)
- 需要部署与服务化运维 → [ops/README.md](i18n/zh-CN/ops/README.zh-CN.md)
- 遇到报错、异常或回归 → [troubleshooting.md](i18n/zh-CN/ops/troubleshooting.zh-CN.md)
- 查看安全现状与路线图 → [security/README.md](i18n/zh-CN/security/README.zh-CN.md)
- 接入板卡与外设 → [hardware/README.md](i18n/zh-CN/hardware/README.zh-CN.md)
- 参与贡献、评审与 CI → [contributing/README.md](i18n/zh-CN/contributing/README.zh-CN.md)
- 查看完整文档地图 → [SUMMARY.md](SUMMARY.md)

## 按目录浏览（推荐）

- 入门文档： [setup-guides/README.md](i18n/zh-CN/setup-guides/README.zh-CN.md)
- 参考手册： [reference/README.md](i18n/zh-CN/reference/README.zh-CN.md)
- 运维与部署： [ops/README.md](i18n/zh-CN/ops/README.zh-CN.md)
- 安全文档： [security/README.md](i18n/zh-CN/security/README.zh-CN.md)
- 硬件与外设： [hardware/README.md](i18n/zh-CN/hardware/README.zh-CN.md)
- 贡献与 CI： [contributing/README.md](i18n/zh-CN/contributing/README.zh-CN.md)
- 项目快照： [maintainers/README.md](i18n/zh-CN/maintainers/README.zh-CN.md)

## 按角色

### 用户 / 运维

- [commands-reference.md](i18n/zh-CN/reference/cli/commands-reference.zh-CN.md) — 按工作流查询命令
- [providers-reference.md](i18n/zh-CN/reference/api/providers-reference.zh-CN.md) — Provider ID、别名、凭证环境变量
- [channels-reference.md](i18n/zh-CN/reference/api/channels-reference.zh-CN.md) — 通道功能与配置路径
- [matrix-e2ee-guide.md](i18n/zh-CN/security/matrix-e2ee-guide.zh-CN.md) — Matrix 加密房间（E2EE）配置与无响应诊断
- [config-reference.md](i18n/zh-CN/reference/api/config-reference.zh-CN.md) — 高优先级配置项与安全默认值
- [custom-providers.md](i18n/zh-CN/contributing/custom-providers.zh-CN.md) — 自定义 Provider/基础 URL 集成模板
- [zai-glm-setup.md](i18n/zh-CN/setup-guides/zai-glm-setup.zh-CN.md) — Z.AI/GLM 配置与端点矩阵
- [langgraph-integration.md](i18n/zh-CN/contributing/langgraph-integration.zh-CN.md) — 模型/工具调用边缘场景的降级集成方案
- [operations-runbook.md](i18n/zh-CN/ops/operations-runbook.zh-CN.md) — 日常运行时运维与回滚流程
- [troubleshooting.md](i18n/zh-CN/ops/troubleshooting.zh-CN.md) — 常见故障特征与恢复步骤

### 贡献者 / 维护者

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](i18n/zh-CN/contributing/pr-workflow.zh-CN.md)
- [reviewer-playbook.md](i18n/zh-CN/contributing/reviewer-playbook.zh-CN.md)
- [ci-map.md](i18n/zh-CN/contributing/ci-map.zh-CN.md)
- [actions-source-policy.md](i18n/zh-CN/contributing/actions-source-policy.zh-CN.md)

### 安全 / 稳定性

> 说明：本分组内有 proposal/roadmap 文档，可能包含设想中的命令或配置。当前可执行行为请优先阅读 [config-reference.md](i18n/zh-CN/reference/api/config-reference.md)、[operations-runbook.md](i18n/zh-CN/ops/operations-runbook.md)、[troubleshooting.md](i18n/zh-CN/ops/troubleshooting.zh-CN.md)。

- [security/README.md](i18n/zh-CN/security/README.zh-CN.md)
- [agnostic-security.md](i18n/zh-CN/security/agnostic-security.zh-CN.md)
- [frictionless-security.md](i18n/zh-CN/security/frictionless-security.zh-CN.md)
- [sandboxing.md](i18n/zh-CN/security/sandboxing.zh-CN.md)
- [resource-limits.md](i18n/zh-CN/ops/resource-limits.zh-CN.md)
- [audit-logging.md](i18n/zh-CN/security/audit-logging.zh-CN.md)
- [security-roadmap.md](i18n/zh-CN/security/security-roadmap.zh-CN.md)

## 文档治理与分类

- 统一目录（TOC）：[SUMMARY.md](SUMMARY.md)
- 文档结构图（按语言/分区/功能）：[structure/README.md](i18n/zh-CN/maintainers/structure-README.zh-CN.md)
- 文档清单与分类：[docs-inventory.md](i18n/zh-CN/maintainers/docs-inventory.zh-CN.md)
- 国际化文档索引：[i18n/README.md](i18n/README.md)
- 国际化覆盖度地图：[i18n-coverage.md](i18n/zh-CN/maintainers/i18n-coverage.zh-CN.md)
- 项目分诊快照：[project-triage-snapshot-2026-02-18.md](i18n/zh-CN/maintainers/project-triage-snapshot-2026-02-18.zh-CN.md)

## 其他语言

- English: [README.md](README.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
