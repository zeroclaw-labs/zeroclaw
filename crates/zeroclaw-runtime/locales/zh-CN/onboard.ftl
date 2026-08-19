# 引导流程 — 与界面无关的字符串。
#
# 提供给所有界面（CLI、RPC、web）。流程将消息 ID 和参数作为数据传递；
# 使用它的界面根据当前区域设置进行解析。

# 语言选择器 — 流程的第一步。
onboard-flow-locale-prompt = 选择语言
onboard-flow-locale-confirmed = 语言已设置为 {$label}。

# 章节遍历结果。
onboard-flow-completed = 已配置 {$items}。
onboard-flow-cancelled = 已取消引导。未做任何更改。
onboard-flow-failed = 无法配置 {$layer}:{$instance}：{$reason}

# 错误。
onboard-flow-no-fields = 章节 {$section} 没有可配置的字段。
