tool-backup = 创建、列出、验证和恢复工作区备份

tool-browser = 支持可插拔后端（agent-browser、rust-native、computer_use）的 Web/浏览器自动化工具。支持 DOM 操作，以及通过 computer-use sidecar 提供的可选系统级操作（mouse_move、mouse_click、mouse_drag、key_type、key_press、screen_capture）。使用 'snapshot' 可将交互元素映射为 refs（@e1、@e2）。open 操作受 browser.allowed_domains 限制。

tool-browser-delegate = 将基于浏览器的任务委托给支持浏览器能力的 CLI，用于与 Teams、Outlook、Jira、Confluence 等 Web 应用交互

tool-browser-open = 在系统浏览器中打开已批准的 HTTPS URL。安全限制：仅允许白名单域名，不允许本地/私有主机，不支持抓取。

tool-cloud-ops = 云转型咨询工具。分析 IaC 计划、评估迁移路径、审查成本，并根据 Well-Architected Framework 支柱检查架构。只读：不会创建或修改云资源。

tool-cloud-patterns = 云模式库。根据工作负载描述，推荐适用的云原生架构模式（容器化、Serverless、数据库现代化等）。

tool-composio = 通过 Composio 在 1000+ 应用上执行操作（Gmail、Notion、GitHub、Slack 等）。使用 action='list' 查看可用操作（包含参数名）。使用 action='execute' 并提供 action_name/tool_slug 与 params 执行操作。如果不确定具体参数，可改为传入 'text'，用自然语言描述需求（Composio 会通过 NLP 自动解析参数）。使用 action='list_accounts' 或 action='connected_accounts' 查看 OAuth 已连接账户。使用 action='connect' 并提供 app/auth_config_id 获取 OAuth URL。connected_account_id 省略时会自动解析。

tool-content-search = 使用正则表达式在工作区内搜索文件内容。支持 ripgrep（rg），若不可用则回退到 grep。输出模式包括：'content'（带上下文的匹配行）、'files_with_matches'（仅返回文件路径）、'count'（每个文件的匹配数量）。示例：pattern='fn main'，include='*.rs'，output_mode='content'。

tool-cron-add = 创建定时 cron 任务（shell 或 agent）。支持 cron/at/every 调度方式。使用 job_type='agent' 并提供 prompt 可定时运行 AI agent。若需将结果发送到频道（Discord、Telegram、Slack、Mattermost、Matrix），请设置 delivery={"{"}"mode":"announce","channel":"discord","to":"<channel_id_or_chat_id>"{"}"}}。这是向用户频道发送定时/延迟消息的推荐工具。

tool-cron-list = 列出所有已调度的 cron 任务

tool-cron-remove = 根据 id 删除 cron 任务

tool-cron-run = 立即强制执行某个 cron 任务并记录运行历史

tool-cron-runs = 列出某个 cron 任务最近的运行历史

tool-cron-update = 更新现有 cron 任务（调度、命令、prompt、enabled、delivery、model 等）

tool-data-management = 工作区数据保留、清理与存储统计管理

tool-delegate = 将子任务委托给专用 Agent。当任务适合其他模型处理时使用（例如快速摘要、深度推理、代码生成）。默认情况下子 Agent 只运行单次 prompt；若设置 agentic=true，则可通过受限工具循环进行多轮迭代。

tool-file-edit = 通过精确字符串匹配替换的方式编辑文件内容

tool-file-read = 读取文件内容并显示行号。支持通过 offset 和 limit 进行部分读取。可从 PDF 提取文本；其他二进制文件将以有损 UTF-8 转换方式读取。

tool-file-write = 向工作区中的文件写入内容

tool-git-operations = 执行结构化 Git 操作（status、diff、log、branch、commit、add、checkout、stash）。提供解析后的 JSON 输出，并集成安全策略与自治控制。

tool-glob-search = 在工作区内按 glob 模式搜索文件。返回相对于工作区根目录排序后的匹配文件路径列表。示例：'**/*.rs'（所有 Rust 文件）、'src/**/mod.rs'（src 下所有 mod.rs）。

tool-google-workspace = 通过 gws CLI 与 Google Workspace 服务交互（Drive、Gmail、Calendar、Sheets、Docs 等）。需要安装并完成 gws 身份认证。

tool-hardware-board-info = 返回已连接硬件的完整板卡信息（芯片、架构、内存映射）。适用于用户询问“板卡信息”、“我连接的是什么板子”、“已连接硬件”、“芯片信息”、“是什么硬件”或“内存映射”等场景。

tool-hardware-memory-map = 返回已连接硬件的内存映射（Flash 和 RAM 地址范围）。适用于用户询问“高低内存地址”、“内存映射”、“地址空间”或“可读地址”等场景。返回来自数据手册的 Flash/RAM 地址范围。

tool-hardware-memory-read = 通过 USB 从 Nucleo 读取实际内存/寄存器值。适用于用户要求“读取寄存器值”、“读取指定地址内存”、“dump 内存”、“低地址内存 0-126”或“给出地址和值”等场景。返回十六进制 dump。需要通过 USB 连接 Nucleo 并启用 probe 功能。参数：address（十六进制，例如 RAM 起始地址 0x20000000）、length（字节数，默认 128）。

tool-http-request = 向外部 API 发起 HTTP 请求。支持 GET、POST、PUT、DELETE、PATCH、HEAD、OPTIONS 方法。安全限制：仅允许白名单域名，不允许本地/私有主机，可配置超时与响应大小限制。

tool-image-info = 读取图片文件元数据（格式、尺寸、大小），并可选返回 Base64 编码数据。

tool-jira = 与 Jira 交互：获取可配置详情级别的工单、使用 JQL 搜索问题，以及支持 mention 与格式化的评论添加。

tool-knowledge = 管理架构决策、解决方案模式、经验教训与专家信息的知识图谱。操作包括：capture、search、relate、suggest、expert_find、lessons_extract、graph_stats。

tool-linkedin = 管理 LinkedIn：创建帖子、列出帖子、评论、点赞/反应、删除帖子、查看互动数据、获取个人资料信息，以及读取已配置的内容策略。需要在 .env 文件中配置 LINKEDIN_* 凭据。

tool-discord-search = 搜索存储在 discord.db 中的 Discord 消息历史。用于查找历史消息、总结频道活动或查看用户发言。支持关键词搜索以及可选过滤条件：channel_id、since、until。

tool-memory-forget = 根据 key 删除记忆。用于移除过期事实或敏感数据。返回是否找到并删除成功。

tool-memory-recall = 从长期记忆中搜索相关事实、偏好或上下文。返回按相关性排序的结果。省略 query 或传入裸 '*' 可返回最近记忆。

tool-memory-store = 在长期记忆中存储事实、偏好或备注。使用 category='core' 保存永久事实，'daily' 保存每日笔记，'conversation' 保存对话上下文，也可使用自定义分类。

tool-microsoft365 = Microsoft 365 集成：通过 Microsoft Graph API 管理 Outlook 邮件、Teams 消息、日历事件、OneDrive 文件和 SharePoint 搜索

tool-model-routing-config = 管理默认模型设置、基于场景的 provider/model 路由、分类规则以及代理子 Agent 配置

tool-notion = 与 Notion 交互：查询数据库、读取/创建/更新页面以及搜索工作区。

tool-pdf-read = 从工作区中的 PDF 文件提取纯文本。返回所有可读文本。对于纯图片或加密 PDF 会返回空结果。需要启用 'rag-pdf' 构建特性。

tool-project-intel = 项目交付智能工具：生成状态报告、检测风险、起草客户更新、总结 Sprint，并估算工作量。只读分析工具。

tool-proxy-config = 管理 ZeroClaw 代理设置（scope: environment | zeroclaw | services），包括运行时和进程环境变量应用

tool-pushover = 向你的设备发送 Pushover 通知。需要在 .env 文件中配置 PUSHOVER_TOKEN 和 PUSHOVER_USER_KEY。

tool-schedule = 管理仅执行 shell 的定时任务。支持操作：create/add/once/list/get/cancel/remove/pause/resume。警告：此工具创建的 shell 任务输出仅记录日志，不会发送到任何频道。如需向 Discord/Telegram/Slack/Matrix 发送定时消息，请使用 cron_add 并配置 job_type='agent' 与 delivery={"{"}"mode":"announce","channel":"discord","to":"<channel_id>"{"}"}}。

tool-screenshot = 捕获当前屏幕截图。返回文件路径和 Base64 编码的 PNG 数据。

tool-security-ops = 面向托管网络安全服务的安全运营工具。操作包括：triage_alert（告警分类与优先级排序）、run_playbook（执行事件响应流程）、parse_vulnerability（解析漏洞扫描结果）、generate_report（生成安全态势报告）、list_playbooks（列出可用 playbook）、alert_stats（汇总告警指标）。

tool-shell = 在工作区目录中执行 Shell 命令。Windows 上宿主 shell 是 PowerShell（优先 pwsh.exe，回退 powershell.exe）——直接调用 cmdlet（Get-ChildItem、Format-Table 等），不要再用 `powershell -Command "..."` 或 `cmd /c ...` 包一层；否则外层 powershell/cmd 也必须在 allowed_commands 里才能通过。Unix 上宿主 shell 是 sh。

tool-sop-advance = 报告当前 SOP 步骤结果并推进到下一步。需提供 run_id、步骤成功或失败状态，以及简要输出摘要。

tool-sop-approve = 批准等待操作员确认的 SOP 步骤。返回需要执行的步骤说明。可使用 sop_status 查看哪些运行正在等待。

tool-sop-execute = 根据名称手动触发标准操作流程（SOP）。返回运行 ID 和第一步说明。可使用 sop_list 查看可用 SOP。

tool-sop-list = 列出所有已加载的标准操作流程（SOP），包括触发器、优先级、步骤数和活动运行数。支持按名称或优先级过滤。

tool-sop-status = 查询 SOP 执行状态。提供 run_id 可查看指定运行，提供 sop_name 可列出该 SOP 的所有运行。不提供参数则显示所有活动运行。

tool-swarm = 编排多个 Agent 协同完成任务。支持顺序（pipeline）、并行（fan-out/fan-in）和路由（LLM-selected）策略。

tool-tool-search = 获取延迟加载 MCP 工具的完整 schema 定义，以便后续调用。使用 "select:name1,name2" 精确匹配，或输入关键词进行搜索。

tool-web-fetch = 抓取网页并返回清理后的纯文本内容。HTML 页面会自动转换为可读文本；JSON 与纯文本响应将原样返回。仅支持 GET 请求并自动跟随重定向。安全限制：仅允许白名单域名，不允许本地/私有主机。

tool-web-search-tool = 在 Web 上搜索信息。返回相关搜索结果，包括标题、URL 和描述。用于获取最新信息、新闻或研究主题。

tool-workspace = 管理多客户端工作区。子命令包括：list、switch、create、info、export。每个工作区都提供隔离的记忆、审计、密钥和工具限制。

tool-weather = 获取全球任意地点的当前天气和天气预报。支持城市名（任意语言/文字）、IATA 机场代码（例如 'LAX'）、GPS 坐标（例如 '51.5,-0.1'）、邮编以及基于域名的地理定位。返回温度、体感温度、湿度、风速/风向、降水量、能见度、气压、UV 指数和云量。可选提供 0-3 天天气预报与小时级明细。默认使用公制单位（°C、km/h、mm），也可按需切换为英制（°F、mph、inch）。无需 API Key。
