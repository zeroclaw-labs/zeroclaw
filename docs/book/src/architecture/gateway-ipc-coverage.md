# Gateway route coverage

The gateway is moving out of the daemon: each HTTP, WebSocket and SSE route it serves will reach the core over the local RPC socket instead of calling into the runtime directly ([RPC socket transport](./rpc-socket.md)). This page classifies every route the gateway registers today by how the split serves it. The architecture test `tests/architecture/gateway_route_coverage.rs` holds the same table. It parses the gateway's source for every route, fallback and nested router, and fails the build when one cannot be resolved, when the gateway registers a route the table does not classify, when a classified route is no longer registered, when an `rpc` entry names a method the core does not serve, or when this page's table or counts differ from the test's in any cell. It is a tripwire for ordinary changes to the gateway's router, not a defence against code written to get past it: it refuses what it cannot resolve, but it does not expand macros or follow a value further than a `let` in the same function.

The classification follows the coverage table of the core-to-gateway IPC contract (pull request #11300), which also records, route by route, what a partial match is missing. That contract is still under review; until it merges, this page is the checked copy.

## Classes

| Class | Meaning |
|---|---|
| `rpc` | Core methods on `master` serve the route. The match may be partial: a missing input or output field, or two calls where the route makes one. |
| `deferred` | No method on `master` serves it yet: the method exists only in an open pull request (`pending`), is proposed by the contract (`proposed`), or waits on a contract decision (`decision`). |
| `ingress` | Inbound webhook and channel traffic. The core verifies it (contract §12); the gateway only relays it. |
| `gateway-local` | The gateway serves it alone: static assets, the OpenAPI document, its own shutdown, unsupported-method answers. |

A route whose method lands on `master` moves from `deferred` to `rpc` in both this page and the test.

## Counts

| Class | Routes |
|---|---:|
| `rpc` | 78 |
| `deferred` | 74 |
| `ingress` | 13 |
| `gateway-local` | 10 |

Routes are counted per HTTP method: `GET /api/cron` and `POST /api/cron` are two rows. `ANY` is every method: `any(..)`, or a method router's fallback. WebSocket and server-sent-event routes are `GET`. A path in angle brackets is decided at runtime: `<unmatched>` is the router's fallback, which serves the dashboard for any path no route matches, and `<prefix>/` is the redirect added when `gateway.path_prefix` is set.

The contract's table counts differently in one place: it gives the plugin route's `HEAD` answer and its fallback one row, which this page splits into `HEAD` and `ANY`. This page therefore has one row more than the contract's table.

## Routes

| Route | Class | Core methods, or why none yet |
|---|---|---|
| POST `/webhook` | ingress | core-verified ingress (contract §12) |
| GET `/ws/chat` | rpc | `session/new`, `session/prompt`, `session/approve`, `session/cancel`, `sops/decide` |
| GET `/api/sessions` | rpc | `session/list` |
| GET `/api/sessions/running` | deferred | pending #11132: session/list |
| GET `/api/sessions/{id}/messages` | rpc | `session/messages` |
| POST `/api/sessions/{id}/messages` | deferred | pending #11132: session/append |
| DELETE `/api/sessions/{id}` | rpc | `session/delete` |
| PUT `/api/sessions/{id}` | deferred | pending #11132: session/rename |
| GET `/api/sessions/{id}/state` | rpc | `session/state` |
| POST `/api/sessions/{id}/abort` | deferred | pending #11132: session/abort |
| GET `/api/memory` | rpc | `memory/list`, `memory/search` |
| POST `/api/memory` | rpc | `memory/store` |
| DELETE `/api/memory/{key}` | rpc | `memory/delete` |
| GET `/api/cron` | rpc | `cron/list` |
| POST `/api/cron` | rpc | `cron/add` |
| GET `/api/cron/settings` | rpc | `cron/settings` |
| PATCH `/api/cron/settings` | rpc | `config/set-many`, `cron/settings` |
| DELETE `/api/cron/{id}` | rpc | `cron/delete` |
| PATCH `/api/cron/{id}` | rpc | `cron/patch` |
| GET `/api/cron/{id}/runs` | rpc | `cron/runs` |
| POST `/api/cron/{id}/run` | rpc | `cron/trigger` |
| GET `/api/config` | rpc | `config/get` |
| PATCH `/api/config` | rpc | `config/set-many` |
| OPTIONS `/api/config` | deferred | proposed: config/schema |
| GET `/api/config/prop` | rpc | `config/get` |
| PUT `/api/config/prop` | rpc | `config/set` |
| DELETE `/api/config/prop` | rpc | `config/delete` |
| OPTIONS `/api/config/prop` | deferred | proposed: config/schema |
| GET `/api/config/list` | rpc | `config/list` |
| GET `/api/config/drift` | deferred | pending #11172: config/drift |
| GET `/api/config/reload-status` | deferred | pending #11172: config/reload-status |
| GET `/api/config/templates` | rpc | `config/templates` |
| GET `/api/config/map-keys` | rpc | `config/map-keys` |
| GET `/api/config/resolve-alias-source` | rpc | `config/resolve-alias-source` |
| POST `/api/config/map-key` | rpc | `config/map-key-create` |
| DELETE `/api/config/map-key` | rpc | `config/map-key-delete` |
| POST `/api/config/rename-map-key` | rpc | `config/map-key-rename` |
| POST `/api/config/model-providers/{type}/{alias}/refresh-context-window` | deferred | pending #11172: providers/refresh-context-window |
| GET `/api/config/delete-plan` | deferred | pending #11172: config/delete-plan |
| GET `/api/config/status` | rpc | `config/status` |
| GET `/api/config/agent-options` | deferred | pending #11172: config/agent-options |
| GET `/api/config/sections` | rpc | `config/sections` |
| GET `/api/config/sections/{section}` | deferred | pending #11172: config/section-picker |
| POST `/api/config/sections/{section}/items/{key}` | deferred | pending #11172: config/section-select |
| POST `/api/config/init` | deferred | pending #11172: config/init |
| POST `/api/config/migrate` | deferred | pending #11172: config/migrate |
| GET `/api/quickstart/state` | rpc | `quickstart/state` |
| POST `/api/quickstart/fields` | rpc | `quickstart/fields` |
| POST `/api/quickstart/validate` | rpc | `quickstart/validate` |
| POST `/api/quickstart/apply` | rpc | `quickstart/apply` |
| POST `/api/quickstart/dismiss` | rpc | `quickstart/dismiss` |
| GET `/api/config/catalog` | rpc | `config/catalog` |
| GET `/api/config/catalog/models` | rpc | `config/catalog-models` |
| GET `/api/tools` | deferred | pending #11182: tools/list |
| POST `/api/tools/param-options` | rpc | `tools/param-options` |
| GET `/api/integrations` | deferred | pending #11182: integrations/list |
| GET `/api/integrations/settings` | deferred | pending #11182: integrations/list |
| GET `/api/cli-tools` | deferred | pending #11182: tools/cli-discover |
| GET `/admin/sop/pending` | rpc | `sops/runs` |
| GET `/admin/sop/logs` | rpc | `logs/query` |
| POST `/admin/sop/approve` | rpc | `sops/decide` |
| POST `/admin/sop/deny` | rpc | `sops/decide` |
| POST `/sop/{*rest}` | ingress | core-verified ingress (contract §12) |
| GET `/api/sops` | rpc | `sops/list` |
| POST `/api/sops` | rpc | `sops/create` |
| PUT `/api/sops/{name}` | rpc | `sops/save` |
| DELETE `/api/sops/{name}` | rpc | `sops/delete` |
| GET `/api/sops/{name}/graph` | rpc | `sops/graph` |
| POST `/api/sops/{name}/run` | rpc | `sops/run` |
| POST `/api/sops/{name}/rename` | rpc | `sops/rename` |
| GET `/api/sops/runs` | rpc | `sops/runs` |
| GET `/api/sops/{name}/full` | rpc | `sops/get` |
| POST `/api/sops/wire-draft` | rpc | `sops/wire-draft` |
| POST `/api/sops/graph-draft` | rpc | `sops/graph-draft` |
| GET `/api/sops/trigger-sources` | rpc | `sops/trigger-sources` |
| GET `/api/sops/decision-models` | deferred | pending #11169: sops/decision-models |
| GET `/api/sops/graph-legend` | deferred | pending #11169: sops/graph-legend |
| GET `/api/sops/{name}/runs/{run_id}/overlay` | rpc | `sops/run-overlay` |
| POST `/api/sops/{name}/runs/{run_id}/decide` | rpc | `sops/decide` |
| POST `/api/sops/{name}/runs/{run_id}/cancel` | deferred | pending #11169: sops/cancel |
| GET `/ws/sops/runs` | deferred | proposed: sops/subscribe-runs, sops/run-changed |
| GET `/api/agents/{alias}/skills` | deferred | pending #11176: skills/effective |
| GET `/api/skills/bundles` | rpc | `skills/bundles` |
| GET `/api/skills/slash-option-kinds` | deferred | pending #11176: skills/slash-option-kinds |
| GET `/api/skills/bundles/{alias}/skills` | rpc | `skills/list` |
| POST `/api/skills/bundles/{alias}/skills` | deferred | pending #11176: skills/create |
| GET `/api/skills/bundles/{alias}/skills/{name}` | rpc | `skills/read` |
| PUT `/api/skills/bundles/{alias}/skills/{name}` | rpc | `skills/write` |
| DELETE `/api/skills/bundles/{alias}/skills/{name}` | rpc | `skills/delete` |
| GET `/api/personality` | rpc | `personality/list` |
| GET `/api/personality/templates` | rpc | `personality/templates` |
| GET `/api/personality/{filename}` | rpc | `personality/get` |
| PUT `/api/personality/{filename}` | rpc | `personality/put` |
| GET `/api/browse` | rpc | `fs/list_dir` |
| POST `/api/browse/mkdir` | deferred | pending #11182: fs/mkdir |
| DELETE `/api/browse/rmdir` | deferred | pending #11182: fs/rmdir |
| GET `/api/agents/{alias}/workspace/list` | deferred | pending #11182: workspace/list |
| GET `/api/agents/{alias}/workspace/read` | deferred | pending #11182: fs/read |
| DELETE `/api/agents/{alias}/workspace/path` | deferred | pending #11182: fs/delete |
| POST `/api/agents/{alias}/workspace/move` | deferred | pending #11182: fs/move |
| POST `/api/agents/{alias}/workspace/mkdir` | deferred | pending #11182: fs/mkdir |
| POST `/api/upload` | rpc | `file/upload/begin`, `file/upload/chunk`, `file/upload/commit`, `file/attach` |
| GET `/api/logs` | rpc | `logs/query` |
| GET `/api/events` | rpc | `events/subscribe`, `logs/subscribe` |
| GET `/api/events/history` | rpc | `events/history` |
| GET `/api/cost` | rpc | `cost/query` |
| GET `/api/status` | rpc | `status`, `agents/status` |
| GET `/api/tuis` | rpc | `tui/list` |
| GET `/health` | rpc | `health` |
| GET `/metrics` | deferred | pending #11182: metrics/scrape |
| POST `/admin/shutdown` | gateway-local | served by the gateway itself |
| POST `/admin/reload` | rpc | `config/reload` |
| GET `/api/health` | rpc | `health` |
| GET `/api/version/check` | deferred | proposed: system/version-check |
| POST `/api/version/upgrade` | deferred | pending #11182: system/upgrade |
| GET `/api/version/upgrade/status` | deferred | pending #11182: system/upgrade-status |
| GET `/api/doctor` | rpc | `doctor/run` |
| POST `/api/doctor` | rpc | `doctor/run` |
| GET `/admin/paircode` | deferred | proposed: pairing/code |
| POST `/admin/paircode/new` | deferred | pending #11182: pairing/new-code, pairing/revoke |
| POST `/pair` | deferred | proposed: pairing/redeem |
| GET `/pair/code` | deferred | proposed: PairingPosture.require_pairing |
| POST `/api/pairing/initiate` | deferred | pending #11182: pairing/new-code |
| POST `/api/pair` | deferred | proposed: pairing/redeem |
| GET `/api/devices` | deferred | pending #11182: pairing/list |
| POST `/api/devices/me/capabilities` | deferred | proposed: pairing/device-capabilities |
| DELETE `/api/devices/{id}` | deferred | pending #11182: pairing/revoke |
| POST `/api/devices/{id}/token/rotate` | deferred | pending #11182: pairing/revoke, pairing/new-code |
| GET `/api/oidc/providers` | deferred | decision D10: OIDC relay |
| POST `/api/oidc/{alias}/device/start` | deferred | decision D10: OIDC relay |
| POST `/api/oidc/{alias}/device/poll` | deferred | decision D10: OIDC relay |
| GET `/oidc/login/{alias}` | deferred | decision D10: OIDC relay |
| GET `/oidc/callback` | deferred | decision D10: OIDC relay |
| POST `/api/webauthn/register/start` | deferred | decision D10: WebAuthn |
| POST `/api/webauthn/register/finish` | deferred | decision D10: WebAuthn |
| POST `/api/webauthn/auth/start` | deferred | decision D10: WebAuthn |
| POST `/api/webauthn/auth/finish` | deferred | decision D10: WebAuthn |
| GET `/api/webauthn/credentials` | deferred | decision D10: WebAuthn |
| DELETE `/api/webauthn/credentials/{id}` | deferred | decision D10: WebAuthn |
| GET `/whatsapp` | ingress | core-verified ingress (contract §12) |
| POST `/whatsapp` | ingress | core-verified ingress (contract §12) |
| GET `/whatsapp/{alias}` | ingress | core-verified ingress (contract §12) |
| POST `/whatsapp/{alias}` | ingress | core-verified ingress (contract §12) |
| POST `/linq` | ingress | core-verified ingress (contract §12) |
| POST `/linq/{alias}` | ingress | core-verified ingress (contract §12) |
| POST `/nextcloud-talk` | ingress | core-verified ingress (contract §12) |
| POST `/nextcloud-talk/{alias}` | ingress | core-verified ingress (contract §12) |
| POST `/webhook/gmail` | ingress | core-verified ingress (contract §12) |
| GET `/api/channels` | deferred | pending #11182: channels/list |
| POST `/api/channels/{channel}/relink` | deferred | pending #11182: channels/relink |
| POST `/api/channels/bind` | deferred | pending #11182: channels/bind |
| GET `/api/plugins` | deferred | pending #11182: plugins/list |
| GET `/plugin/{path}` | ingress | core-verified ingress (contract §12) |
| POST `/plugin/{path}` | ingress | core-verified ingress (contract §12) |
| GET `/.well-known/agents-card.json` | deferred | pending #11182: a2a/identity |
| GET `/a2a/.well-known/agents-card.json` | deferred | pending #11182: a2a/identity |
| GET `/a2a/{alias}/.well-known/agent-card.json` | deferred | pending #11182: a2a/identity |
| POST `/a2a/{alias}` | deferred | pending #11132: session/run-once |
| GET `/acp` | deferred | decision D10: ACP |
| GET `/ws/nodes` | deferred | decision D10: nodes |
| GET `/api/canvas` | deferred | pending #11182: canvas/list |
| GET `/api/canvas/{id}` | deferred | pending #11182: canvas/get |
| POST `/api/canvas/{id}` | deferred | pending #11182: canvas/render |
| DELETE `/api/canvas/{id}` | deferred | pending #11182: canvas/clear |
| GET `/api/canvas/{id}/history` | deferred | pending #11182: canvas/history |
| GET `/ws/canvas/{id}` | deferred | proposed: canvas/subscribe, canvas/frame |
| GET `/_app/` | gateway-local | served by the gateway itself |
| GET `/_app/{*path}` | gateway-local | served by the gateway itself |
| GET `/api/openapi.json` | gateway-local | served by the gateway itself |
| GET `/api/docs` | gateway-local | served by the gateway itself |
| POST `/hooks/claude-code` | gateway-local | served by the gateway itself |
| HEAD `/plugin/{path}` | gateway-local | 405 for unsupported methods |
| ANY `/plugin/{path}` | gateway-local | 405 for unsupported methods |
| GET `<unmatched>` | gateway-local | SPA fallback: the dashboard for unmatched paths |
| GET `<prefix>/` | gateway-local | redirect from the configured `gateway.path_prefix` |

## Updating this page

When you add, remove or move a gateway route, change its row here and its entry in `ROUTES` in `tests/architecture/gateway_route_coverage.rs` together. When an open pull request's method reaches `master`, promote the routes it serves from `deferred` to `rpc`.
