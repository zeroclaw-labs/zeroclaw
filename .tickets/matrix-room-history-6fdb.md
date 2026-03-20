---
id: matrix-room-history-6fdb
stage: triage
deps: []
links: []
created: 2026-03-20T13:58:37Z
type: task
priority: 2
assignee: Dustin Reynolds
tags: [matrix, tools, channels]
version: 1
---
# Matrix room history not accessible to LLM agent


The LLM agent cannot read Matrix room message history when asked to review a room. Three blockers:

1. Access token encrypted: config stores it as enc2:... — cannot be used directly against the Matrix CS API.
2. No gateway proxy: no /api/matrix/messages endpoint exists in the ZeroClaw gateway to let the agent pull room history without decrypting the token itself.
3. Daemon isolation: running the CLI binary in a workspace does not share the daemon's decrypted secret store.

Proposed fix — expose a Matrix room history tool or gateway endpoint:
- Add a gateway route GET /api/matrix/rooms/{roomId}/messages?limit=N that the daemon (which already holds the decrypted token) uses to fetch and return messages from the Matrix CS API.
- Register a tool (matrix_history) that the agent can call during a conversation, returning recent messages for a given room ID.
- The tool/endpoint should filter to allowed rooms from config and respect the existing allowed_users list.
- Token decryption happens inside the daemon — the agent never sees the raw token.

This would let the agent answer questions like 'what happened in the habla-spanish room today' without requiring the user to manually copy-paste room history.
