# ZeroClaw 文档结构地图

本页面从三个维度定义文档结构：

1. 语言
2. 部分（分类）
3. 功能（文档意图）

最后更新时间：**2026 年 2 月 22 日**。

## 1) 按语言分类

| 语言 | 入口点 | 规范目录树 | 说明 |
|---|---|---|---|
| 英文 | `docs/README.md` | `docs/` | 运行时行为的权威文档首先以英文编写。 |
| 中文（`zh-CN`） | `docs/README.zh-CN.md` | `docs/` 本地化中心 + 精选本地化文档 | 使用本地化中心和共享分类结构。 |
| 日文（`ja`） | `docs/README.ja.md` | `docs/` 本地化中心 + 精选本地化文档 | 使用本地化中心和共享分类结构。 |
| 俄文（`ru`） | `docs/README.ru.md` | `docs/` 本地化中心 + 精选本地化文档 | 使用本地化中心和共享分类结构。 |
| 法文（`fr`） | `docs/README.fr.md` | `docs/` 本地化中心 + 精选本地化文档 | 使用本地化中心和共享分类结构。 |
| 越南文（`vi`） | `docs/i18n/vi/README.md` | `docs/i18n/vi/` | 完整越南文目录树的规范路径位于 `docs/i18n/vi/` 下；`docs/vi/` 和 `docs/*.vi.md` 是兼容性路径。 |

## 2) 按部分（分类）分类

这些目录是按产品领域划分的主要导航模块。

- `docs/getting-started/`：初始安装和首次运行流程
- `docs/reference/`：命令/配置/提供商/渠道参考索引
- `docs/operations/`：Day-2 运维、部署和故障排除入口
- `docs/security/`：安全指南和面向安全的导航
- `docs/hardware/`：开发板/外设实现和硬件工作流
- `docs/contributing/`：贡献指南和 CI/评审流程
- `docs/project/`：项目快照、规划上下文和状态相关文档

## 3) 按功能（文档意图）分类

使用此分组来决定新文档的存放位置。

### 运行时契约（当前行为）

- `docs/commands-reference.md`
- `docs/providers-reference.md`
- `docs/channels-reference.md`
- `docs/config-reference.md`
- `docs/operations-runbook.md`
- `docs/troubleshooting.md`
- `docs/one-click-bootstrap.md`

### 安装 / 集成指南

- `docs/custom-providers.md`
- `docs/zai-glm-setup.md`
- `docs/langgraph-integration.md`
- `docs/network-deployment.md`
- `docs/matrix-e2ee-guide.md`
- `docs/mattermost-setup.md`
- `docs/nextcloud-talk-setup.md`

### 政策 / 流程

- `docs/pr-workflow.md`
- `docs/reviewer-playbook.md`
- `docs/ci-map.md`
- `docs/actions-source-policy.md`

### 提案 / 路线图

- `docs/sandboxing.md`
- `docs/resource-limits.md`
- `docs/audit-logging.md`
- `docs/agnostic-security.md`
- `docs/frictionless-security.md`
- `docs/security-roadmap.md`

### 快照 / 时间限制报告

- `docs/project-triage-snapshot-2026-02-18.md`

### 资产 / 模板

- `docs/datasheets/`
- `docs/doc-template.md`

## 放置规则（快速参考）

- 新的运行时行为文档必须链接到相应的分类索引和 `docs/SUMMARY.md`。
- 导航变更必须在 `docs/README*.md` 和 `docs/SUMMARY*.md` 之间保持语言区域 parity。
- 越南文完整本地化内容位于 `docs/i18n/vi/`；兼容性文件应指向规范路径。
