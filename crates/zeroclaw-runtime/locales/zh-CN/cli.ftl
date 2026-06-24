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

channel-wecom-ws-stream-bootstrap = 正在处理中，请稍候。
channel-wecom-ws-stop-ack = 已停止当前消息处理。
channel-wecom-ws-voice-unavailable = 我现在无法处理语音消息 {$emoji}
channel-wecom-ws-unsupported-message = 暂不支持该消息类型。
channel-wecom-ws-welcome = 你好，欢迎来找我聊天 {$emoji}
channel-wecom-ws-supplemental-message =
    [补充消息]
    {$extra}
channel-wecom-ws-group-allowlist-missing =
    管理员尚未配置 WeCom allowlist，当前机器人不接收任何群消息。

    群 chatid: {$chatid}
    发送者 userid: {$userid}

    请在 {$allowed_groups_path} 或 {$allowed_users_path} 中加入允许项，也可以临时设置为 ["*"] 进行测试。
channel-wecom-ws-group-access-denied =
    当前群未被允许使用此机器人。

    群 chatid: {$chatid}
    发送者 userid: {$userid}

    请管理员将该群加入 {$allowed_groups_path}，或将你的 userid 加入 {$allowed_users_path}。
channel-wecom-ws-dm-allowlist-missing =
    管理员尚未配置 WeCom allowlist，当前机器人不接收任何消息。

    你的 userid: {$userid}

    请在 {$allowed_users_path} 中加入允许项，也可以临时设置为 ["*"] 进行测试。
channel-wecom-ws-dm-access-denied =
    你没有权限使用此机器人。

    你的 userid: {$userid}

    请管理员将你的 userid 加入 {$allowed_users_path}。
