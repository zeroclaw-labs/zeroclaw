# PR #9040 修复计划

## 📋 问题总结

### IftekharUddin的评审（3个Blocking问题）

#### 1. PR body不符合当前模板
**问题**：
- 风险声明不匹配（实际是high-risk，但body说是low-risk）
- 缺少Security & Privacy Impact的明确回答（5个Yes/No问题）
- 缺少Compatibility的当前格式（3个Yes/No问题）
- 缺少Rollback记录（fast rollback命令、feature flags、observable failure symptoms）
- Commit包含禁止的`Co-Authored-By: Claude`脚注

**需要做什么**：
- 重写PR body，使用当前的required template
- 更新Risk声明为high-risk
- 添加Security & Privacy Impact部分，回答所有5个问题
- 添加Compatibility部分，回答所有3个问题
- 添加Rollback部分，包含fast rollback命令和observable failure symptoms
- 检查commit history，移除所有bot/AI attribution脚注

#### 2. 缺少Fluent本地化
**问题**：
- 新的echo消息没有使用Fluent catalogue
- 当前实现可能使用了硬编码的英文字符串

**需要做什么**：
- 检查`echo_daemon_started_to_terminal`中使用的字符串
- 确保所有operator-facing输出都通过`cli_t()`或`cli_ta()`调用Fluent key
- 添加所有必要的Fluent key到`locales/*/cli.ftl`

#### 3. 测试不完整
**问题**：
- 需要证明ordered `Starting` then `Ready`生命周期
- 当前的测试只证明了等待和最终状态，没有证明两个独立的消息

**需要做什么**：
- 添加测试证明：先emit Starting，然后独立等待，然后emit Ready
- 测试应该证明即使超过15秒，最终也会emit Ready

### Audacity88的评审（2个Blocking问题）

#### 1. 生命周期消息不完整
**问题**：
- 当前实现只emit一次消息（要么Starting要么Ready）
- 应该先立即emit Starting消息
- 然后独立等待endpoint ready
- 当endpoint ready时再emit Ready消息
- 15秒超时不应该阻止后续的Ready确认
- 同步等待延迟了进入daemon正常shutdown-signal handling路径

**需要做什么**：
**重大重构**：
1. 在`daemon::run`中：
   ```rust
   // 立即emit Starting（如果foreground）
   if foreground {
       echo_daemon_starting_to_terminal(...);
   }
   
   // 异步等待readiness，不阻塞
   tokio::spawn(async move {
       let readiness = await_startup_readiness(...).await;
       if matches!(readiness, StartupReadiness::Ready { .. }) {
           // 再次emit Ready
           echo_daemon_ready_to_terminal(...);
       }
   });
   
   // 立即继续到shutdown-signal handling
   ```

2. 拆分`echo_daemon_started_to_terminal`为两个函数：
   - `echo_daemon_starting_to_terminal` - 立即emit
   - `echo_daemon_ready_to_terminal` - 等待ready后emit

3. 添加测试证明ordered lifecycle

#### 2. 缺少PTY测试
**问题**：
- 需要在实际PTY中测试daemon行为
- 需要覆盖：foreground startup、background job、--verbose、detached/service、delayed bind

**需要做什么**：
添加集成测试或文档证明：
1. Foreground startup通过ready
2. Background job不emit banner
3. --verbose只emit structured event
4. Detached/service不emit banner
5. Delayed bind先emit Starting然后emit Ready

## 🎯 修复优先级

### 最高优先级 - Audacity88的问题1（生命周期重构）
这是一个**重大设计变更**，需要重构整个startup echo逻辑。

### 高优先级 - IftekharUddin的问题2（Fluent本地化）
需要检查当前实现是否已经使用了Fluent。

### 中优先级 - PR body更新
需要重写PR body，但不影响代码。

### 中优先级 - Commit history清理
需要检查并移除bot attribution。

### 低优先级 - 测试补充
可以逐步添加。

## 📝 当前状态检查清单

- [ ] 检查`echo_daemon_started_to_terminal`是否使用Fluent
- [ ] 检查commit history是否有bot attribution
- [ ] 评估生命周期重构的工作量
- [ ] 制定详细的代码修改计划

## 🔧 详细修复步骤

### 步骤1：检查当前实现
```bash
# 检查是否使用Fluent
grep -n "cli_t\|cli_ta" crates/zeroclaw-runtime/src/daemon/mod.rs

# 检查commit history
git log --oneline | head -10
git show HEAD --stat
```

### 步骤2：生命周期重构（如果需要）
修改`crates/zeroclaw-runtime/src/daemon/mod.rs`：
1. 在`daemon::run`中立即emit Starting
2. 异步spawn等待readiness
3. ready时emit Ready
4. 立即继续到shutdown handling

### 步骤3：添加测试
添加测试证明ordered lifecycle

### 步骤4：更新PR body
重写PR body使用当前template

### 步骤5：清理commit history
移除bot attribution

## ⏰ 时间估算
- 步骤1（检查）：15分钟
- 步骤2（重构）：2-3小时
- 步骤3（测试）：1小时
- 步骤4（PR body）：30分钟
- 步骤5（commit清理）：30分钟

**总计**：4-5小时
