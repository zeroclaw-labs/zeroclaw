---
id: context-mode-mcp-8382
stage: triage
deps: []
links: []
created: 2026-03-23T12:28:28Z
type: task
priority: 2
assignee: Dustin Reynolds
tags: [mcp]
version: 1
---
# context-mode MCP blocks curl/wget in SSH commands to remote hosts


The context-mode MCP hook blocks any Bash command containing 'curl' or 'wget' strings, even when those commands are executed on a remote host via SSH (e.g. ssh mintg708.local 'wget ...'). The hook pattern-matches the command text without distinguishing local vs remote execution context. This forced a workaround: writing a shell script locally, scp'ing it to the remote host, then executing it via ssh — since the script contents aren't in the Bash command string. Need to decide: (a) should context-mode allow curl/wget when wrapped in ssh, (b) should there be an allowlist for trusted remote hosts, (c) is a 'remote-execute' skill needed that bypasses the hook, or (d) should the hook only block local curl/wget. The current behavior is overly restrictive for legitimate remote administration tasks.
