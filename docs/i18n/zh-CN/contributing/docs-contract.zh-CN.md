# 文档系统契约

将文档视为一等产品表面，而非合并后的附属产物。

## 规范入口点

- 根目录 README：`README.md`、`README.zh-CN.md`、`README.ja.md`、`README.ru.md`、`README.fr.md`、`README.vi.md`
- 文档中心：`docs/README.md`、`docs/README.zh-CN.md`、`docs/README.ja.md`、`docs/README.ru.md`、`docs/README.fr.md`、`docs/README.vi.md`
- 统一目录：`docs/SUMMARY.md`

## 支持的语言

`en`、`zh-CN`、`ja`、`ru`、`fr`、`vi`

## 分类索引

- `docs/setup-guides/README.md`
- `docs/reference/README.md`
- `docs/ops/README.md`
- `docs/security/README.md`
- `docs/hardware/README.md`
- `docs/contributing/README.md`
- `docs/maintainers/README.md`

## 治理规则

- 保持 README/文档中心的顶部导航和快速路径直观且不重复。
- 更改导航架构时，保持所有支持语言的入口点一致性。
- 如果变更涉及文档 IA（信息架构）、运行时契约参考或共享文档中的用户-facing 措辞，在同一个 PR 中完成支持语言的国际化（i18n）跟进：
  - 更新语言导航链接（`README*`、`docs/README*`、`docs/SUMMARY.md`）。
  - 更新存在对应版本的本地化运行时契约文档。
  - 对于越南语，将 `docs/vi/**` 视为权威版本。
- 提案/路线图文档要显式标记；避免将提案文本混入运行时契约文档。
- 项目快照要标注日期，被更新日期的版本取代后保持不可变。
