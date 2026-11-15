tool-browser = 使用可插拔后端（agent-browser、rust-native、computer_use）进行网页/浏览器自动化。支持 DOM 操作以及通过 computer-use 边车进行的可选系统级操作（mouse_move、mouse_click、mouse_drag、key_type、key_press、screen_capture）。使用 'snapshot' 将交互元素映射到引用（@e1、@e2）。对 open 操作强制执行 browser.allowed_domains。
tool-browser-open = 在系统浏览器中打开经批准的 HTTPS URL。安全约束：仅限允许列表中的域名，不允许本地/私有主机，不允许抓取。
tool-content-search = 在工作区内按正则表达式模式搜索文件内容。支持 ripgrep (rg)，并以 grep 或内部搜索作为后备。输出模式：'content'（带上下文的匹配行）、'files_with_matches'（仅文件路径）、'count'（每个文件的匹配数量）。示例：pattern='fn main'，include='*.rs'，output_mode='content'。
tool-cron-add = 创建一个定时 cron 任务（shell 或 agent），支持 cron/at/every 调度。使用 job_type='agent' 并提供提示词以按计划运行 AI agent。要将输出投递到频道（Telegram 或已配置的网关 bridge），请设置 delivery={"{"}"mode":"announce","channel":"discord","to":"<channel_id_or_chat_id>"{"}"}。这是通过可用通道面向用户发送定时/延迟消息的首选工具。
tool-cron-list = 列出所有定时 cron 任务
tool-cron-remove = 按 id 移除一个 cron 任务
tool-cron-run = 立即强制运行一个 cron 任务并记录运行历史
tool-cron-runs = 列出某个 cron 任务的近期运行历史
tool-cron-update = 修补现有的 cron 任务（schedule、command、prompt、enabled、delivery、model 等）
tool-file-edit = 通过将精确匹配的字符串替换为新内容来编辑文件
tool-file-download = 从配置的远程端点下载文件并将其写入代理的工作区。提供要获取的文档标识符和工作区相对目标路径；端点 URL 由主机配置固定，永远不受模型控制。字节直接流式写入磁盘，不会加载到模型上下文中。返回 HTTP 状态、写入的字节数和目标路径。
tool-file-download-param-document-id = 要从配置的端点获取的文档标识符。
tool-file-download-param-dest-path = 用于写入文件的工作区相对路径。父目录必须已经存在。
tool-file-download-error-disabled = file_download 已禁用：未配置 [file_download].url
tool-file-download-error-read-only = 操作被阻止：自主权限为只读
tool-file-download-error-rate-limited-hour = 超出速率限制：过去一小时内操作过多
tool-file-download-error-rate-limited-budget = 超出速率限制：操作预算已耗尽
tool-file-download-error-missing-document-id = 缺少 'document_id' 参数
tool-file-download-error-missing-dest-path = 缺少 'dest_path' 参数
tool-file-download-error-invalid-file-name = 无效的 dest_path '{ $dest_path }'：必须以具体的文件名结尾
tool-file-download-error-no-parent = 无效的 dest_path '{ $dest_path }'：没有父目录
tool-file-download-error-resolve-dir = 无法解析 '{ $dest_path }' 的目标目录：{ $err }
tool-file-download-error-client-build = 构建下载客户端失败：{ $err }
tool-file-download-error-request = 下载请求失败：{ $err }
tool-file-download-error-status = 下载端点返回状态 { $status }
tool-file-download-error-too-large-reported = 下载文件过大：端点报告 { $len } 字节（限制：{ $limit } 字节）
tool-file-download-error-too-large-stream = 下载文件过大：超出 { $limit } 字节的限制
tool-file-download-error-temp-create = 创建临时下载文件失败：{ $err }
tool-file-download-error-read-body = 读取响应正文时失败：{ $err }
tool-file-download-error-write-body = 写入已下载字节时失败：{ $err }
tool-file-download-error-flush = 刷新已下载文件失败：{ $err }
tool-file-download-error-move = 将已下载文件移动到位失败：{ $err }
tool-file-download-success = 已下载 { $written } 字节到 { $dest_path }（{ $status }）
tool-file-read = 读取带行号的文件内容。支持通过 offset 和 limit 进行部分读取。二进制文件和图像文件会被拒绝（图像请使用 image_info 工具）。设置 encoding="base64" 可以返回经 base64 编码的原始字节（适用于 .pdf/.xlsx/.docx 等二进制文件）；该模式下 offset/limit 被忽略。
tool-file-write = 将内容写入工作区中的文件
tool-git-operations = 执行结构化的 Git 操作（status、diff、log、branch、commit、add、checkout、stash）。提供解析后的 JSON 输出，并与安全策略集成以实现自主控制。
tool-git-operations-error-not-in-repo = 当前路径 '{ $path }' 不在 Git 仓库中。请选择一个 Git 工作树内的路径，通过 'path' 参数指定仓库子目录，或在运行 git_operations 前初始化一个仓库。
tool-glob-search = 在工作区内搜索匹配 glob 模式的文件。返回相对于工作区根目录的匹配文件路径排序列表。示例：'**/*.rs'（所有 Rust 文件）、'src/**/mod.rs'（src 中所有的 mod.rs）。
tool-http-request = 向外部 API 发起 HTTP 请求。支持 GET、POST、PUT、DELETE、PATCH、HEAD、OPTIONS 方法。安全约束：仅限允许列表中的域名，不允许本地/私有主机，可配置超时和响应大小限制。
tool-image-info = 读取图像文件元数据（格式、尺寸、大小），并可选择返回 base64 编码的数据。
tool-knowledge = 管理架构决策、解决方案模式、经验教训、专家和关系链接的知识图谱。
tool-discord-search = 搜索存储在 discord.db 中的 Discord 消息历史。用于查找过往消息、总结频道活动或查看用户说过的话。支持关键词搜索和可选过滤器：channel_id、since、until。
tool-memory-forget = 按 key 移除一条记忆。用于删除过时的事实或敏感数据。返回该记忆是否被找到并移除。
tool-memory-recall = 在长期记忆中搜索相关的事实、偏好或上下文。返回按相关性排序的评分结果。省略查询或传入裸 * 以返回近期记忆。
tool-memory-store = 在长期记忆中存储事实、偏好或备注。使用类别 'core' 表示永久性事实，'daily' 表示会话备注，'conversation' 表示聊天上下文，或自定义类别名称。
tool-schedule = 管理仅限 shell 的定时任务。操作：create/add/once/list/get/cancel/remove/pause/resume。警告：此工具创建的 shell 任务输出仅被记录，不会投递到任何频道。要向 Telegram 或网关 bridge 发送定时消息，请使用 cron_add 工具，并设置 job_type='agent' 和如 {"{"}"mode":"announce","channel":"discord","to":"<channel_id>"{"}"} 的 delivery 配置。
tool-screenshot = 捕获当前屏幕的截图。返回文件路径和 base64 编码的 PNG 数据。
tool-shell = 在工作区目录中执行 shell 命令
tool-tool-search = 获取延迟加载的 MCP 工具的完整 schema 定义，以便调用它们。使用 "select:name1,name2" 进行精确匹配，或使用关键词进行搜索。
tool-web-fetch = 获取网页并将其内容以干净的纯文本形式返回。HTML 页面会自动转换为可读文本。JSON 和纯文本响应按原样返回。仅支持 GET 请求；会跟随重定向。安全性：仅限白名单域名，不允许本地/私有主机。
tool-web-search-tool = 在网络上搜索信息。返回包含标题、URL 和描述的相关搜索结果。使用此工具查找最新信息、新闻或研究主题。
tool-workspace = 管理多客户端工作区。子命令：list、switch、create、info、export。每个工作区提供隔离的内存、审计、密钥和工具限制。

tool-browser-screenshot-error-args-not-object = 截图参数必须是 JSON 对象
tool-browser-screenshot-error-computeruse-non-string-path = 截图的 'path' 参数必须是字符串，实际为 { $path }
tool-browser-screenshot-error-missing-filename = 截图路径 '{ $path }' 缺少文件名部分
tool-browser-screenshot-error-non-string-path = 截图的 'path' 必须是字符串或省略
tool-browser-screenshot-error-parent-not-exist = 截图路径 '{ $path }' 的父目录 '{ $parent }' 不存在
tool-browser-screenshot-error-path-not-allowed = 截图路径 '{ $path }' 不在工作区允许列表中
tool-browser-screenshot-error-path-not-utf8 = 截图路径 '{ $path }' 解析为非 UTF-8 路径；拒绝通过有损转换写入
tool-browser-screenshot-error-path-outside-workspace = 截图路径 '{ $path }' 解析为 '{ $canonical }'，位于工作区之外
tool-browser-screenshot-error-runtime-config-target = 无法将截图写入运行时配置路径 '{ $target }'
tool-browser-screenshot-error-sidecar-empty-png = computer-use 边车返回了空的截图载荷
tool-browser-screenshot-error-sidecar-no-png-data = computer-use 边车未返回 PNG 数据
tool-browser-screenshot-error-sidecar-non-json-success = 对于带路径的截图，computer-use 边车返回了非 JSON 的成功响应；请求的文件未被写入
tool-browser-screenshot-error-sidecar-not-png = computer-use 边车返回了非 PNG 的截图载荷
tool-browser-screenshot-error-symlink-target = 无法将截图写入符号链接目标 '{ $target }'

# MCP 延迟发现与不回显输入值的参数恢复。
tool-mcp-deferred-instructions = 以下工具可用，但完整 schema 尚未加载。需要 schema 时，用 tool_search 的 "select:name1,name2" 或关键词查询。已知参数时也可直接调用列表中的精确名称；运行时在常规 hooks 和审批通过后激活工具。发现工具不会授予未列出或被拒绝工具的权限。
tool-mcp-search-query-required = 必须提供 query 参数
tool-mcp-search-no-matches = 没有找到匹配的延迟工具。
tool-mcp-search-not-found = 未找到：{ $names }
tool-mcp-arguments-invalid = MCP 参数无效。{ $problem } 预期字段和类型：{ $signature }。如需完整 schema，请用 tool_search 获取后修正参数。此提示不回显输入值。
tool-mcp-arguments-server-rejected = MCP 服务端拒绝了这些参数。
tool-mcp-arguments-root-type = 参数必须为 { $expected } 类型，实际为 { $actual }。
tool-mcp-arguments-missing = 缺少必填字段 { $field }。
tool-mcp-arguments-field-type = 字段 { $field } 应为 { $expected } 类型，实际为 { $actual }。
