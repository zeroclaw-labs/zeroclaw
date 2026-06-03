tool-backup = 创建、列出、验证和恢复工作区备份
tool-browser = 使用可插拔后端（agent-browser、rust-native、computer_use）进行网页/浏览器自动化。支持 DOM 操作以及通过 computer-use 边车进行的可选系统级操作（mouse_move、mouse_click、mouse_drag、key_type、key_press、screen_capture）。使用 'snapshot' 将交互元素映射到引用（@e1、@e2）。对 open 操作强制执行 browser.allowed_domains。
tool-browser-delegate = 将基于浏览器的任务委托给具备浏览器能力的 CLI，用于与 Teams、Outlook、Jira、Confluence 等 Web 应用进行交互
tool-browser-open = 在系统浏览器中打开经批准的 HTTPS URL。安全约束：仅限允许列表中的域名，不允许本地/私有主机，不允许抓取。
tool-cloud-ops = 云转型咨询工具。分析 IaC 计划、评估迁移路径、审查成本，并依据 Well-Architected Framework 支柱检查架构。只读：不创建或修改云资源。
tool-cloud-patterns = 云模式库。根据工作负载描述，建议适用的云原生架构模式（容器化、无服务器、数据库现代化等）。
tool-composio = 通过 Composio 在 1000 多个应用上执行操作（Gmail、Notion、GitHub、Slack 等）。使用 action='list' 查看可用操作（包含参数名称）。使用 action='execute' 并提供 action_name/tool_slug 和 params 来运行操作。如果不确定确切的参数，请改为传入 'text' 并附上你想要执行内容的自然语言描述（Composio 将通过 NLP 解析出正确的参数）。使用 action='list_accounts' 或 action='connected_accounts' 列出 OAuth 已连接的账户。使用 action='connect' 并提供 app/auth_config_id 获取 OAuth URL。省略时会自动解析 connected_account_id。
tool-content-search = 在工作区内按正则表达式模式搜索文件内容。支持 ripgrep (rg)，并以 grep 作为后备。输出模式：'content'（带上下文的匹配行）、'files_with_matches'（仅文件路径）、'count'（每个文件的匹配数量）。示例：pattern='fn main'，include='*.rs'，output_mode='content'。
tool-cron-add = 创建一个定时 cron 任务（shell 或 agent），支持 cron/at/every 调度。使用 job_type='agent' 并提供提示词以按计划运行 AI agent。要将输出投递到频道（Discord、Telegram、Slack、Mattermost、Matrix），请设置 delivery={"{"}"mode":"announce","channel":"discord","to":"<channel_id_or_chat_id>"{"}"}。这是通过频道向用户发送定时/延迟消息的首选工具。
tool-cron-list = 列出所有定时 cron 任务
tool-cron-remove = 按 id 移除一个 cron 任务
tool-cron-run = 立即强制运行一个 cron 任务并记录运行历史
tool-cron-runs = 列出某个 cron 任务的近期运行历史
tool-cron-update = 修补现有的 cron 任务（schedule、command、prompt、enabled、delivery、model 等）
tool-data-management = 工作区数据保留、清除和存储统计
tool-delegate = 将子任务委托给专门的 agent。适用场景：某项任务受益于不同的模型（例如快速摘要、深度推理、代码生成）。默认情况下子 agent 运行单个提示词；当 agentic=true 时，它可以通过经过筛选的工具调用循环进行迭代。
tool-file-edit = 通过将精确匹配的字符串替换为新内容来编辑文件
tool-file-read = 读取带行号的文件内容。支持通过 offset 和 limit 进行部分读取。可从 PDF 提取文本；其他二进制文件以有损 UTF-8 转换方式读取。
tool-file-write = 将内容写入工作区中的文件
tool-git-operations = 执行结构化的 Git 操作（status、diff、log、branch、commit、add、checkout、stash）。提供解析后的 JSON 输出，并与安全策略集成以实现自主控制。
tool-glob-search = 在工作区内搜索匹配 glob 模式的文件。返回相对于工作区根目录的匹配文件路径排序列表。示例：'**/*.rs'（所有 Rust 文件）、'src/**/mod.rs'（src 中所有的 mod.rs）。
tool-google-workspace = 通过 gws CLI 与 Google Workspace 服务（Drive、Gmail、Calendar、Sheets、Docs 等）交互。需要已安装并通过认证的 gws。
tool-hardware-board-info = 返回已连接硬件的完整开发板信息（芯片、架构、内存映射）。适用场景：用户询问 'board info'、'我有什么开发板'、'已连接硬件'、'芯片信息'、'什么硬件' 或 'memory map'。
tool-hardware-memory-map = 返回已连接硬件的内存映射（flash 和 RAM 地址范围）。适用场景：用户询问 '上下内存地址'、'memory map'、'地址空间' 或 '可读地址'。从数据手册返回 flash/RAM 范围。
tool-hardware-memory-read = 通过 USB 从 Nucleo 读取实际的内存/寄存器值。适用场景：用户要求 '读取寄存器值'、'读取某地址的内存'、'转储内存'、'lower memory 0-126' 或 '给出地址和值'。返回十六进制转储。需要通过 USB 连接的 Nucleo 和 probe 功能。参数：address（十六进制，例如 RAM 起始处为 0x20000000）、length（字节，默认 128）。
tool-http-request = 向外部 API 发起 HTTP 请求。支持 GET、POST、PUT、DELETE、PATCH、HEAD、OPTIONS 方法。安全约束：仅限允许列表中的域名，不允许本地/私有主机，可配置超时和响应大小限制。
tool-image-info = 读取图像文件元数据（格式、尺寸、大小），并可选择返回 base64 编码的数据。
tool-jira = 与 Jira 交互：读取工单、使用 JQL 搜索、添加评论、列出项目和每个问题的状态转换、推动问题在其工作流中转换状态，以及创建新问题。
tool-knowledge = 管理架构决策、解决方案模式、经验教训和专家的知识图谱。操作：capture、search、relate、suggest、expert_find、lessons_extract、graph_stats。
tool-linkedin = 管理 LinkedIn：创建帖子、列出你的帖子、评论、点赞、删除帖子、查看互动、获取个人资料信息，以及读取已配置的内容策略。需要 .env 文件中的 LINKEDIN_* 凭据。
tool-discord-search = 搜索存储在 discord.db 中的 Discord 消息历史。用于查找过往消息、总结频道活动或查看用户说过的话。支持关键词搜索和可选过滤器：channel_id、since、until。
tool-memory-forget = 按 key 移除一条记忆。用于删除过时的事实或敏感数据。返回该记忆是否被找到并移除。
tool-memory-recall = 在长期记忆中搜索相关的事实、偏好或上下文。返回按相关性排序的评分结果。省略查询或传入裸 * 以返回近期记忆。
tool-memory-store = 在长期记忆中存储事实、偏好或备注。使用类别 'core' 表示永久性事实，'daily' 表示会话备注，'conversation' 表示聊天上下文，或自定义类别名称。
tool-microsoft365 = Microsoft 365 集成：通过 Microsoft Graph API 管理 Outlook 邮件、Teams 消息、Calendar 事件、OneDrive 文件和 SharePoint 搜索
tool-model-routing-config = 管理默认模型设置、基于场景的提供商/模型路由、分类规则和别名 agent 配置
tool-notion = 与 Notion 交互：查询数据库、读取/创建/更新页面，以及搜索工作区。
tool-pdf-read = 从工作区中的 PDF 文件提取纯文本。返回所有可读文本。仅含图像或加密的 PDF 返回空结果。需要 'rag-pdf' 构建功能。
tool-project-intel = 项目交付智能：生成状态报告、检测风险、起草客户更新、总结冲刺，以及估算工作量。只读分析工具。
tool-proxy-config = 管理 ZeroClaw 代理设置（范围：environment | zeroclaw | services），包括运行时和进程环境变量应用
tool-pushover = 向你的设备发送 Pushover 通知。需要 .env 文件中的 PUSHOVER_TOKEN 和 PUSHOVER_USER_KEY。
tool-schedule = 管理仅限 shell 的定时任务。操作：create/add/once/list/get/cancel/remove/pause/resume。警告：此工具创建的 shell 任务输出仅被记录，不会投递到任何频道。要向 Discord/Telegram/Slack/Matrix 发送定时消息，请使用 cron_add 工具，并设置 job_type='agent' 和如 {"{"}"mode":"announce","channel":"discord","to":"<channel_id>"{"}"} 的 delivery 配置。
tool-screenshot = 捕获当前屏幕的截图。返回文件路径和 base64 编码的 PNG 数据。
tool-security-ops = 用于托管网络安全服务的安全运营工具。操作：triage_alert（对告警分类/排序）、run_playbook（执行事件响应步骤）、parse_vulnerability（解析扫描结果）、generate_report（创建安全态势报告）、list_playbooks（列出可用 playbook）、alert_stats（汇总告警指标）。
tool-shell = 在工作区目录中执行 shell 命令
tool-sop-advance = 报告当前 SOP 步骤的结果并推进到下一步。提供 run_id、步骤是成功还是失败，以及简要的输出摘要。
tool-sop-approve = 批准一个正在等待操作员审批的待处理 SOP 步骤。返回要执行的步骤指令。使用 sop_status 查看哪些运行正在等待。
tool-sop-execute = 按名称手动触发标准操作流程 (SOP)。返回运行 ID 和第一步指令。使用 sop_list 查看可用的 SOP。
tool-sop-list = 列出所有已加载的标准操作流程 (SOP) 及其触发器、优先级、步骤数和活动运行数。可选择按名称或优先级过滤。
tool-sop-status = 查询 SOP 执行状态。为特定运行提供 run_id，或使用 sop_name 列出该 SOP 的运行。不带参数时，显示所有活动运行。
tool-tool-search = 获取延迟加载的 MCP 工具的完整 schema 定义，以便调用它们。使用 "select:name1,name2" 进行精确匹配，或使用关键词进行搜索。
tool-web-fetch = 获取网页并将其内容以干净的纯文本形式返回。HTML 页面会自动转换为可读文本。JSON 和纯文本响应按原样返回。仅支持 GET 请求；会跟随重定向。安全性：仅限白名单域名，不允许本地/私有主机。
tool-web-search-tool = 在网络上搜索信息。返回包含标题、URL 和描述的相关搜索结果。使用此工具查找最新信息、新闻或研究主题。
tool-workspace = 管理多客户端工作区。子命令：list、switch、create、info、export。每个工作区提供隔离的内存、审计、密钥和工具限制。
tool-weather = 获取全球任意位置的当前天气状况和预报。支持城市名称（任意语言或文字）、IATA 机场代码（例如 'LAX'）、GPS 坐标（例如 '51.5,-0.1'）、邮政编码以及基于域名的地理定位。返回温度、体感温度、湿度、风速/风向、降水量、能见度、气压、紫外线指数和云量。可选 0-3 天的逐小时预报。单位默认使用公制（°C、km/h、mm），也可按请求设置为英制（°F、mph、英寸）。无需 API 密钥。
