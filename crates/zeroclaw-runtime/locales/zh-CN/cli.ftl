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
cli-skills-install-about = 从 URL 或本地路径安装新技能
cli-skills-remove-about = 移除已安装的技能
cli-skills-test-about = 为某个技能（或所有技能）运行 TEST.sh 验证
cli-skills-install-start = 正在安装技能来源：{$source}
cli-skills-install-resolving-registry = { "  " }正在从技能注册表解析 '{$source}'...
cli-skills-install-installed-audited = { "  " }{$status} 技能已安装并审计：{$path}（已扫描 {$files} 个文件）
cli-skills-install-security-audit-completed = { "  " }安全审计已成功完成。
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
cli-auth-login-about = 使用 OAuth 登录（OpenAI Codex 或 Gemini）
cli-auth-refresh-about = 使用刷新令牌刷新 OpenAI Codex 访问令牌
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
channel-discord-delivery-failure-note-one = （注意：我无法传送 {$count} 个文件。）
channel-discord-delivery-failure-note-many = （注意：我无法传送 {$count} 个文件。）
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
cli-skills-tags = 标签:  {$tags}
cli-sop-none = 未找到 SOP。
cli-sop-create-hint = {"  "}创建一个: mkdir -p <workspace>/sops/my-sop
cli-sop-create-hint-2 = {"              "}然后添加 SOP.toml 和 SOP.md
cli-sop-loaded-header = 已加载的 SOP ({$count}):
cli-sop-none-to-validate = 未找到可验证的 SOP。
cli-sop-valid = ✅ {$name} — 有效
cli-sop-warnings = ⚠️  {$name} — {$count} 个警告:
cli-sop-all-passed = 所有 SOP 均已通过验证。
cli-sop-priority = {"  "}优先级:       {$value}
cli-sop-execution-mode = {"  "}执行模式: {$value}
cli-sop-deterministic = {"  "}确定性:  {$value}
cli-sop-cooldown = {"  "}冷却时间:       {$value}s
cli-sop-max-concurrent = {"  "}最大并发数: {$value}
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
cli-quickstart-cancelled = 已取消 quickstart。未写入配置。
cli-quickstart-incomplete = {"  "}尚未填写所有选择器。
cli-no-channels-compiled = {"  "}此二进制文件中未编译任何通道类型。
cli-quickstart-complete = Quickstart 完成。已创建 agent `{$alias}`。
cli-next-steps = 后续步骤：
cli-agent-not-created = 未创建您的 agent — 磁盘上没有任何更改。
cli-onboard-deprecated = `zeroclaw onboard` 已弃用 — 请使用 `zeroclaw quickstart`。
cli-otp-initialized = 已为 ZeroClaw 初始化 OTP 密钥。
cli-otp-enrollment-uri = 注册 URI：{$uri}
cli-pairing-enabled = 🔐 已启用 gateway 配对。
cli-pairing-use-code = {"  "}使用此一次性代码配对新设备：
cli-pairing-post = {"    "}POST /pair，附带请求头 X-Pairing-Code: {$code}
cli-pairing-restart = {"   "}重启 gateway 以生成新的配对码。
cli-pairing-disabled = ⚠️  配置中已禁用 gateway 配对。
cli-gateway-running-q = {"   "}gateway 是否正在运行？使用以下命令启动它：
cli-status-title = 🦀 ZeroClaw 状态
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
cli-status-agents = 🛡️  Agents:        {$v}
cli-status-runtime = ⚙️  运行时：       {$v}
cli-status-security-noprofile = 安全（{$alias}）：<无 risk_profile>
cli-status-security = 安全（{$alias}）：
cli-status-workspace-only = {"  "}仅工作区：    {$v}
cli-status-max-actions = {"  "}每小时最大操作数：  {$v}
cli-status-max-cost-day = {"  "}每日最大成本：      ${$v}
cli-status-max-cost-month = {"  "}每月最大成本：    ${$v}
cli-status-otp = {"  "}已启用 OTP：       {$v}
cli-status-estop = {"  "}已启用急停：    {$v}
cli-status-boards = {"  "}Boards:    {$v}
cli-desktop-not-installed = 未安装 ZeroClaw 配套应用。
cli-desktop-blurb1 = 该配套应用是一个轻量级菜单栏应用，
cli-desktop-blurb2 = 它连接到与 CLI 相同的网关。
cli-config-all-configured = 所有部分均已配置。
cli-config-schema-current = 配置已为当前架构版本。
cli-config-applied-ops = 已应用 {$count} 个操作：
cli-plugins-none = 未安装任何插件。
cli-plugins-installed = 已安装的插件：
cli-plugin-installed-from = 已从 {$source} 安装插件
cli-plugin-removed = 已移除插件“{$name}”。
cli-plugin-not-found = 未找到插件“{$name}”。
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
cli-selftest-all-passed = 全部 {$total} 项检查通过。
cli-selftest-some-failed = {$failed}/{$total} 项检查失败。
cli-channels-header = 渠道：
cli-channels-cli-always = {"  "}✅ CLI（始终可用）
cli-channels-notion = {"  "}{$status} Notion
cli-channels-start-hint = 启动渠道：zeroclaw channel start
cli-channels-doctor-hint = 检查健康状况：    zeroclaw channel doctor
cli-channels-configure-hint = 配置方法：      zeroclaw config set channels.<name>.<field>=<value>
