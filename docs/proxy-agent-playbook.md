# Proxy Agent Playbook

This playbook provides copy-paste tool calls for configuring proxy behavior via `proxy_config`.

Use this document when you want the agent to switch proxy scope quickly and safely.

## 0. Summary

- **Purpose:** provide copy-ready agent tool calls for proxy scope management and rollback.
- **Audience:** operators and maintainers running ZeroClaw in proxied networks.
- **Scope:** `proxy_config` actions, mode selection, verification flow, and troubleshooting.
- **Non-goals:** generic network debugging outside ZeroClaw runtime behavior.

---

## 1. Fast Path by Intent

Use this section for quick operational routing.

### 1.1 Proxy only ZeroClaw internal traffic

1. Use scope `zeroclaw`.
2. Set `http_proxy`/`https_proxy` or `all_proxy`.
3. Validate with `{"action":"get"}`.

Go to:

- [Section 4](#4-mode-a--proxy-only-for-zeroclaw-internals)

### 1.2 Proxy only selected services

1. Use scope `services`.
2. Set concrete keys or wildcard selectors in `services`.
3. Validate coverage using `{"action":"list_services"}`.

Go to:

- [Section 5](#5-mode-b--proxy-only-for-specific-services)

### 1.3 Export process-wide proxy environment variables

1. Use scope `environment`.
2. Apply with `{"action":"apply_env"}`.
3. Verify env snapshot via `{"action":"get"}`.

Go to:

- [Section 6](#6-mode-c--proxy-for-full-process-environment)

### 1.4 Emergency rollback

1. Disable proxy.
2. If needed, clear env exports.
3. Re-check runtime and environment snapshots.

Go to:

- [Section 7](#7-disable--rollback-patterns)

---

## 2. Scope Decision Matrix

| Scope | Affects | Exports env vars | Typical use |
|---|---|---|---|
| `zeroclaw` | ZeroClaw internal HTTP clients | No | Normal runtime proxying without process-level side effects |
| `services` | Only selected service keys/selectors | No | Fine-grained routing for specific providers/tools/channels |
| `environment` | Runtime + process environment proxy variables | Yes | Integrations that require `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` |

---

## 3. Standard Safe Workflow

Use this sequence for every proxy change:

1. Inspect current state.
2. Discover valid service keys/selectors.
3. Apply target scope configuration.
4. Verify runtime and environment snapshots.
5. Roll back if behavior is not expected.

Tool calls:

```json
{"action":"get"}
{"action":"list_services"}
```

---

## 4. Mode A — Proxy Only for ZeroClaw Internals

Use when ZeroClaw provider/channel/tool HTTP traffic should use proxy, without exporting process-level proxy env vars.

Tool calls:

```json
{"action":"set","enabled":true,"scope":"zeroclaw","http_proxy":"http://127.0.0.1:7890","https_proxy":"http://127.0.0.1:7890","no_proxy":["localhost","127.0.0.1"]}
{"action":"get"}
```

Expected behavior:

- Runtime proxy is active for ZeroClaw HTTP clients.
- `HTTP_PROXY` / `HTTPS_PROXY` process env exports are not required.

---

## 5. Mode B — Proxy Only for Specific Services

Use when only part of the system should use proxy (for example specific providers/tools/channels).

### 5.1 Target specific services

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.openai","tool.multimodal","tool.http_request","channel.telegram"],"all_proxy":"socks5h://127.0.0.1:1080","no_proxy":["localhost","127.0.0.1",".internal"]}
{"action":"get"}
```

### 5.2 Target by selectors

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.*","tool.*","channel.qq"],"http_proxy":"http://127.0.0.1:7890"}
{"action":"get"}
```

Expected behavior:

- Only matched services use proxy.
- Unmatched services bypass proxy.

---

## 6. Mode C — Proxy for Full Process Environment

Use when you intentionally need exported process env vars (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`) for runtime integrations.

### 6.1 Configure and apply environment scope

```json
{"action":"set","enabled":true,"scope":"environment","http_proxy":"http://127.0.0.1:7890","https_proxy":"http://127.0.0.1:7890","no_proxy":"localhost,127.0.0.1,.internal"}
{"action":"apply_env"}
{"action":"get"}
```

Expected behavior:

- Runtime proxy is active.
- Environment variables are exported for the process.

---

## 7. Disable / Rollback Patterns

### 7.1 Disable proxy (default safe behavior)

```json
{"action":"disable"}
{"action":"get"}
```

### 7.2 Disable proxy and force-clear env vars

```json
{"action":"disable","clear_env":true}
{"action":"get"}
```

### 7.3 Keep proxy enabled but clear environment exports only

```json
{"action":"clear_env"}
{"action":"get"}
```

---

## 8. Common Operation Recipes

### 8.1 Switch from environment-wide proxy to service-only proxy

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.openai","tool.http_request"],"all_proxy":"socks5://127.0.0.1:1080"}
{"action":"get"}
```

### 8.2 Add one more proxied service

```json
{"action":"set","scope":"services","services":["provider.openai","tool.http_request","channel.slack"]}
{"action":"get"}
```

### 8.3 Reset `services` list with selectors

```json
{"action":"set","scope":"services","services":["provider.*","channel.telegram"]}
{"action":"get"}
```

---

## 9. Troubleshooting

- Error: `proxy.scope='services' requires a non-empty proxy.services list`
  - Fix: set at least one concrete service key or selector.

- Error: invalid proxy URL scheme
  - Allowed schemes: `http`, `https`, `socks5`, `socks5h`.

- Proxy does not apply as expected
  - Run `{"action":"list_services"}` and verify service names/selectors.
  - Run `{"action":"get"}` and check `runtime_proxy` and `environment` snapshot values.

---

## 10. Related Docs

- [README.md](./README.md) — Documentation index and taxonomy.
- [network-deployment.md](./network-deployment.md) — end-to-end network deployment and tunnel topology guidance.
- [resource-limits.md](./resource-limits.md) — runtime safety limits for network/tool execution contexts.

---

## 11. Maintenance Notes

- **Owner:** runtime and tooling maintainers.
- **Update trigger:** new `proxy_config` actions, proxy scope semantics, or supported service selector changes.
- **Last reviewed:** 2026-02-18.
