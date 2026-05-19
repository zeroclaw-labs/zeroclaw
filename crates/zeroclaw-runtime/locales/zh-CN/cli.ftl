cli-wechat-pairing-required = 🔐 需要绑定 WeChat。一次性绑定码：{$code}
cli-wechat-send-bind-command = 请在 WeChat 中发送 `{$command} <code>`。
cli-wechat-qr-login = 📱 WeChat 二维码登录（{$attempt}/{$max}）
cli-wechat-scan-to-connect = 请使用 WeChat 扫码连接。
cli-wechat-qr-url = 二维码 URL：{$url}
cli-wechat-qr-expired-giving-up = WeChat 二维码已过期 {$max} 次，停止重试。
cli-wechat-qr-fetch-failed = 获取 WeChat 二维码失败。
cli-wechat-qr-fetch-status-failed = 获取 WeChat 二维码失败（{$status}）：{$body}
cli-wechat-missing-response-field = WeChat 响应缺少 {$field}。
cli-wechat-scanned-confirm = 👀 已扫码！请在手机上确认...
cli-wechat-qr-expired-refreshing = ⏳ 二维码已过期，正在刷新...
cli-wechat-login-confirmed-missing-field = 登录已确认，但缺少 {$field}。
cli-wechat-connected = ✅ WeChat 已连接！
cli-wechat-bound-success = ✅ WeChat 账号绑定成功。现在可以和 ZeroClaw 对话了。
cli-wechat-invalid-bind-code = ❌ 绑定码无效。请重试。
cli-skills-install-start = 正在安装技能来源：{$source}
cli-skills-install-resolving-registry = { "  " }正在从技能注册表解析 '{$source}'...
cli-skills-install-installed-audited = { "  " }{$status} 技能已安装并审计：{$path}（已扫描 {$files} 个文件）
cli-skills-install-security-audit-completed = { "  " }安全审计已成功完成。
cli-skills-install-tier-official = 正在安装 {$name} v{$version} — 官方（zeroclaw-labs 维护）
cli-skills-install-tier-community =
    正在安装 {$name} v{$version} — 社区提交
    此技能未经 ZeroClaw 审计。请检查技能内容，
    并在授予任何权限或用于生产前运行 `zeroclaw skills audit {$name}`。

channel-runtime-current-route =
    当前提供商：`{$provider}`
    当前模型：`{$model}`
channel-runtime-switch-model-help = 使用 `/model <model-id>` 或 `/model <hint>` 切换模型。
channel-runtime-configured-model-routes = 已配置的模型路由：
channel-runtime-no-cached-models = 未找到 `{$provider}` 的缓存模型列表。请让操作者运行 `zeroclaw models refresh --provider {$provider}`。
channel-runtime-cached-models = 缓存模型 ID（前 {$count} 个）：
channel-runtime-switch-provider-help = 使用 `/models <provider>` 切换提供商。
channel-runtime-switch-model-command-help = 使用 `/model <model-id>` 切换模型。
channel-runtime-available-providers = 可用提供商：
channel-runtime-provider-aliases = 别名：{$aliases}
channel-runtime-use-models-and-model =
    使用 `/models <provider>` 切换提供商。
    使用 `/model <model-id>` 切换模型。
channel-runtime-provider-switched =
    已为当前发送者会话切换到提供商 `{$provider}`。当前模型为 `{$model}`。
    使用 `/model <model-id>` 设置兼容该提供商的模型。
channel-runtime-provider-init-failed =
    初始化提供商 `{$provider}` 失败。路由未更改。
    详情：{$details}
channel-runtime-provider-unavailable =
    ⚠️ 初始化提供商 `{$provider}` 失败。请运行 `/models` 选择其他提供商。
    详情：{$details}
channel-runtime-unknown-provider = 未知提供商 `{$provider}`。使用 `/models` 查看可用提供商。
channel-runtime-model-id-empty = 模型 ID 不能为空。请使用 `/model <model-id>`。
channel-runtime-model-switched = 已切换到模型 `{$model}`（提供商：`{$provider}`）。上下文已保留。
channel-runtime-new-session = 已清空对话历史，将从新的会话开始。
channel-runtime-stop-sent = 已发送停止信号。
channel-runtime-stop-none = 当前发送者范围内没有正在执行的任务。
channel-runtime-malformed-tool-output = 我遇到了格式异常的工具调用输出，无法生成安全回复。请重试。
channel-runtime-fallback-footer =
    ---
    ⚡ `{$requested_provider}` 不可用 — 本次由 **{$actual_provider}**（`{$actual_model}`）回复
    切换模型：/models
channel-runtime-tool-receipts-header =
    ---
    工具回执：
channel-runtime-context-window-exceeded-compacted = ⚠️ 当前对话超过上下文窗口。我已压缩最近历史并保留最新上下文。请重新发送上一条消息。
channel-runtime-context-window-exceeded = ⚠️ 当前对话超过上下文窗口。请重新发送上一条消息。
channel-runtime-request-timed-out = ⚠️ 等待模型回复超时。请重试。
channel-runtime-config-block-title = *模型配置*
    当前：`{$provider}` / `{$model}`
channel-runtime-select-provider = 选择提供商
channel-runtime-select-model = 选择模型
channel-runtime-provider-label = *提供商*
channel-runtime-model-label = *模型*
