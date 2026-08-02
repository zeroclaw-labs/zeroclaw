---
name: PR提交前必须自检
description: 用户在提交PR推送之前，必须做好自查，确保分支干净、同步了主分支最新内容、修复内容完整
type: feedback
---

PR提交前必须自检

**Why:** 用户在PR #8874的修复过程中发现，分支可能包含大量无关的合并提交（如通过`git merge master`引入的数十个额外提交），导致分支不干净、PR审查困难。经过多次返工后才创建了真正干净的分支。

**How to apply:** 每次执行`git push`推送PR之前，必须运行以下自检命令：

1. **检查分支是否干净**（只包含与PR相关的提交）：
   ```bash
   git log --oneline master..HEAD  # 应该只显示本PR的提交，不应有无关的merge提交
   git diff master..HEAD --name-only  # 应该只显示本PR修改的文件
   ```

2. **检查分支是否同步了主分支最新内容**：
   ```bash
   git log --oneline -3  # 确认HEAD是基于最新的master
   ```

3. **检查修复内容是否完整**：
   ```bash
   git diff --stat  # 确认修改的文件和行数符合预期
   git status  # 确认工作区干净，没有未提交的修改
   ```

4. **确认测试通过**：
   ```bash
   cargo test --locked -p <affected-crate> --lib  # 运行相关测试
   ```

**期望的干净分支示例**：
```
$ git log --oneline master..HEAD
71c493742 fix(ci): scope rustdoc --default-theme away from cargo test --doc (#8847)

$ git diff master..HEAD --name-only
.cargo/config.toml
xtask/src/cmd/mdbook/refs.rs
```

**不干净的分支示例**（应避免）：
```
$ git log --oneline master..HEAD | wc -l
95  # 包含了大量无关的merge提交
```
