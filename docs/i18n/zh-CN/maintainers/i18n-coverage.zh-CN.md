# ZeroClaw 国际化（i18n）覆盖率和结构

本文档定义了 ZeroClaw 文档的本地化结构，并跟踪当前覆盖率。

最后更新时间：**2026 年 2 月 21 日**。

## 规范布局

使用以下国际化路径：

- 根语言着陆页：`README.<语言区域>.md`
- 完整本地化文档树：`docs/i18n/<语言区域>/...`
- 可选的兼容性垫片位于 docs 根目录：
  - `docs/README.<语言区域>.md`
  - `docs/commands-reference.<语言区域>.md`
  - `docs/config-reference.<语言区域>.md`
  - `docs/troubleshooting.<语言区域>.md`

## 语言区域覆盖率矩阵

| 语言区域 | 根 README | 规范文档中心 | 命令参考 | 配置参考 | 故障排除 | 状态 |
|---|---|---|---|---|---|---|
| `en` | `README.md` | `docs/README.md` | `docs/commands-reference.md` | `docs/config-reference.md` | `docs/troubleshooting.md` | 权威来源 |
| `zh-CN` | `README.zh-CN.md` | `docs/README.zh-CN.md` | - | - | - | 中心级本地化 |
| `ja` | `README.ja.md` | `docs/README.ja.md` | - | - | - | 中心级本地化 |
| `ru` | `README.ru.md` | `docs/README.ru.md` | - | - | - | 中心级本地化 |
| `fr` | `README.fr.md` | `docs/README.fr.md` | - | - | - | 中心级本地化 |
| `vi` | `README.vi.md` | `docs/i18n/vi/README.md` | `docs/i18n/vi/commands-reference.md` | `docs/i18n/vi/config-reference.md` | `docs/i18n/vi/troubleshooting.md` | 完整树本地化 |

## 根 README 完整性

并非所有根 README 都是 `README.md` 的完整翻译：

| 语言区域 | 风格 | 近似覆盖率 |
|---|---|---|
| `en` | 完整来源 | 100% |
| `zh-CN` | 中心式入口点 | ~26% |
| `ja` | 中心式入口点 | ~26% |
| `ru` | 中心式入口点 | ~26% |
| `fr` | 接近完整翻译 | ~90% |
| `vi` | 接近完整翻译 | ~90% |

中心式入口点提供快速入门指南和语言导航，但不复制完整的英文 README 内容。这是准确的状态记录，而非需要立即解决的缺口。

## 分类索引国际化

分类目录（`docs/getting-started/`、`docs/reference/`、`docs/operations/`、`docs/security/`、`docs/hardware/`、`docs/contributing/`、`docs/project/`）下的本地化 `README.md` 文件目前仅存在英文和越南文版本。其他语言的分类索引本地化将延后处理。

## 本地化规则

- 技术标识符保持英文：
  - CLI 命令名称
  - 配置键
  - API 路径
  - 特征/类型标识符
- 优先使用简洁的、面向运维的本地化，而非逐字翻译。
- 本地化页面变更时更新"最后更新" / "最后同步"日期。
- 确保每个本地化中心都有"其他语言"部分。

## 添加新的语言区域

1. 创建 `README.<语言区域>.md`。
2. 在 `docs/i18n/<语言区域>/` 下创建规范文档树（至少包含 `README.md`、`commands-reference.md`、`config-reference.md`、`troubleshooting.md`）。
3. 添加语言区域链接到：
   - 每个 `README*.md` 的根语言导航
   - `docs/README.md` 中的本地化中心列表
   - 每个 `docs/README*.md` 的"其他语言"部分
   - `docs/SUMMARY.md` 中的语言入口部分
4. 可选地添加 docs 根目录垫片文件以保持向后兼容性。
5. 更新此文件（`docs/i18n-coverage.md`）并运行链接验证。

## 评审检查清单

- 所有本地化入口文件的链接可解析。
- 没有语言区域引用过时的文件名（例如 `README.vn.md`）。
- 目录（`docs/SUMMARY.md`）和文档中心（`docs/README.md`）包含该语言区域。
