# PR #9037 修复方案

## 问题总结

### Audacity88 第三轮评审 (2026-08-01) 提出的 3 个 Blocking 问题

#### Blocking 1: 堆叠标记碎片化处理不当

**问题描述**: 状态机只在下一块**以完整标记或标记前缀开头**时才识别第二个标记。对于碎片化的堆叠标记，第一个标记被错误地当作 inline 文本输出。

**失败示例**:
```
chunks: ["Summary<eom>", "<|", "eom|>"]
当前产出: "Summary<eom>"  ❌
期望产出: "Summary"       ✅
```

**根本原因**: `push()` 行 322-329 的逻辑缺陷：

```rust
let meaningful_pos = chunk.find(|c: char| !c.is_whitespace()).unwrap_or(chunk.len());

if meaningful_pos == 0 {
    // Meaningful text immediately (not starting with '<'): marker was inline
    visible.push_str(&old_pending);  // 错误：当 chunk="<|" 时，old_pending="<eom>" 被当作 inline
    input.push_str(chunk);
}
```

注释说 "not starting with '<'"，但 `chunk = "<|"` 明明以 `'<'` 开头！当 `old_pending = "<eom>"` (完整标记) 且 `chunk = "<|"` (第二个标记的前缀)时：
- `meaningful_pos = 0`（因为 `<` 不是 whitespace）
- 进入 "meaningful text immediately" 分支
- 错误地将第一个标记 `<eom>` 当作 inline 文本输出

**正确逻辑**: 当 `old_pending` 是完整标记时，`chunk` 以 `'<'` 开头的情况下，应该检查 `chunk` 是否是另一个标记的前缀，而不是直接判定第一个标记为 inline。

---

#### Blocking 2: 非流式 `strip_trailing_terminal_markers()` 的 10 字节限制

**问题描述**: 函数 `strip_trailing_terminal_markers()` 在 `zeroclaw-tool-call-parser/src/lib.rs:2194` 硬编码了最多检查 10 字节的 trailing whitespace。

**失败示例**:
```rust
strip_trailing_terminal_markers("Summary<eom>           <|eom|>")
// 如果空白超过 10 字节，留下 "Summary<eom>           "  ❌
// 期望产出: "Summary" ✅
```

**根本原因**: 
```rust
for ws_len in (1..=10.min(result.len())).rev() {
    // 只检查最多 10 字节的 whitespace
}
```

**正确逻辑**: 应该使用 `trim_end()` 迭代或 `strip_suffix()` 循环，没有任意字节限制。

---

#### Blocking 3: 堆叠 inline 文本在 EOF 前未释放

**问题描述**: 当完整标记后跟另一个标记再跟有意义文本时，状态机将整个后缀移入 `pending` 而不释放。

**失败示例**:
```
chunks: ["literal <eom><|eom|> in code"]
当前产出: "literal " (停在这里，直到 EOF) ❌
期望产出: "literal <eom><|eom|> in code" ✅
```

**根本原因**: `pending_is_terminal_marker_plus_whitespace()` 只检查 pending 是否**全是** marker+whitespace。当 pending 包含 marker 链后跟有意义文本时，无法识别应该释放。

**正确逻辑**: 应该扫描通过完整的 marker+whitespace 链，一旦检测到后面有有意义文本，立即将整个序列作为 inline 文本释放。

---

### IftekharUddin 评审提出的 4 个 Blocking 问题

#### Blocking 1: 非流式和 fallback 路径未标准化终端标记 ✅ 已修复

**状态**: 已在 `parse_response.rs:176` 添加调用

```rust
let response_text = strip_think_tags(resp.text_or_empty());
let response_text = zeroclaw_tool_call_parser::strip_trailing_terminal_markers(&response_text);
```

#### Blocking 2: 保持空白和堆叠标记后缀为终端 ✅ 已修复

**状态**: `stream_guard.rs` 的状态机已实现，新增测试覆盖

#### Blocking 3: 刷新 PR body 以匹配当前 head 和模板 ⚠️ 部分修复

**状态**: body 已部分更新，但仍与实际代码不完全匹配

#### Blocking 4: 从分支历史中移除个人身份数据 ❌ 未修复

**状态**: 当前 head 仍包含：
- commit 作者邮箱 `wang.miao86@xydigit.com`
- `2366ef1` 的 AI co-author trailer: `Co-Authored-By: nebulacoder-v8.0 <noreply@zte.com.cn>`

---

## 修复方案设计

### 修复 Blocking 1: 堆叠标记碎片化

**核心思路**: 当 `old_pending` 是完整标记时，对 `chunk` 的检查应该区分三种情况：
1. `chunk` 本身是完整标记或标记前缀 → 累积 pending
2. `chunk` 以 `'<'` 开头但不是标记前缀 → 检查是否是多字节字符后的 `<`
3. `chunk` 不以 `'<'` 开头 → meaningful text，第一个标记是 inline

**修复代码** (`stream_guard.rs:305-355`):

```rust
if old_pending_was_complete_marker {
    // Check if chunk starts with '<' - if not, it's meaningful text and the marker was inline
    if !chunk.starts_with('<') {
        visible.push_str(&old_pending);
        input.push_str(chunk);
    } else {
        // Chunk starts with '<' - check if it's a marker or marker prefix
        let chunk_is_complete_marker = Self::MARKERS.contains(&chunk);
        let chunk_is_marker_prefix = Self::MARKERS.iter().any(|m| m.starts_with(chunk));
        
        if chunk_is_complete_marker || chunk_is_marker_prefix {
            // Another marker (or prefix) arrives - accumulate
            self.pending.push_str(&old_pending);
            self.pending.push_str(chunk);
        } else {
            // Chunk starts with '<' but is not a marker prefix (e.g., "<foo>")
            // The first marker is inline
            visible.push_str(&old_pending);
            input.push_str(chunk);
        }
    }
}
```

**新增测试**:
```rust
#[test]
fn strips_stacked_markers_fragmented_across_chunks() {
    // Audacity88 round-3 Blocking 1
    assert_eq!(drain(&["Summary<eom>", "<|", "eom|>"]), "Summary");
    assert_eq!(drain(&["Summary<|", "eom", "|>", "extra"]), "Summary<|eom|>extra");
    assert_eq!(drain(&["<eom>", "<|", "eom|>"]), "");
}

#[test]
fn preserves_inline_stacked_markers_followed_by_text() {
    // Audacity88 round-3 Blocking 3
    assert_eq!(drain(&["literal <eom><|eom|> in code"]), "literal <eom><|eom|> in code");
    assert_eq!(drain(&["a", "<eom>", "<|eom|>", " b"]), "a<eom><|eom|> b");
}
```

---

### 修复 Blocking 2: 移除 10 字节限制

**修复代码** (`lib.rs:2176-2218`):

```rust
pub fn strip_trailing_terminal_markers(s: &str) -> String {
    const MARKERS: &[&str] = &["<|eom|>", "<eom>"];

    let mut result = s.to_string();

    loop {
        let mut stripped_any = false;

        for &marker in MARKERS {
            // Try stripping marker at end
            if let Some(stripped) = result.strip_suffix(marker) {
                result = stripped.to_string();
                stripped_any = true;
                break;
            }

            // Try stripping marker + trailing whitespace (no arbitrary limit)
            // Use trim_end to handle any amount of whitespace
            let trimmed = result.trim_end();
            if let Some(stripped) = trimmed.strip_suffix(marker) {
                // Check that we actually had whitespace
                if stripped.len() < result.len() - marker.len() {
                    result = stripped.to_string();
                    stripped_any = true;
                    break;
                }
            }
        }

        if !stripped_any {
            break;
        }
    }

    result
}
```

**新增测试**:
```rust
#[test]
fn strips_marker_followed_by_long_whitespace() {
    // Audacity88 round-3 Blocking 2: more than 10 bytes of whitespace
    let long_ws = "            "; // 12 spaces
    assert_eq!(strip_trailing_terminal_markers(&format!("Summary<eom>{}", long_ws)), "Summary");
    assert_eq!(strip_trailing_terminal_markers(&format!("Summary<eom>{}<|eom|>", long_ws)), "Summary");
}

#[test]
fn strips_stacked_markers_with_long_whitespace() {
    let long_ws = "              "; // 14 spaces
    assert_eq!(strip_trailing_terminal_markers(&format!("A<eom>{}<|eom|>", long_ws)), "A");
}
```

---

### 修复 Blocking 3: 释放堆叠 inline 文本

**核心思路**: `push()` 需要扫描通过 marker+whitespace 链，检测后面是否有有意义文本。

**修复代码** (`stream_guard.rs:391-420`):

```rust
// Case 2: `tail` starts with a complete marker but has more text after it
if let Some(marker) = Self::MARKERS
    .iter()
    .find(|m| tail.starts_with(**m))
    .copied()
{
    let after_marker = &tail[marker.len()..];
    
    // Scan through the chain of markers and whitespace
    let mut scan_pos = marker.len();
    let mut found_meaningful_text = false;
    
    while scan_pos < tail.len() {
        let remaining = &tail[scan_pos..];
        
        // Check if remaining starts with whitespace
        if let Some(ws_end) = remaining.find(|c: char| !c.is_whitespace()) {
            // There's whitespace followed by something
            let after_ws = &remaining[ws_end..];
            
            // Check if that something is another marker
            let next_marker = Self::MARKERS.iter().find(|m| after_ws.starts_with(**m));
            if let Some(next_marker) = next_marker {
                // Another marker follows - continue scanning
                scan_pos += ws_end + next_marker.len();
                continue;
            }
            
            // Meaningful non-marker text after whitespace - entire chain is inline
            found_meaningful_text = true;
            break;
        } else {
            // Only whitespace remains - terminal
            break;
        }
    }
    
    if found_meaningful_text {
        // Meaningful text after marker chain - inline
        visible.push_str(tail);
        input.clear();
    } else {
        // Only markers and whitespace - hold pending
        self.pending.clear();
        self.pending.push_str(tail);
    }
    return visible;
}
```

**新增测试**已在 Blocking 1 的测试中覆盖。

---

### 修复 Blocking 4: 个人身份数据

**步骤**:

1. 配置 git 使用 privacy-safe 身份：
```bash
git config user.email "123456789+wangmiao0668000666@users.noreply.github.com"
git config user.name "wangmiao0668000666"
```

2. 重写最后两个包含问题的 commits：
```bash
# 查看需要重写的 commits
git log --oneline -5

# 使用 interactive rebase 重写作者信息
git rebase -i 8d56a893a^  # 从 merge commit 之前开始

# 对每个需要重写的 commit 执行：
git commit --amend --reset-author --no-edit
git rebase --continue
```

3. 移除 AI co-author trailer：
```bash
# 编辑 commit message，删除 "Co-Authored-By: nebulacoder-v8.0 <noreply@zte.com.cn>"
git commit --amend -m "fix(runtime): strip terminal markers on streaming and non-streaming paths (#9006)

- Add strip_trailing_terminal_markers() to zeroclaw-tool-call-parser
  for non-streaming responses and stacked marker handling
- Wire StreamTerminalMarkerStripper in stream_consume.rs
- Enhance StreamTerminalMarkerStripper to handle stacked markers
  and marker+whitespace sequences
- Add 8 unit tests for strip_trailing_terminal_markers()
- Add 17 unit tests for StreamTerminalMarkerStripper
- Add 4 end-to-end tests in stream_consume.rs"
```

---

## 范围清理建议

### Audacity88 建议：重建为专注的 #9006 分支

**问题**: 当前 PR 包含无关的 #8733 modalities 工作，使评审复杂化。

**建议步骤**:

1. 从当前 master 创建新分支：
```bash
git fetch origin master
git checkout -b fix-9006-terminal-markers origin/master
```

2. 只移植 #9006 相关的变更：
```bash
# 从旧分支提取相关 commits
git cherry-pick <commit-with-stream_guard-changes>
git cherry-pick <commit-with-parse_response-changes>
git cherry-pick <commit-with-lib-changes>
```

3. 或者手动应用变更：
```bash
# 只复制 3 个核心文件
git checkout <old-branch> -- crates/zeroclaw-runtime/src/agent/turn/stream_guard.rs
git checkout <old-branch> -- crates/zeroclaw-runtime/src/agent/turn/parse_response.rs
git checkout <old-branch> -- crates/zeroclaw-tool-call-parser/src/lib.rs
```

4. 运行测试：
```bash
cargo test --locked -p zeroclaw-runtime --lib -- agent::turn::stream_guard::terminal_marker_stripper_tests
cargo test --locked -p zeroclaw-tool-call-parser --lib terminal_marker_strip_tests
```

---

## 实施顺序

1. **先修复 Blocking 2** (最简单) - 修改 `lib.rs:strip_trailing_terminal_markers()`
2. **再修复 Blocking 1 和 3** - 重写 `stream_guard.rs:push()` 的状态机逻辑
3. **添加新测试** - 覆盖 Audacity88 round-3 提出的失败场景
4. **清理范围** - 剥离 #8733 modalities 变更
5. **修复 commit metadata** - 重写包含个人身份的 commits
6. **更新 PR body** - 匹配新的精确 diff

---

## 验证清单

```bash
# 1. 流式路径测试
cargo test --locked -p zeroclaw-runtime --lib -- \
    agent::turn::stream_guard::terminal_marker_stripper_tests::strips_stacked_markers_fragmented_across_chunks \
    agent::turn::stream_guard::terminal_marker_stripper_tests::preserves_inline_stacked_markers_followed_by_text

# 2. 非流式路径测试
cargo test --locked -p zeroclaw-tool-call-parser --lib -- \
    terminal_marker_strip_tests::strips_marker_followed_by_long_whitespace

# 3. 集成测试
cargo test --locked -p zeroclaw-runtime --lib -- \
    agent::turn::stream_consume::tests::stream_consume_strips_terminal_eom_split_across_chunks

# 4. 完整验证
cargo fmt --all -- --check
cargo clippy --locked -p zeroclaw-runtime -p zeroclaw-tool-call-parser --all-targets -- -D warnings
cargo test --locked -p zeroclaw-runtime -p zeroclaw-tool-call-parser
```

---

## 风险与回滚

**风险**: 
- 状态机逻辑修改可能引入新的边界情况 bug
- 多字节 UTF-8 文本的处理需要额外测试

**回滚**:
```bash
# 如果合并后发现问题，revert 整个 PR
git revert <merge-commit-hash>
git push origin fix/9006-terminal-markers
```

**Mitigation**: 
- 新增的 10+ 个测试覆盖 Audacity88 提出的所有失败场景
- 现有的 28 个单元测试 + 4 个集成测试作为回归防护
