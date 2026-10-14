#!/usr/bin/env bash
# Minimal stdio MCP server test double. Writes its own PID to the path
# given as $1, then answers "initialize" and "tools/list" JSON-RPC
# requests read from stdin. Checked in with the executable bit set so
# tests can symlink to it rather than writing and chmodding a fresh
# script inside an already-multithreaded test process.

echo $$ > "$1"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"reconnect-test","version":"0.1.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}'
      ;;
  esac
done
