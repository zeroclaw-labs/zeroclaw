#!/bin/sh
set -eu
IFS= read -r mode < mode
IFS= read -r request
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"cached_token"}]}}'
IFS= read -r request
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
IFS= read -r request
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"capture-session"}}'
IFS= read -r request
printf '%s\n' '{"method":"session/update","params":{"sessionId":"foreign-session","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"FOREIGN_THOUGHT"}}}}'
printf '%s\n' '{"method":"session/update","params":{"sessionId":"capture-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"progress"}}}}'
printf '%s\n' '{"method":"session/update","params":{"sessionId":"capture-session","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"ACP_THOUGHT"}}}}'
printf '%s\n' '{"method":"session/update","params":{"sessionId":"capture-session","update":{"sessionUpdate":"plan","entries":[{"content":"plan item","status":"in_progress"}],"token":"fixture-secret"}}}'
printf '%s\n' '{"method":"session/update","params":{"sessionId":"capture-session","update":{"sessionUpdate":"tool_call","toolCallId":"native-call","title":"lookup","status":"in_progress"}}}'
if [ "$mode" = crash ]; then
    exit 1
fi
if [ "$mode" = remote_error ]; then
    printf '%s\n' '{"jsonrpc":"2.0","id":4,"error":{"code":-32042,"message":"REMOTE_ERROR_DETAIL","data":{"reason":"quota exceeded","request_id":"audit-request-42","api_key":"fixture-secret"}}}'
    while IFS= read -r request; do :; done
    exit 0
fi
printf '%s\n' '{"jsonrpc":"2.0","id":50,"method":"session/request_permission","params":{"sessionId":"capture-session","toolCall":{"toolCallId":"native-call"},"options":[{"optionId":"reject","kind":"reject_once"},{"optionId":"allow","kind":"allow_once"}]}}'
IFS= read -r reply
printf '%s\n' "$reply" > permission-reply.json
printf '%s\n' '{"method":"session/update","params":{"sessionId":"capture-session","update":{"sessionUpdate":"tool_call_update","toolCallId":"native-call","status":"completed"}}}'
printf '%s\n' '{"method":"session/update","params":{"sessionId":"capture-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"image","data":"cG5n","mimeType":"image/png"}}}}'
printf '%s\n' '{"method":"session/update","params":{"sessionId":"capture-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ACP_FINAL"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}'
while IFS= read -r request; do :; done
