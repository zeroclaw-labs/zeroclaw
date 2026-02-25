# Security Advisory 维护者检查清单（中文）

本清单用于高影响安全漏洞从私密受理到公开披露的全流程执行。

## A）私密受理与分诊

- [ ] 确认该报告在私密 advisory 流程中（不是公开 issue）。
- [ ] 验证可复现性与影响。
- [ ] 基于 `SECURITY.md` 的 S0-S3 矩阵完成级别判定。
- [ ] 记录级别判定依据与 SLA 计时起点。
- [ ] 选择处理路径：
  - [ ] 接受并转为 advisory 草稿
  - [ ] 请求补充信息
  - [ ] 判定为非安全问题并给出理由后关闭
- [ ] 在 advisory 评论中记录 owner、里程碑和时间线。

## B）封版期修复开发

- [ ] 启动或使用 advisory 的 temporary private fork。
- [ ] 在公开 PR/Issue 讨论中避免披露可利用细节。
- [ ] 实现最小风险修复，并补充回归测试。
- [ ] 本地完成必要验证：
  - [ ] `cargo test --workspace --all-targets`
  - [ ] `cargo test -- security`
  - [ ] `cargo test -- tools::shell`
  - [ ] `cargo test -- tools::file_read`
  - [ ] `cargo test -- tools::file_write`
- [ ] 如需支持历史版本，准备 backport。

## C）Advisory 元数据质量

- [ ] 使用元数据模板：`docs/security/advisory-metadata-template.zh-CN.md`。
- [ ] 受影响 package/ecosystem 字段正确。
- [ ] 受影响版本范围精确。
- [ ] 已填写修复版本；若暂无修复版本则提供缓解方案。
- [ ] 尽可能补齐 CWE/CVSS 字段。
- [ ] 引用修复提交、发布说明或关联文档。

## D）披露与披露后维护

- [ ] 修复或缓解准备就绪后发布 advisory。
- [ ] 合适时申请 CVE（或关联已有 CVE）。
- [ ] 校验已发布 advisory 是否引用了可获取的修复产物。
- [ ] 确认下游通知/依赖安全信号一致。
- [ ] 若后续发现影响面变化，及时更新 advisory 元数据。

## E）内部信息卫生

- [ ] 提交记录、日志、CI 输出、讨论线程中不包含密钥。
- [ ] 披露前不在公开渠道给出不必要的利用细节。
- [ ] 在 advisory 评论中保留分诊决策和响应时间线。
