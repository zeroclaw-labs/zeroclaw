cli-about = 最快、最小的 AI 助手。
cli-no-command-provided = 未提供命令。
cli-try-quickstart = 尝试运行 `zeroclaw quickstart` 来创建你的第一个智能体。
cli-quickstart-about = 端到端创建你的第一个智能体
cli-agent-about = 启动 AI 智能体循环
cli-gateway-about = 管理网关服务器（webhooks、websockets）
cli-acp-about = 启动 ACP 服务器（基于 stdio 的 JSON-RPC 2.0）
cli-daemon-about = 启动长时间运行的自主守护进程
cli-service-about = 管理操作系统服务生命周期（launchd/systemd 用户服务）
cli-doctor-about = 运行守护进程/调度器/渠道新鲜度诊断
cli-status-about = 显示系统状态（完整详情）
cli-estop-about = 启用、检查和恢复紧急停止状态
cli-cron-about = 配置和管理定时任务
cli-models-about = 管理提供商模型目录
cli-providers-about = 列出支持的 AI 提供商
cli-channel-about = 管理通信渠道
cli-integrations-about = 浏览 50+ 个集成
cli-skills-about = 管理技能（用户自定义能力）
cli-sop-about = 管理标准操作程序（SOPs）
cli-migrate-about = 从其他智能体运行时迁移数据
cli-auth-about = 管理提供商订阅认证配置文件
cli-hardware-about = 发现并检查 USB 硬件
cli-peripheral-about = 管理硬件外设
cli-memory-about = 管理智能体记忆条目
cli-config-about = 管理 ZeroClaw 配置
cli-update-about = 检查并应用 ZeroClaw 更新
cli-self-test-about = 运行诊断自检
cli-completions-about = 生成 shell 补全脚本
cli-desktop-about = 启动 ZeroClaw 伴侣桌面应用
cli-config-schema-about = 将完整的配置 JSON Schema 输出到 stdout
cli-config-list-about = 列出所有配置属性及其当前值
cli-config-get-about = 获取配置属性值
cli-config-set-about = 设置配置属性（密钥字段会自动提示进行掩码输入）
cli-config-init-about = 使用默认值初始化未配置的部分（enabled=false）
cli-config-migrate-about = 将磁盘上的 config.toml 迁移到当前架构版本（保留注释）
cli-service-install-about = 安装守护进程服务单元以实现自动启动和重启
cli-service-start-about = 启动守护进程服务
cli-service-stop-about = 停止守护进程服务
cli-service-restart-about = 重启守护进程服务以应用最新配置
cli-service-status-about = 检查守护进程服务状态
cli-service-uninstall-about = 卸载守护进程服务单元
cli-service-logs-about = 跟踪守护进程服务日志
cli-channel-list-about = 列出所有已配置的渠道
cli-channel-start-about = 启动所有已配置的渠道
cli-channel-doctor-about = 对已配置的渠道运行健康检查
cli-channel-add-about = 添加新的渠道配置
cli-channel-remove-about = 移除渠道配置
cli-channel-send-about = 向已配置的渠道发送一次性消息
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
cli-skills-list-about = 列出所有已安装的技能
cli-skills-audit-about = 审计技能源目录或已安装的技能名称
cli-skills-audit-failed = 技能审核失败。
cli-skills-install-about = 从 URL 或本地路径安装新技能
cli-skills-remove-about = 移除已安装的技能
cli-skills-test-about = 为某个技能（或所有技能）运行 TEST.sh 验证
cli-skills-review-summary = { "  " }💾 技能审查：{$summary}
cli-skills-install-start = 正在安装技能来源：{$source}
cli-skills-install-resolving-registry = { "  " }正在从技能注册表解析 '{$source}'...
cli-skills-install-resolving-extra-registry = { "  " }正在从注册表 '{$registry}' 解析 '{$source}'...
cli-skills-install-git-failed = 安装 git 技能源失败：{$source}
cli-skills-install-registry-failed = 从注册表安装技能失败：{$source}
cli-skills-install-extra-registry-failed = 从额外注册表安装技能失败：{$source}
cli-skills-install-local-failed = 安装本地技能源失败：{$source}
cli-skills-install-installed-audited = { "  " }{$status} 技能已安装并审计：{$path}（已扫描 {$files} 个文件）
cli-skills-install-security-audit-completed = { "  " }安全审计已成功完成。
cli-skills-install-into-bundle = { "  " }已安装到技能包 '{$alias}'。在 skill_bundles 中列出此技能包的代理将会加载它。
cli-skills-install-global-note = { "  " }注意：已安装到全局技能目录，没有代理会自动加载它。请使用 --bundle <alias> 重新运行，或将某个技能包分配给代理，以使其可加载。
cli-skills-removed-archived = { "  " }{$status} 技能 '{$name}' 已从技能包 '{$bundle}' 中移除（归档于 shared/skills/_deleted/ 下）。
cli-skills-removed-global = { "  " }{$status} 技能 '{$name}' 已从全局技能目录中移除。
cli-skills-install-tier-official = 正在安装 {$name} v{$version} — 官方（zeroclaw-labs 维护）
cli-skills-install-tier-community =
    正在安装 {$name} v{$version} — 社区提交
    此技能未经 ZeroClaw 审计。请检查技能内容，
    并在授予任何权限或用于生产前运行 `zeroclaw skills audit {$name}`。
cli-skills-add-scaffolded = 已在 {$dir} 搭建技能 {$target}
cli-skills-bundle-add-prompt =
    要创建目录为 '{$dir}' 的 skill-bundle '{$alias}'，请运行：
    zeroclaw config map-key skill-bundles {$alias}
    zeroclaw config set skill-bundles.{$alias}.directory {$dir}

    （通过 `zeroclaw skills bundle add` 直接创建包会重复配置变更接口。）
cli-skills-bundle-remove-prompt =
    要移除 skill-bundle '{$alias}'，请运行：
    zeroclaw config map-key-delete skill-bundles {$alias}

    （移除配置条目；磁盘上该包的目录会保留。）
cli-skills-bundle-list-empty =
    未配置技能包。
    创建一个：zeroclaw config set skill-bundles.default.directory shared/skills/default
cli-skills-bundle-list-header = 技能包（{$count}）：
cli-skills-bundle-entry = {$alias} -> {$dir}
cli-skills-bundle-include = 包含：{$values}
cli-skills-bundle-exclude = 排除：{$values}
cli-skills-bundle-show-no-skills = （未安装技能）
cli-skills-bundle-show-skills-header = 技能（{$count}）：
cli-skills-bundle-show-skill = {$name}：{$description}
cli-cron-list-about = 列出所有计划任务
cli-cron-add-about = 添加新的周期性计划任务
cli-cron-add-at-about = 添加一个在特定 UTC 时间戳触发的一次性任务
cli-cron-add-every-about = 添加一个以固定间隔重复的任务
cli-cron-once-about = 添加一个在从现在起延迟后触发的一次性任务
cli-cron-remove-about = 移除计划任务
cli-cron-update-about = 更新现有计划任务的一个或多个字段
cli-cron-pause-about = 暂停计划任务
cli-cron-resume-about = 恢复已暂停的任务
cli-auth-login-about = 使用 OAuth 登录（OpenAI Codex、Gemini 或 xAI）
cli-auth-refresh-about = 使用刷新令牌刷新 OAuth 访问令牌
cli-auth-logout-about = 移除认证配置文件
cli-auth-use-about = 为提供商设置活动配置文件
cli-auth-list-about = 列出认证配置文件
cli-auth-status-about = 显示认证状态，包括活动配置文件和令牌过期信息
cli-memory-list-about = 列出内存条目，可使用可选过滤器
cli-memory-get-about = 按键获取特定的内存条目
cli-memory-stats-about = 显示内存后端的统计信息和健康状况
cli-memory-clear-about = 按类别、按键清除内存，或清除全部
cli-memory-clear-unsupported-backend = 内存清除不支持仅追加后端 '{$backend}'；请切换到可删除的后端（sqlite、lucid 或 postgres）
cli-estop-status-about = 打印当前急停状态
cli-estop-resume-about = 从已激活的急停级别恢复
cli-models-refresh-about = 刷新并缓存提供商模型
cli-models-list-about = 列出提供商的缓存模型
cli-models-set-about = 在配置中设置默认模型
cli-models-status-about = 显示当前模型配置和缓存状态
cli-doctor-models-about = 探测各提供商的模型目录并报告可用性
cli-doctor-traces-about = 查询运行时跟踪事件（工具诊断和模型回复）
cli-doctor-update-context-windows-about = 从提供商的 /models 端点更新 config.toml 中的 context_window
cli-hardware-discover-about = 枚举 USB 设备并显示已知开发板
cli-hardware-introspect-about = 通过序列号或设备路径检视设备
cli-hardware-info-about = 通过 ST-Link 使用 probe-rs 经 USB 获取芯片信息
cli-peripheral-list-about = 列出已配置的外设
cli-peripheral-add-about = 按开发板类型和传输路径添加外设
cli-peripheral-flash-about = 将 ZeroClaw 固件刷写到 Arduino 开发板
cli-sop-list-about = 列出已加载的 SOP
cli-sop-validate-about = 验证 SOP 定义
cli-sop-show-about = 显示 SOP 的详细信息
cli-migrate-openclaw-about = 将 OpenClaw 工作区中的记忆导入到此 ZeroClaw 工作区
cli-agent-long-about =
    启动 AI 代理循环。

    与已配置的 AI 提供商启动交互式聊天会话。使用 --message 进行单次查询，无需进入交互模式。

    示例：
    zeroclaw agent                              # 交互式会话
    zeroclaw agent -m "Summarize today's logs"  # 单条消息
    zeroclaw agent -p anthropic --model claude-sonnet-4-20250514
    zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
cli-gateway-long-about =
    管理网关服务器（webhooks、websockets）。

    启动、重启或检查接受传入 webhook 事件和 WebSocket 连接的 HTTP/WebSocket 网关。

    示例：
    zeroclaw gateway start              # 启动网关
    zeroclaw gateway restart            # 重启网关
    zeroclaw gateway get-paircode       # 显示配对码
cli-acp-long-about =
    启动 ACP 服务器（通过 stdio 的 JSON-RPC 2.0）。

    在 stdin/stdout 上启动 JSON-RPC 2.0 服务器，用于 IDE 和工具集成。支持会话管理，并以通知形式流式传输代理响应。

    方法：initialize、session/new、session/prompt、session/stop。

    示例：
    zeroclaw acp                        # 启动 ACP 服务器
    zeroclaw acp --max-sessions 5       # 限制并发会话数
cli-daemon-long-about =
    启动长期运行的自主守护进程。

    启动完整的 ZeroClaw 运行时：网关服务器、所有已配置的通道（Telegram、Discord、Slack 等）、心跳监视器以及 cron 调度器。这是在生产环境中或作为始终在线助手运行 ZeroClaw 的推荐方式。

    使用 'zeroclaw service install' 将守护进程注册为操作系统服务（systemd/launchd），以便开机自动启动。

    示例：
    zeroclaw daemon                   # 使用配置默认值
    zeroclaw daemon -p 9090           # 网关在端口 9090
    zeroclaw daemon --host 127.0.0.1  # 仅 localhost
cli-cron-long-about =
    配置和管理计划任务。

    使用 cron 表达式、RFC 3339 时间戳、持续时间或固定间隔来调度重复、一次性或基于间隔的任务。

    Cron 表达式使用标准的 5 字段格式：'min hour day month weekday'。时区默认为 UTC；使用 --tz 和 IANA 时区名称覆盖。

    示例：
    zeroclaw cron list
    zeroclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York --agent
    zeroclaw cron add '*/30 * * * *' 'Check system health' --agent
    zeroclaw cron add '*/5 * * * *' 'echo ok'
    zeroclaw cron add-at 2025-01-15T14:00:00Z 'Send reminder' --agent
    zeroclaw cron add-every 60000 'Ping heartbeat'
    zeroclaw cron once 30m 'Run backup in 30 minutes' --agent
    zeroclaw cron pause TASK_ID
    zeroclaw cron update TASK_ID --expression '0 8 * * *' --tz Europe/London
cli-channel-long-about =
    管理通信通道。

    添加、删除、列出、发送以及对将 ZeroClaw 连接到消息平台的通道进行健康检查。支持的通道类型：telegram、discord、slack、whatsapp、matrix、imessage、email。

    示例：
    zeroclaw channel list
    zeroclaw channel doctor
    zeroclaw channel add telegram '{ "{" }"bot_token":"...","name":"my-bot"{ "}" }'
    zeroclaw channel remove my-bot
    zeroclaw channel bind-telegram zeroclaw_user
    zeroclaw channel send 'Alert!' --channel-id telegram --recipient 123456789
cli-hardware-long-about =
    发现和检视 USB 硬件。

    枚举已连接的 USB 设备，识别已知的开发板（STM32 Nucleo、Arduino、ESP32），并通过 probe-rs / ST-Link 检索芯片信息。

    示例：
    zeroclaw hardware discover
    zeroclaw hardware introspect /dev/ttyACM0
    zeroclaw hardware info --chip STM32F401RETx
cli-peripheral-long-about =
    管理硬件外设。

    添加、列出、烧录和配置向代理公开工具的硬件板（GPIO、传感器、执行器）。支持的板：nucleo-f401re、rpi-gpio、esp32、arduino-uno。

    示例：
    zeroclaw peripheral list
    zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
    zeroclaw peripheral add rpi-gpio native
    zeroclaw peripheral flash --port /dev/cu.usbmodem12345
    zeroclaw peripheral flash-nucleo
cli-memory-long-about =
    管理代理记忆条目。

    列出、检视和清除代理存储的记忆条目。支持按类别和会话过滤、分页以及带确认的批量清除。

    示例：
    zeroclaw memory stats
    zeroclaw memory list
    zeroclaw memory list --category core --limit 10
    zeroclaw memory get KEY
    zeroclaw memory clear --category conversation --yes
cli-config-long-about =
    管理 ZeroClaw 配置。

    通过点分路径查看、设置或初始化配置属性。使用 'schema' 转储配置文件的完整 JSON Schema。

    属性通过点分路径寻址（例如 channels.matrix.mention-only）。
    密钥字段（API 密钥、令牌）会自动使用掩码输入。
    枚举字段在省略值时提供交互式选择。

    示例：
    zeroclaw config list                                  # 列出所有属性
    zeroclaw config list --secrets                        # 仅列出密钥
    zeroclaw config list --filter channels.matrix         # 按前缀过滤
    zeroclaw config get channels.matrix.mention-only      # 获取值
    zeroclaw config set channels.matrix.mention-only true # 设置值
    zeroclaw config set channels.matrix.access-token      # 密钥：掩码输入
    zeroclaw config set channels.matrix.stream-mode       # 枚举：交互式选择
    zeroclaw config init channels.matrix                  # 使用默认值初始化部分
    zeroclaw config schema                                # 将 JSON Schema 打印到 stdout
    zeroclaw config schema > schema.json

    属性路径 Tab 补全会自动包含在 `zeroclaw completions <shell>` 中。
cli-update-long-about =
    检查并应用 ZeroClaw 更新。

    默认情况下，使用 6 阶段流水线下载并安装最新版本：预检、下载、备份、验证、交换和冒烟测试。失败时自动回滚。

    使用 --check 仅检查更新而不安装。
    使用 --force 跳过确认提示。
    使用 --version 指定特定版本而非最新版本。

    示例：
    zeroclaw update                      # 下载并安装最新版本
    zeroclaw update --check              # 仅检查，不安装
    zeroclaw update --force              # 不确认直接安装
    zeroclaw update --version 0.6.0      # 安装特定版本
cli-self-test-long-about =
    运行诊断自检以验证 ZeroClaw 安装。

    默认情况下，运行完整的测试套件，包括网络检查（网关健康状况、记忆往返）。使用 --quick 跳过网络检查以进行更快的离线验证。

    示例：
    zeroclaw self-test             # 完整套件
    zeroclaw self-test --quick     # 仅快速检查（无网络）
cli-skills-install-suggestion =
    看起来此请求需要 `{$name}` 技能，但它尚未安装。

    匹配的能力：{$matched}
    下一步：运行 `{$install_command}` 进行安装。
cli-plugin-install-suggestion =
    看起来此请求需要 `{$name}` 插件，但它尚未安装。

    匹配的能力：{$matched}
    下一步：运行 `{$install_command}` 进行安装。
cli-completions-long-about =
    为 `zeroclaw` 生成 shell 补全脚本。

    脚本会打印到 stdout，以便可以直接 source：

    示例：
    source <(zeroclaw completions bash)
    zeroclaw completions zsh > ~/.zfunc/_zeroclaw
    zeroclaw completions fish > ~/.config/fish/completions/zeroclaw.fish
cli-desktop-long-about =
    启动 ZeroClaw 配套桌面应用。

    配套应用是一个轻量级的菜单栏 / 系统托盘应用程序，它连接到与 CLI 相同的网关。它提供对仪表板、状态监控和设备配对的快速访问。

    使用 --install 下载适用于您平台的预构建配套应用。

    示例：
    zeroclaw desktop              # 启动配套应用
    zeroclaw desktop --install    # 下载并安装
channel-needs-quickstart-reply = 此代理尚未完全设置。操作员需要先运行 Quickstart，然后我才能回复。
channel-whatsapp-web-feature-missing-warning = ⚠ WhatsApp Web 已配置，但未编译 'whatsapp-web' 功能。
channel-whatsapp-web-feature-missing-build = 使用以下命令构建/运行：cargo build --features whatsapp-web
channel-whatsapp-web-feature-missing-install = 如果已安装到 PATH，请使用以下命令重新安装：cargo install --path . --force --locked --features whatsapp-web
channel-whatsapp-web-feature-missing-error = WhatsApp Web 通道需要 'whatsapp-web' 功能。使用以下命令启用：cargo build --features whatsapp-web（或者，如果已安装到 PATH：cargo install --path . --force --locked --features whatsapp-web）
channel-wecom-ws-stream-bootstrap = 正在处理中，请稍候。
channel-wecom-ws-stop-ack = 已停止当前消息处理。
channel-wecom-ws-voice-unavailable = 我现在无法处理语音消息 {$emoji}
channel-wecom-ws-unsupported-message = 暂不支持该消息类型。
channel-wecom-ws-welcome = 你好，欢迎来找我聊天 {$emoji}
channel-wecom-ws-supplemental-message =
    {"["}补充消息]
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
channel-discord-interaction-unauthorized = 你没有权限在此处使用该命令。
channel-discord-interaction-malformed = 未知或格式错误的命令。
channel-discord-interaction-unavailable = 该命令已不可用，或其输入为空。
channel-discord-component-expired = 此按钮或菜单已过期或已被使用。
channel-discord-approval-recorded = 您的决定已记录。
channel-discord-delivery-failure-note-one = （注意：我无法传送 {$count} 个文件。）
channel-discord-delivery-failure-note-many = （注意：我无法传送 {$count} 个文件。）
channel-whatsapp-web-delivery-failure-note-one = （注意：我无法传送 {$count} 个 WhatsApp 媒体附件。）
channel-whatsapp-web-delivery-failure-note-many = （注意：我无法传送 {$count} 个 WhatsApp 媒体附件。）
channel-line-bind-success = ✅ 已配对！现在可以聊天了。
channel-line-bind-invalid-code = ❌ 代码无效。请重试。
channel-line-bind-rate-limited = ⏳ 尝试次数过多。请在 { $secs }s 后重试。
onboard-openai-auth-note =
    OpenAI 身份验证：
    • API 密钥 — 通过 platform.openai.com 的标准 API 访问（sk-...）
    • Codex 订阅 — 使用您的 ChatGPT Plus/Pro 账户（无需 API 密钥）
onboard-openai-auth-prompt = 身份验证
onboard-openai-auth-api-key = API 密钥
onboard-openai-auth-codex = Codex 订阅
onboard-openai-codex-followup =
    Codex 订阅身份验证使用您的 ChatGPT 账户。
    在启动代理之前，运行 `zeroclaw auth login --provider openai-codex` 进行身份验证。
cli-web-dist-dir-reason-tilde = 以不会被展开的 `~` 开头
cli-web-dist-dir-reason-dollar = 包含不会被展开的 `$`
cli-doctor-web-dist-dir-expansion-warning = gateway.web_dist_dir = "{$path}" — {$reason}；gateway.web_dist_dir 按原样读取，请自行展开该值（例如使用绝对路径）
cli-doctor-codex-auth-profile-no-slot = OpenAI Codex 凭据已登录，但没有模型提供方槽位使用它们。请在 OpenAI 提供方槽位上设置 `requires_openai_auth = true`，并将 agent 的 `model_provider` 指向它，或运行 `zeroclaw quickstart`。
cli-doctor-codex-auth-slot-no-profile = OpenAI 槽位 {$slots} 已设置 `requires_openai_auth = true`，但没有 OpenAI Codex 凭据登录。请运行 `zeroclaw auth login --provider openai-codex`。
cli-doctor-codex-auth-ok = OpenAI Codex 凭据已登录，并由模型提供方槽位引用。
cli-doctor-systemd-linger-enabled = systemd 用户 linger 已启用
cli-doctor-systemd-linger-disabled = systemd 用户 linger 已禁用；用户服务可能会在注销后停止。启用命令：loginctl enable-linger {$user}
cli-doctor-systemd-linger-unknown = 无法使用 loginctl 检查 systemd 用户 linger
cli-self-test-web-dist-dir-name = web_dist_dir
cli-self-test-web-dist-dir-pass-unset = 未设置（使用自动检测）
cli-self-test-web-dist-dir-pass-literal = {$path}（字面路径）
cli-self-test-web-dist-dir-fail-expansion = 警告：{$path} — {$reason}；gateway.web_dist_dir 按原样读取，请自行展开该值（例如使用绝对路径）
cli-service-systemd-linger-disabled-warning = systemd 用户 linger 已禁用。ZeroClaw 的用户服务可能会在注销后停止。启用命令：loginctl enable-linger {$user}
cli-peripherals-none = 未配置外设。
cli-peripherals-add-hint = 使用以下命令添加: zeroclaw peripheral add <board> <path>
cli-peripherals-add-example = {"  "}示例: zeroclaw peripheral add nucleo-f401re <serial-path>
cli-peripherals-config-hint = 或添加到 config.toml:
cli-peripherals-configured = 已配置的外设:
cli-peripherals-already-configured = 位于 {$path} 的开发板 {$board} 已配置。
cli-peripherals-added = 已在 {$path} 添加 {$board}。重启守护进程以应用。
cli-peripherals-flash-needs-hardware = Arduino 烧录需要 'hardware' 功能。
cli-peripherals-unoq-needs-hardware = Uno Q 设置需要 'hardware' 功能。
cli-peripherals-nucleo-needs-hardware = Nucleo 烧录需要 'hardware' 功能。
cli-skills-none-installed = 未安装技能。
cli-skills-create-hint = {"  "}创建一个: mkdir -p ~/.zeroclaw/workspace/skills/my-skill
cli-skills-install-hint = {"  "}或安装: zeroclaw skills install <source>
cli-skills-installed-header = 已安装的技能 ({$count}):
cli-skills-list-group-bundle = 技能包：{$alias}
cli-skills-list-group-agent = 由代理 '{$alias}' 加载
cli-skills-list-group-global = 全局 / open-skills / 插件（非来自技能包）
cli-skills-agent-not-configured = 代理 '{$alias}' 未配置
cli-skills-agent-multiple-bundles = 代理 '{$alias}' 有多个技能包（{$bundles}）；请传入 --bundle 以选择其中一个
cli-skills-multiple-locations-bundle = 技能 '{$name}' 存在于多个位置（{$locations}）；请传入 --bundle 以选择其中一个
cli-skills-multiple-locations-path = 技能 '{$name}' 存在于多个位置（{$locations}）；请传入明确的路径以消除歧义
cli-skills-tags = 标签:  {$tags}
cli-skills-skipped-header = 已跳过 ({$count}):
cli-skills-skipped-reason = {"    "}原因: {$reason}
cli-skills-skipped-scripts-hint = {"    "}在 zeroclaw 配置中设置 `skills.allow_scripts = true` 以启用它。
cli-sop-none = 未找到 SOP。
cli-sop-pending-none = 没有等待审批的 SOP 运行。
cli-sop-pending-header = 等待审批的 SOP 运行：
cli-sop-pending-row = {"  "}{$run_id} [{$sop_name}] 步骤 {$step}/{$total}
cli-sop-ws-invalid-approval = sop approval_response 需要 run_id，以及 approve 或 deny 决策
cli-sop-ws-resolve-failed = SOP 解析失败：{$error}
cli-sop-ws-engine-lock-poisoned = SOP 引擎锁已中毒
cli-sop-ws-subsystem-disabled = SOP 子系统未启用
cli-sop-create-hint = {"  "}创建一个: mkdir -p <workspace>/sops/my-sop
cli-sop-create-hint-2 = {"              "}然后添加 SOP.toml 和 SOP.md
cli-sop-loaded-header = 已加载的 SOP ({$count}):
cli-sop-none-to-validate = 未找到可验证的 SOP。
cli-sop-valid = ✅ {$name} — 有效
cli-sop-deleted = 已删除 SOP：{$name}
cli-sop-warnings = ⚠️  {$name} — {$count} 个警告:
cli-sop-all-passed = 所有 SOP 均已通过验证。
cli-sop-priority = {"  "}优先级:       {$value}
cli-sop-execution-mode = {"  "}执行模式: {$value}
cli-sop-deterministic = {"  "}确定性:  {$value}
cli-sop-cooldown = {"  "}冷却时间:       {$value}s
cli-sop-max-concurrent = {"  "}最大并发数: {$value}
cli-sop-admission-policy = {"  "}准入策略:   {$value}
cli-sop-max-pending-approvals = {"  "}最大待批数: {$value}
cli-sop-location = {"  "}位置:       {$value}
cli-sop-triggers = {"  "}触发器:
cli-sop-steps = {"  "}步骤:
cli-sop-step-tools = 工具: {$tools}
cli-memory-reindexing = 正在重新索引记忆后端...
cli-memory-none = 未找到记忆条目。
cli-memory-none-at-offset = 偏移量 {$offset} 处无条目(总计: {$total})。
cli-memory-next-page = 使用 --offset {$offset} 查看下一页。
cli-memory-key-not-found = 未找到键对应的记忆条目: {$key}
cli-memory-prefix-matched = 前缀 '{$key}' 匹配了 {$n} 个条目:
cli-memory-narrow-prefix = 请指定更长的前缀以缩小匹配范围。
cli-memory-key = 键:       {$value}
cli-memory-category = 类别:  {$value}
cli-memory-timestamp = 时间戳: {$value}
cli-memory-session = 会话:   {$value}
cli-memory-stats-header = 记忆统计:
cli-memory-backend = {"  "}后端:  {$value}
cli-memory-total = {"  "}总计:    {$value}
cli-memory-by-category = {"  "}按类别:
cli-memory-none-to-clear = 无可清除的条目。
cli-memory-found-in-scope = 在 '{$scope}' 中找到 {$count} 个条目。
cli-memory-aborted = 已中止。
cli-memory-deleted-key = 已删除键：{$key}
cli-cron-none = 暂无计划任务。
cli-cron-usage = 用法：
cli-cron-jobs-header = 🕒 计划任务 ({$count}):
cli-cron-list-cmd = {"    "}命令: {$cmd}
cli-cron-list-prompt = {"    "}提示词: {$prompt}
cli-cron-added-agent = ✅ 已添加 agent cron 任务 {$id}
cli-cron-added = ✅ 已添加 cron 任务 {$id}
cli-cron-added-oneshot-agent = ✅ 已添加一次性 agent cron 任务 {$id}
cli-cron-added-oneshot = ✅ 已添加一次性 cron 任务 {$id}
cli-cron-added-interval-agent = ✅ 已添加间隔 agent cron 任务 {$id}
cli-cron-added-interval = ✅ 已添加间隔 cron 任务 {$id}
cli-cron-updated = ✅ 已更新 cron 任务 {$id}
cli-cron-update-no-field = 必须至少提供 --expression、--tz、--command、--name、--allowed-tool 或 --uses-memory 中的一个
cli-cron-removed = ✅ 已移除 cron 任务 {$id}
cli-cron-paused = ⏸️  已暂停 cron 任务 {$id}
cli-cron-resumed = ▶️  已恢复 cron 任务 {$id}
cli-cron-expr = {"  "}表达式  : {$v}
cli-cron-expr2 = {"  "}表达式: {$v}
cli-cron-next = {"  "}下次  : {$v}
cli-cron-next2 = {"  "}下次: {$v}
cli-cron-next3 = {"  "}下次     : {$v}
cli-cron-prompt = {"  "}提示词: {$v}
cli-cron-prompt3 = {"  "}提示词   : {$v}
cli-cron-cmd = {"  "}命令 : {$v}
cli-cron-cmd3 = {"  "}命令      : {$v}
cli-cron-at = {"  "}时间    : {$v}
cli-cron-at2 = {"  "}时间  : {$v}
cli-cron-every = {"  "}间隔(ms): {$v}
cli-no-command = 未提供命令。
cli-press-enter = 按 Enter 退出...
cli-quickstart-title = Quickstart — 端到端创建一个可用的 agent。
cli-quickstart-needs-tty = Quickstart 是交互式流程，需要 stdin 和 stderr 连接到终端。请从交互式 shell 中运行，或使用 `zeroclaw config set <path> <value>` 进行无头配置。
cli-quickstart-cancelled = 已取消 quickstart。未写入配置。
cli-quickstart-incomplete = {"  "}尚未填写所有选择器。
cli-quickstart-create-agent = ── 创建 agent
cli-quickstart-create-agent-locked = ── 创建 agent（已锁定 — 请先填写所有选择器）
cli-quickstart-open-selector-prompt = 打开选择器（Enter），或选择创建。Esc 退出。
cli-quickstart-use-existing = 使用已有项
cli-quickstart-create-new = 新建
cli-quickstart-model-provider-prompt = 模型提供方
cli-quickstart-pick-configured-provider = 选择已配置的提供方
cli-quickstart-row-model-provider = {$glyph} 模型提供方       — {$summary}
cli-quickstart-row-risk-profile = {$glyph} 风险配置文件     — {$summary}
cli-quickstart-row-memory = {$glyph} 记忆              — {$summary}
cli-quickstart-row-channels = {$glyph} 通道 (0..N)      — {$summary}
cli-quickstart-row-peer-groups = {$glyph} 对等组            — {$summary}
cli-quickstart-row-agent-identity = {$glyph} agent 身份       — {$summary}
cli-quickstart-summary-not-yet-chosen = 尚未选择
cli-quickstart-summary-not-yet-visited = 尚未访问
cli-quickstart-summary-not-yet-named = 尚未命名
cli-quickstart-summary-provider-fresh = {$name}（别名：{$alias}，模型：{$model}）
cli-quickstart-summary-use-existing = 使用已有 {$reference}
cli-quickstart-summary-preset-fresh = 预设：{$name}
cli-quickstart-summary-channels-none = 无（仅通过 `zeroclaw agent` 聊天）
cli-quickstart-summary-agent = 别名：{$alias}，system prompt：{$chars} 个字符，{$files} 个性格文件
cli-quickstart-summary-peer-groups-none = 无 — 通道不接受任何对等方
cli-quickstart-channel-remove-row = {"  "}{$reference}（移除）
cli-quickstart-peer-group-row = {$channel} → {$name}（{$count} 个对等方）
cli-quickstart-provider-local-label = {$name}（本地）
cli-quickstart-provider-type-prompt = 提供方类型
cli-quickstart-alias-for = {$name} 的别名
cli-quickstart-openai-auth-mode-label = 身份验证
cli-quickstart-openai-auth-mode-help = 选择 `codex` 以使用 ChatGPT/Codex 订阅身份验证配置文件。如果你已通过 Codex CLI 登录，请运行 `zeroclaw auth login --model-provider openai-codex --import ~/.codex/auth.json`；否则请运行 `zeroclaw auth login --model-provider openai-codex`。
cli-quickstart-anthropic-auth-mode-label = 身份验证
cli-quickstart-anthropic-auth-mode-help = 如需 Anthropic Console 密钥，请选择 `api_key`；如果你打算为 Claude Max 运行 `claude setup-token` 并粘贴生成的令牌，请选择 `setup_token`。
cli-quickstart-anthropic-api-key-help = 粘贴 Anthropic Console API 密钥或由 `claude setup-token` 生成的令牌。
cli-quickstart-auth-codex-prompt = 现在使用你的 ChatGPT 账户登录 OpenAI Codex 吗？
cli-quickstart-auth-codex-import-prompt = 发现已有的 Codex 登录 (~/.codex/auth.json) — 现在导入吗？
cli-quickstart-auth-codex-skip-hint = {"  "}稍后完成：zeroclaw auth login --model-provider openai-codex
cli-quickstart-auth-anthropic-prompt = 现在为 Anthropic 提供方 `{$alias}` 运行 `claude setup-token` 吗？
cli-quickstart-auth-anthropic-token-prompt = 粘贴 `claude setup-token` 生成的令牌
cli-quickstart-auth-anthropic-saved = {"  "}已为 anthropic.{$alias} 保存 Claude setup token。
cli-quickstart-auth-anthropic-skip-hint = {"  "}稍后完成：claude setup-token，然后 zeroclaw config set providers.models.anthropic.{$alias}.api_key <token>
cli-quickstart-auth-failed = {"  "}身份验证设置未完成：{$error}
cli-quickstart-model-field-missing-warning = 警告：架构没有为 `{$provider}` 生成 `model` 字段 — 将退回到手动输入。请报告此问题。
cli-quickstart-model-id-for = {$name} 的模型 ID
cli-quickstart-risk-profile-prompt = 风险配置文件
cli-quickstart-memory-backend-prompt = 记忆后端
cli-quickstart-add-channel = + 添加通道
cli-quickstart-channels-done = 完成（通道选择器会记为已访问）
cli-quickstart-channels-prompt = 通道（可选，0..N）
cli-quickstart-channel-source-prompt = 通道来源
cli-quickstart-all-channels-bound = {"  "}所有已配置的通道都已经绑定到 agent。若要在这里复用，请先用 `zeroclaw config set agents.<alias>.channels ...` 释放一个通道。
cli-quickstart-pick-configured-channel = 选择已配置的通道
cli-quickstart-channel-type-prompt = 通道类型
cli-quickstart-add-peer-group = + 添加对等组
cli-quickstart-done = 完成
cli-quickstart-peer-groups-prompt = 对等组（在某一行按 Enter 可移除，+ 添加可创建）
cli-quickstart-channel-to-authorize-prompt = 要授权的通道
cli-quickstart-external-peers-prompt = 外部对等方（用逗号或换行分隔，留空表示无）
cli-quickstart-agent-alias-prompt = Agent 别名
cli-quickstart-edit-system-prompt = 在 $EDITOR 中编辑 system prompt？（留空则跳过）
cli-quickstart-personality-start-template = 从模板开始（在 $EDITOR 中打开）
cli-quickstart-personality-start-current = 从当前内容开始（在 $EDITOR 中打开）
cli-quickstart-personality-start-scratch = 从空白开始（在 $EDITOR 中打开）
cli-quickstart-personality-skip = 跳过
cli-quickstart-esc-go-back = {" "}（Esc 返回）
cli-quickstart-esc-return-checklist = {" "}（Esc 返回检查清单）
cli-quickstart-personality-file-prompt = {$filename}{$position} — 下一步？{$back_hint}
cli-quickstart-next-agent-command = {"  "}zeroclaw agent -a {$alias}  # 在终端中与此 agent 聊天
cli-quickstart-fix-and-rerun = 现有配置未被修改。请修复以下问题，然后重新运行 quickstart：
cli-quickstart-could-not-finish = quickstart 无法完成：需要修复 {$count} 个问题
cli-quickstart-pick-preset = 选择预设
cli-quickstart-pick-existing-prompt = 选择已有的 {$prompt}
cli-quickstart-pick-preset-prompt = 选择 {$prompt} 预设
cli-quickstart-step-model-provider = 模型提供方
cli-quickstart-step-risk-profile = 风险配置文件
cli-quickstart-step-runtime-profile = 运行时配置文件
cli-quickstart-step-memory = 记忆
cli-quickstart-step-channels = 通道
cli-quickstart-step-peer-groups = 对等组
cli-quickstart-step-agent = Agent
cli-quickstart-error-internal-no-result = 内部错误：没有验证错误，但 apply_into 没有返回结果
cli-quickstart-error-completion-flag = 无法切换 quickstart-completed：{$err}
cli-quickstart-error-persist-config = 无法持久化配置：{$err}
cli-quickstart-error-not-type-alias-ref = `{$reference}` 不是 `<type>.<alias>` 引用
cli-quickstart-error-no-configured-path = 未配置 `{$path}`
cli-quickstart-error-provider-required = 必须填写提供方类型、别名和模型
cli-quickstart-error-unknown-provider-type = 未知模型提供方类型 `{$provider}` — 请从提供方列表中选择
cli-quickstart-error-unknown-openai-auth-mode = 未知的 OpenAI 身份验证模式 `{$mode}` — 请选择 `api_key` 或 `codex`
cli-quickstart-error-unknown-anthropic-auth-mode = 未知的 Anthropic 身份验证模式 `{$mode}` — 请选择 `api_key` 或 `setup_token`
cli-quickstart-error-alias-exists = 别名 `{$alias}` 已存在
cli-quickstart-error-no-profile = 未配置 `{$alias}` 配置文件
cli-quickstart-error-unknown-risk-preset = 未知风险预设 `{$preset}`
cli-quickstart-error-unknown-runtime-preset = 未知运行时预设 `{$preset}`
cli-quickstart-error-channel-bound = 通道 `{$reference}` 已绑定到 agent `{$owner}`
cli-quickstart-error-channel-required = 必须填写通道类型和别名
cli-quickstart-error-channel-field-not-advertised = Quickstart 中不支持通道字段 `{$field}`
cli-quickstart-error-channel-token-required = 必须填写 Telegram Bot 令牌
cli-quickstart-error-peer-group-name-required = 必须填写对等组名称
cli-quickstart-error-peer-group-channel-required = 必须填写对等组通道引用
cli-quickstart-error-peer-group-unknown-channel = 对等组 `{$name}` 引用了未知通道 `{$channel}`
cli-quickstart-error-peer-group-exists = 对等组 `{$name}` 已存在
cli-quickstart-error-personality-workspace = 无法创建 agent 工作区：{$err}
cli-quickstart-error-personality-filename-required = 必须填写文件名
cli-quickstart-error-personality-not-editable = `{$filename}` 不是可编辑的人格文件
cli-quickstart-error-personality-too-large = 内容超过 {$limit} 字符限制
cli-quickstart-error-personality-stage-failed = 暂存 {$filename} 失败：{$err}
cli-quickstart-error-personality-write-failed = 写入 {$path} 失败：{$err}
cli-quickstart-error-agent-name-required = 必须填写 agent 名称
cli-quickstart-error-agent-exists = agent `{$name}` 已存在
cli-no-channels-compiled = {"  "}此二进制文件中未编译任何通道类型。
cli-quickstart-complete = Quickstart 完成。已创建 agent `{$alias}`。
cli-next-steps = 后续步骤：
cli-agent-not-created = 未创建您的 agent — 磁盘上没有任何更改。
cli-onboard-deprecated = `zeroclaw onboard` 已弃用 — 请使用 `zeroclaw quickstart`。
cli-otp-initialized = 已为 ZeroClaw 初始化 OTP 密钥。
cli-otp-enrollment-uri = 注册 URI：{$uri}
cli-otp-received = {"  "}✓ OTP 已接收
cli-secret-captured = {"  "}● 已接收输入 — 按 Enter 保存
cli-secret-received = {"  "}✓ 密钥已接收
cli-pairing-enabled = 🔐 已启用 gateway 配对。
cli-pairing-use-code = {"  "}使用此一次性代码配对新设备：
cli-pairing-post = {"    "}POST /pair，附带请求头 X-Pairing-Code: {$code}
cli-pairing-restart = {"   "}重启 gateway 以生成新的配对码。
cli-pairing-disabled = ⚠️  配置中已禁用 gateway 配对。
cli-gateway-running-q = {"   "}gateway 是否正在运行？使用以下命令启动它：
cli-status-title = 🦀 ZeroClaw 状态
cli-security-status-title = ZeroClaw 安全状态
cli-security-status-source = 来源：      {$v}
cli-security-status-agent = Agent：       {$v}
cli-security-status-agent-enabled = Agent 已启用：{$enabled}
cli-security-status-risk-profile = 风险配置文件：{$v}
cli-security-status-autonomy = 自主性：   {$v}
cli-security-status-approvals = 审批：  需要中风险审批：{$medium}，已阻止高风险命令：{$high}
cli-security-status-sandbox = 沙箱：    已请求 {$requested}，已激活 {$active}（{$description}）
cli-security-status-workspace = 工作区：  {$dir}；仅工作区：{$workspace_only}；读写根目录：{$read_write_roots}；只读根目录：{$read_only_roots}；只写根目录：{$write_only_roots}；环境变量透传：{$env_passthrough}
cli-security-status-credentials = 凭据：加密：{$encryption}；已设置密钥：{$secrets_set}/{$secrets_total}；已分类字段：{$classified_total}；类别：{$classification_summary}
cli-security-status-credentials-classes-none = 无
cli-security-status-gateway = 网关：    {$host}:{$port}；需要配对：{$pairing}；公开绑定：{$public_bind}；TLS：{$tls}
cli-security-status-warnings = 警告：   {$v}
cli-security-status-warnings-none = 警告：   无
cli-security-status-warning-agent-disabled = agent 已禁用
cli-security-status-warning-sandbox-disabled = 此 agent 风险配置文件已禁用沙箱
cli-security-status-warning-sandbox-none = 激活的沙箱仅为应用层
cli-security-status-warning-sandbox-fallback = 请求的沙箱后端 `{$requested}` 已回退到 `{$active}`
cli-security-status-warning-workspace-not-restricted = 仅工作区文件系统策略已禁用
cli-security-status-warning-shell-env-passthrough = 已透传 {$count} 个 shell 环境变量
cli-security-status-warning-secrets-unencrypted = 配置密钥加密已禁用
cli-security-status-warning-credential-follow-up = 某些凭据形式的配置项仍需后续处理
cli-security-status-warning-pairing-disabled = 网关配对非必需
cli-security-status-warning-public-bind-no-tls = 网关允许公开绑定但未启用 TLS
cli-status-provider-none = 🤖 ModelProvider:      （未配置）
cli-status-agents-none = 🛡️  Agents:        （未配置）
cli-status-service-running = 🟢 服务：       运行中
cli-status-service-stopped = 🔴 服务：       已停止
cli-status-channels = 通道：
cli-status-cli-always = {"  "}CLI:      ✅ 始终
cli-status-peripherals = 外设：
cli-desktop-download = 下载 ZeroClaw 配套应用：
cli-desktop-homebrew = 或通过 Homebrew 安装（即将推出）：
cli-desktop-linux-pkg = {"  "}下载适合您架构的 .deb 或 .AppImage。
cli-desktop-launching = 正在启动 ZeroClaw 配套应用...
cli-status-version = 版本：     {$v}
cli-status-workspace = 工作区：   {$v}
cli-status-config = 配置：      {$v}
cli-status-provider-indent = {"   "}ModelProvider:      {$family}.{$alias}
cli-status-provider = 🤖 ModelProvider:      {$family}.{$alias}
cli-status-model = {"   "}模型：         {$model}
cli-status-observability = 📊 可观测性：  {$v}
cli-status-trace-storage = 🧾 跟踪存储：  {$mode}（{$path}）
cli-status-agents = 🛡️  Agents:        {$v}
cli-status-runtime = ⚙️  运行时：       {$v}
cli-status-heartbeat = 💓 心跳：      {$v}
cli-status-heartbeat-every-minutes = 每 {$minutes} 分钟
cli-status-memory = 🧠 内存：         {$backend}（自动保存：{$auto_save}）
cli-status-security-noprofile = 安全（{$alias}）：<无 risk_profile>
cli-status-security = 安全（{$alias}）：
cli-status-workspace-only = {"  "}仅工作区：    {$v}
cli-status-allowed-roots = {"  "}允许的根目录：     {$v}
cli-status-allowed-commands = {"  "}允许的命令：  {$v}
cli-status-max-actions = {"  "}每小时最大操作数：  {$v}
cli-status-cost-tracking = {"  "}成本跟踪：     {$v}
cli-status-max-cost-day = {"  "}每日最大成本：      ${$v}
cli-status-max-cost-month = {"  "}每月最大成本：    ${$v}
cli-status-spent-today = {"  "}今日已花费：       ${$spent} / ${$limit}
cli-status-spent-month = {"  "}本月已花费：  ${$spent} / ${$limit}
cli-status-otp = {"  "}已启用 OTP：       {$v}
cli-status-estop = {"  "}已启用急停：    {$v}
cli-status-peripherals-enabled = {"  "}已启用：   {$v}
cli-status-boards = {"  "}Boards:    {$v}
cli-status-word-enabled = 已启用
cli-status-word-disabled = 已禁用
cli-status-word-yes = 是
cli-status-word-no = 否
cli-status-word-on = 开启
cli-status-word-off = 关闭
cli-status-word-none = （无）
cli-status-word-configured = 已配置
cli-status-word-not-configured = 未配置
cli-status-channel-not-compiled = 🚫 已配置，未编译
cli-desktop-not-installed = 未安装 ZeroClaw 配套应用。
cli-desktop-blurb1 = 该配套应用是一个轻量级菜单栏应用，
cli-desktop-blurb2 = 它连接到与 CLI 相同的网关。
cli-config-all-configured = 所有部分均已配置。
cli-config-schema-current = 配置已为当前架构版本。
cli-config-applied-ops = 已应用 {$count} 个操作：
cli-plugins-none = 未安装任何插件。
cli-plugins-installed = 已安装的插件：
cli-plugin-search-none = 没有匹配 '{$query}' 的插件。
cli-plugin-search-results = 匹配 '{$query}' 的插件（{$count}）：
cli-plugin-search-result = {$name} v{$version} — {$description}
cli-plugin-no-description = （无描述）
cli-plugin-install-resolving = 正在从插件注册表解析 '{$source}'...
cli-plugin-installed-from = 已从 {$source} 安装插件
cli-plugin-installed-name-version = 已安装插件 {$name} v{$version}
cli-plugin-config-entry-seeded = 已为 '{$name}' 创建 [[plugins.entries]]。使用 `zeroclaw config set plugins.entries.{$name}.config.<key>` 设置插件配置值。
cli-plugin-config-entry-seed-skipped = 警告：已跳过为 '{$name}' 创建配置条目：磁盘上的 [plugins] 部分格式不正确。请修复它，添加带有 `name = "{$name}"` 的 [[plugins.entries]] 块，然后使用 `zeroclaw config set plugins.entries.{$name}.config.<key>` 设置值。
cli-plugin-config-entry-seed-unaddressable = 警告：已跳过为 '{$name}' 创建配置条目：包含 '.' 的插件名称无法通过点分配置路径寻址（`config set` 会按 '.' 分割）。请手动向配置文件添加带有 `name = "{$name}"` 的 [[plugins.entries]] 块。
cli-config-section-degraded = 警告：{$path} 中的配置部分 `{$section}` 格式不正确，本次运行已重置为默认值。该部分中的值不会生效。请运行 `zeroclaw config migrate` 查看解析错误，然后修复文件。
cli-plugin-removed = 已移除插件“{$name}”。
cli-plugin-not-found = 未找到插件“{$name}”。
cli-plugin-legacy-detected = 注意：位于旧位置（{$path}）的插件未被代理加载。请运行 `zeroclaw plugin migrate` 将其移动到 {$target}。
cli-plugin-migrated = 已将 {$count} 个插件从 {$path} 移动到 {$target}。
cli-plugin-migrate-none = 没有需要迁移的内容。
cli-estop-resume-done = 急停恢复已完成。
cli-estop-engaged = 急停已启用。
cli-estop-status = 急停状态：
cli-auth-none = 未配置认证配置文件。
cli-auth-active = 活动配置文件：
cli-warn-crypto-provider = 警告：安装默认加密提供程序失败：{$err}
cli-error-label = {"   "}错误：{$err}
cli-warn-cost-usage = {"  "}⚠ 无法加载成本使用情况：{$err}
cli-warn-cost-tracker = {"  "}⚠ 无法初始化成本跟踪器：{$err}
cli-desktop-download-at = {"  "}下载地址：{$url}
cli-config-legend = 图例：💉 env 已覆盖  🔒 密钥
cli-config-secret-set = {$path} 已设置（加密密钥——不显示值）
cli-config-secret-unset = {$path} 未设置（加密密钥）
cli-config-updated = {$path} 已更新。
cli-config-review-hint = 运行 `zeroclaw config list` 进行查看，然后设置必填字段。
cli-config-backed-up = 已备份至 {$path}
cli-plugin-name-version = 插件：{$name} v{$version}
cli-plugin-description = 描述：{$desc}
cli-plugin-capabilities = 功能：{$v}
cli-plugin-permissions = 权限：{$v}
cli-plugin-wasm = WASM：{$path}
cli-plugin-wasm-none = WASM：（仅技能插件）
cli-estop-domains-none = {"  "}domain_blocks:  （无）
cli-estop-domains = {"  "}domain_blocks:  {$v}
cli-estop-tools-none = {"  "}tool_freeze:    （无）
cli-estop-tools = {"  "}tool_freeze:    {$v}
cli-estop-updated-at = {"  "}updated_at:     {$v}
cli-auth-saved = 已保存配置文件 {$profile}
cli-auth-active-for = {$provider} 的活动配置文件：{$profile}
cli-auth-refresh-ok = ✓ 令牌刷新成功（配置文件 {$profile}）
cli-auth-removed = 已移除身份验证配置文件 {$provider}:{$profile}
cli-auth-not-found = 未找到身份验证配置文件：{$provider}:{$profile}
cli-auth-xai-imported = 已从 {$path} 导入 xAI 身份验证配置文件
cli-auth-xai-device-code-started = xAI 设备码登录已开始。
cli-auth-oauth-visit = 访问：{$uri}
cli-auth-oauth-code = 代码：  {$code}
cli-auth-oauth-fast-link = 快速链接：{$uri}
cli-auth-xai-open-oauth-url = 在浏览器中打开此 xAI OAuth URL 并授权访问：
cli-auth-callback-capture-failed = 回调捕获失败：{$error}
cli-auth-run-paste-redirect = 运行 `zeroclaw auth paste-redirect --model-provider {$provider} --profile {$profile}`
cli-auth-xai-no-pending-login = 未找到待处理的 xAI 登录。请先运行 `zeroclaw auth login --model-provider xai`。
cli-auth-paste-redirect-requires-input = paste-redirect 需要重定向 URL 或 OAuth 代码
cli-locales-fetched = {"  "}已获取 {$name} -> {$path}
cli-locales-skipped = {"  "}已跳过 {$name}：不在上游（{$path}；已尝试 {$refs}）
cli-locales-installed = 已为“{$locale}”在 {$dir} 下安装 {$count} 个目录
cli-browse-header = {$path}（{$count} 个条目）
cli-browse-empty = （空）
cli-browse-file-bytes = {$name}（{$bytes} 字节）
cli-hardware-feature-required = 硬件发现需要 'hardware' 功能。
cli-hardware-feature-build = 构建命令：cargo build --features hardware
cli-hardware-unsupported-platform = 此平台不支持硬件 USB 发现。
cli-hardware-supported-platforms = 支持的平台：Linux、macOS、Windows。
cli-update-already-current = 已是最新版本（v{$version}）。
cli-update-success = 已成功更新至 v{$version}！
cli-update-prebuilt-channel-note = 预构建更新使用精简的标准发行集。如需 Slack 和其他未包含在该发行集中的通道，请从源码构建：`./install.sh --source --preset full`、`--features channels-full`，或指定对应的 `channel-*` 功能。
cli-update-available = 有可用更新：v{$current} -> v{$latest}
cli-update-forcing-reinstall = 强制重新安装：v{$current} -> v{$latest}
cli-update-not-writable = 安装目录 {$dir} 不可写（{$error}）；请使用更高权限重新运行 `zeroclaw update`（在 macOS/Linux 上使用 sudo，在 Windows 上使用管理员控制台）
cli-selftest-all-passed = 全部 {$total} 项检查通过。
cli-selftest-some-failed = {$failed}/{$total} 项检查失败。
cli-selftest-channel-config-uncompiled = {$compiled} 个通道类型已编译，{$configured} 个已编译且已配置；已配置但未编译：{$names}。请从源码构建：`./install.sh --source --preset full`、`--features channels-full`，或指定对应的 `channel-*` 功能。
cli-channels-header = 渠道：
cli-channels-cli-always = {"  "}✅ CLI（始终可用）
cli-channels-notion = {"  "}{$status} Notion
cli-channels-not-compiled-header = {"  "}已配置但未编译进此二进制文件：
cli-channels-not-compiled-entry = {"  "}🚫 {$name}（已配置，未编译）
cli-channels-build-hint = {"  "}请从源码构建：`./install.sh --source --preset full`、`--features channels-full`，或指定对应的 `channel-*` 功能。
cli-channels-start-hint = 启动渠道：zeroclaw channel start
cli-channels-doctor-hint = 检查健康状况：    zeroclaw channel doctor
cli-channels-configure-hint = 配置方法：      zeroclaw config set channels.<name>.<field>=<value>
cli-models-set-ok = 默认模型已设置为 "{ $model }" (provider: { $provider })。
cli-models-status-current = 默认模型: { $model } (provider: { $provider })
cli-models-status-none = 未配置默认模型。
turn-interrupted-by-user = [被用户中断]
turn-cancelled-client-rpc = [已通过客户端取消回合]
turn-stream-interrupted = [流已中断]
turn-model-fallback-notice = ⚡ { $requested_model }（{ $requested_provider }）不可用；此回复由 { $actual_model }（{ $actual_provider }）生成。
history-trim-breadcrumb = [earlier turns omitted to fit the context window]
history-trim-reason-budget = context token budget exceeded
history-trim-reason-message-cap = 已超出历史消息数量限制
history-trim-floor-exceeds-budget = system prompt and tool definitions ({$floor} tokens) alone meet or exceed the context budget ({$budget} tokens); raise [runtime_profiles.<name>] max_context_tokens or reduce the tool surface by disabling unused integrations
turn-ingress-dropped = 此请求未被处理：{ $reason }
turn-tool-interrupted-before-result = [在此工具产生结果前被用户中断]
channel-runtime-malformed-tool-output = 我生成了内部工具调用格式错误，无法完成此请求。请重试。
channel-runtime-new-session = 对话历史已清除。重新开始。
channel-runtime-stop-sent = 已发送停止信号。
channel-runtime-stop-no-task = 此发送者范围内没有正在执行的任务。
channel-runtime-model-empty = 模型 ID 不能为空。请使用 `/model <model-id>`。
channel-runtime-model-switched = 已切换到模型 `{ $model }`（model_provider：`{ $provider }`）。上下文已保留。
channel-runtime-agent-scope-rejected = 发送者 `{ $sender }` 无权在 agent `{ $agent }` 上执行 `/model --agent`。请改用 `/model --user { $model }`（仅本次会话生效），或请管理员将 peer group 的 `admin_for_agent_scope` 设为 `true` 并将你列为成员。
channel-runtime-request-timeout = ⚠️ 等待模型响应超时，请重试。
channel-runtime-current-model-status =
    当前 model_provider：`{ $provider }`
    当前模型：`{ $model }`
channel-runtime-model-switch-hint = 使用 `/model <model-id>` 或 `/model <hint>` 切换模型。
channel-runtime-provider-switch-hint = 使用 `/models <model_provider>` 切换 model_provider。
channel-runtime-available-providers-header = 可用的 model_provider：
channel-runtime-configured-routes-header = 已配置的模型路由：
channel-runtime-no-cached-models = 未找到 `{ $provider }` 的缓存模型列表。请让操作者运行 `zeroclaw models refresh --model-provider { $provider }`。
channel-runtime-cached-model-ids-header = 缓存的模型 ID（前 { $count } 个）：
channel-runtime-config-switch-hints =
    使用 `/models <model_provider>` 切换 model_provider。
    使用 `/model <model-id>` 切换模型。
channel-runtime-config-block-title =
    { "*" }模型配置{ "*" }
    当前：`{ $provider }` / `{ $model }`
channel-runtime-config-select-provider-placeholder = 选择 model_provider
channel-runtime-config-select-model-placeholder = 选择模型
channel-runtime-config-provider-label = *ModelProvider*
channel-runtime-config-model-label = *模型*
channel-runtime-scope-user = 用户
channel-runtime-scope-agent = agent
channel-runtime-scope-overrides-summary =
    { "**" }模型覆盖{ "**" }（仅会话内；优先级 user > agent > session > default）：
    • user：{ $user }
    • agent：{ $agent }
    • session（此聊天）：{ $session }
    • default（配置）：{ $default }
    使用 `/model --user|--agent <model-id>` 设置范围；设置回默认值即可清除。
channel-runtime-set-provider-switched =
    已为此发送者会话切换到 ModelProvider `{ $provider }`。当前模型为 `{ $model }`。
    使用 `/model <model-id>` 设置与该 provider 兼容的模型。
channel-runtime-set-provider-init-failed =
    初始化 model_provider `{ $provider }` 失败。路由未更改。
    详情：{ $error }
channel-runtime-provider-ambiguous = ModelProvider `{ $family }` 有多个已配置别名。请用 `/models { $family }.<alias>` 指定其中一个：{ $list }
channel-runtime-provider-no-alias = 未找到 `{ $provider }` 的已配置 provider 条目。请添加 `[providers.models.{ $provider }]`（含 api_key/uri），或选择一个已配置的 provider；`/models` 会列出有效项。
channel-runtime-provider-unknown = 未知 model_provider `{ $provider }`。使用 `/models` 查看有效的 model_provider。
channel-runtime-scoped-model-empty = 模型 ID 不能为空。请使用 `/model --user|--agent <model-id>`。
channel-runtime-scoped-model-switched = 已为 **{ $scope }** 范围设置模型 `{ $model }`（model_provider：`{ $provider }`）。仅会话内有效，重启后重置。
channel-runtime-shadow-note = ⚠️ 当前有更高优先级的覆盖生效，因此消息将改用 `{ $model }`（`{ $provider }`）；请查看 `/model`。
channel-runtime-thinking-set =
    已为此发送者会话将 thinking 设为 `{ $level }`。
    使用 `/thinking reset` 返回 agent 默认值。
channel-runtime-thinking-cleared = thinking 覆盖已清除。此发送者会话将使用 agent 默认值 `{ $default }`。
channel-runtime-thinking-default =
    thinking 已经在此发送者会话中使用 agent 默认值 `{ $default }`。
    使用 `/thinking high`、`/thinking max` 或 `/thinking off` 覆盖。
channel-runtime-thinking-invalid = 未知 thinking 等级 `{ $raw }`。使用 `/thinking off|minimal|low|medium|high|max`、`/thinking on` 或 `/thinking reset`。
channel-runtime-provider-turn-init-failed =
    ⚠️ 初始化 model_provider `{ $provider }` 失败。请运行 `/models` 选择另一个 model_provider。
    详情：{ $error }
channel-runtime-fallback-footer =
    ⚡ `{ $requested }` 不可用 — 已由 **{ $actual }**（`{ $model }`）响应
    切换模型：/models
cli-alias-list-empty = （{$section} 下无条目）
cli-alias-created = 已创建 {$section}.{$alias}
cli-alias-exists = {$section}.{$alias} 已存在（未更改）
cli-alias-impact-scrub-header = 删除 {$section}.{$alias} 将清除 {$count} 个引用：
cli-alias-impact-blocked-header = 删除 {$section}.{$alias} 被 {$count} 个硬引用阻止：
cli-alias-impact-blocker = ✗ {$path}（硬引用）
cli-alias-impact-scrub = • {$path}（将被清除）
cli-alias-no-changes = 未做任何更改。请使用 --yes 重新运行以应用更改（或使用 --dry-run 预览）。
cli-alias-warn-workspace-archive = 警告：workspace 归档失败：{$error}
cli-alias-owned-cascaded = 所属状态已级联：memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions} → {$archive}
cli-alias-owned-repointed = 所属状态已重新指向：memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions}
cli-alias-warn-workspace-move = 警告：workspace 移动失败：{$error}
cli-alias-warn = 警告：{$warning}
cli-alias-deleted = 已删除 {$section}.{$alias}（已清除 {$count} 个引用）
cli-alias-delete-refused-header = 已拒绝：{$count} 个硬引用阻止了删除：
cli-alias-delete-refused-hint = 删除被拒绝——请先解决硬引用问题
cli-alias-not-configured = {$path} 未配置
cli-alias-delete-failed = 删除失败：{$error}
cli-alias-delete-reserved-default = `default` agent 为保留项，无法删除
cli-alias-create-reserved-default = `default` agent 为保留项，无法创建
cli-alias-renamed = 已重命名 {$section}.{$from} → {$section}.{$to}（重写了 {$count} 个引用路径）
cli-alias-rename-invalid = 新别名无效：{$message}
cli-alias-rename-reserved = 别名 `{$alias}` 为保留项，无法重命名
cli-alias-rename-postcondition = 重命名级联后置条件失败：{$message}
cli-alias-unknown-provider-category = 未知的 provider 类别 `{$category}`（应为 models | tts | transcription）
cli-alias-no-such-section = 没有这样的配置部分：{$section}
cli-alias-live-acp-sessions = `{$alias}` 有 {$count} 个活跃的 ACP 会话——请先结束它们
cli-alias-owned-state-unavailable = 注意：配置引用已更新，但此 CLI 尚未级联该 agent 的所属状态（memory 行、workspace 目录、cron/acp/session 行）——请使用 gateway API 进行完整的所属状态级联。
cli-bundle-not-configured = skill bundle '{$alias}' 未配置
cli-bundle-rename-failed = 重命名失败：{$error}
cli-bundle-exists = skill bundle '{$alias}' 已存在（未更改）
cli-bundle-created = 已创建 skill_bundles.{$alias}（目录：{$dir}）
cli-bundle-created-warn = 已创建 skill_bundles.{$alias}（警告：目录解析失败：{$error}）
cli-bundle-impact-header = 删除 skill_bundles.{$alias} 将从 {$count} 个 agent 引用中移除它：
cli-bundle-no-changes = 未做任何更改。请使用 --yes 重新运行以应用更改。
cli-bundle-archived = 已归档 bundle 目录 → {$path}
cli-bundle-warn-archive = 警告：bundle 目录归档失败：{$error}
cli-bundle-deleted = 已删除 skill_bundles.{$alias}（已从 {$count} 个 agent 中移除）
cli-bundle-warn-move = 警告：bundle 目录移动失败：{$error}
cli-bundle-renamed = 已重命名 skill_bundles.{$from} → skill_bundles.{$to}

# ── Web 仪表盘重启提示 — 应用内升级后显示的 RestartInfo.hint（PR #8173）──
# 前四个是按原样展示的 shell 命令模板，不作翻译。
cli-gateway-restart-hint-kubernetes = kubectl rollout restart deployment/zeroclaw
cli-gateway-restart-hint-container = docker compose restart
cli-gateway-restart-hint-systemd = systemctl restart zeroclaw
cli-gateway-restart-hint-launchd = launchctl kickstart -k <your-zeroclaw-label>
cli-gateway-restart-hint-process = 重启 `zeroclaw daemon` 进程

cli-daemon-gateway-already-running = ZeroClaw gateway 已在 {$host}:{$port} 运行。daemon 会管理自己的 gateway，不会在同一地址启动第二个 gateway。请停止该 gateway（或使用 `zeroclaw config set gateway.port <port>` 将 daemon 指向空闲端口），然后重新运行 daemon。
cli-daemon-gateway-port-occupied = Gateway 地址 {$host}:{$port} 已被另一个进程占用。请释放该端口或将 daemon 指向空闲端口（`zeroclaw config set gateway.port <port>`），然后重新运行 daemon。
cli-agent-context-bar = ctx: {$used} / {$max}  {$bar}  {$pct}%
cli-agent-context-bar-unknown = ctx: 未知 / {$max}
cli-doctor-ctxwin-already-set = {$provider_ref}: 已有 context_window = {$ctx}
cli-doctor-ctxwin-no-model = {$provider_ref}: 未配置模型，跳过
cli-doctor-ctxwin-would-set = {$provider_ref}: 将设置 context_window = {$ctx} (试运行)
cli-doctor-ctxwin-set = {$provider_ref}: 已设置 context_window = {$ctx}
cli-doctor-ctxwin-not-found = {$provider_ref}: 找不到要更新的条目
cli-doctor-ctxwin-fetch-failed = {$provider_ref}: 提供商未暴露上下文窗口或获取失败
cli-doctor-ctxwin-saved = 已保存 {$updated} 项更新到 config.toml
cli-doctor-ctxwin-dry-run = 试运行完成 — 未写入更改。去掉 --dry-run 以应用。
cli-doctor-ctxwin-none = 无需更新。
cli-doctor-ctxwin-write-failed = {$provider_ref}: 写入 context_window 失败: {$error}

# ── Degraded config sections (doctor diagnose, #8835) ──
cli-doctor-degraded-security = 安全关键配置节 `{$path}` 无效，已重置为默认值以便守护进程启动；当前运行的安全态势可能弱于预期。运行 `zeroclaw config migrate` 查看解析错误，然后修复该文件。
cli-doctor-degraded-section = 配置节 `{$path}` 格式错误，已重置为默认值；该节中的值当前不生效。运行 `zeroclaw config migrate` 查看解析错误，然后修复该文件。
sop-approval-deferred-at-capacity = 执行槽位已满，无法恢复运行 {$run_id}。审批仍处于等待状态；请在槽位释放后重试。
sop-approval-policy-unavailable = 无法使用暂停的 SOP 步骤，审批失败：{$reason}。运行仍处于等待状态。
sop-rpc-decision-invalid-state = 运行 {$run_id} 无法在当前状态下完成决策。
sop-rpc-decision-unauthorized = RPC 主体无权对该 SOP 步骤作出决策。
sop-rpc-policy-missing = 未配置 SOP 审批策略“{$name}”。
sop-rpc-policy-unavailable = 暂停的 SOP 策略不可用：{$reason}。
