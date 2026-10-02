# MCP memory soak diagnostic

This opt-in harness drives the actual gateway, provider adapter and MCP transport
with synthetic data. It addresses the Linux/glibc evidence request in issue #8642;
it is not a production fix or a claim that the original growth is reproduced.
No production dependencies or runtime source files are changed.

## Isolation and requirements

Use Python 3.11+ and `websocket-client` (Debian `python3-websocket`) in a disposable
fixture container. The mock and analyzer otherwise use only the standard library.
Do not use real API credentials, a user configuration, a user home mount, public
ports or external egress. Do not override HOME or CODEX_HOME. Run containers on a
dedicated Docker **internal** network; share the runtime container's PID namespace
with the driver, not its process memory. The runtime must use Linux/glibc; a macOS
RSS run does not test the allocator concern in this issue.

The mock listens on loopback by default. `--host 0.0.0.0` is appropriate only in
the isolated mock container without a published port. The fixture intentionally
has no authentication and is unsuitable for other deployment.

## Workload and runtime configuration

Use a fresh mock and runtime data directory for each run. Configure:

- An explicit agent named `soak`, gateway WebSocket enabled, and pairing disabled
  **only in this isolated, unpublished test environment**.
- A custom OpenAI-compatible provider pointing at `http://mock:9100/v1`, model
  `soak-mock`; any required API-key placeholder must be synthetic.
- Three eagerly discovered HTTP MCP servers named `soak0`, `soak1`, `soak2`, URLs
  `http://mock:9100/mcp/0`, `/mcp/1`, `/mcp/2`. Each advertises 12 harmless tools.
- Set `max_history_messages = 16`, tool-iteration cap **26**, sufficiently high
  synthetic action budget, memory backend `none`, session persistence disabled,
  and sufficient context capacity to avoid unrelated compaction. The historical
  runtime preserves whole turns, so this is a configured trimming target, not a
  hard bound of 16 messages: a tool-heavy active turn can exceed it. Keep the
  same setting and history policy in both runs.

Every tool has a 3,072-byte input schema using Python's default JSON serialization
(compact wire encoding is slightly smaller). The synthetic schema has three
simple properties and reaches that size by padding a description. It does not
represent the allocation shape of every deeply nested real-world MCP schema;
report this shape as well as the serialized size. One turn makes **25 sequential real
MCP calls**, then a 26th provider request yields the final answer. The iteration
cap therefore needs to be 26, not 25. Names cycle deterministically across all 36
tools; arguments and call IDs are unique per turn/step. The mock refuses to advance
the provider until its requested MCP call actually arrives with the right arguments.

The 36 actual MCP tool definitions in provider requests are canonicalized, hashed,
and counted in bytes, excluding built-ins. Any within-run payload drift fails the
fixture; the analyzer also refuses unmatched before/after hashes. Counters and one
pending call occupy constant space. Request bodies are limited to 4 MiB; no request
or conversation history is recorded in mock memory. HTTP connections time out in
10 seconds and driver HTTP requests in 5 seconds.
The stdlib mock uses HTTP/1.0 with connection closure, identically for both runs;
this does not reproduce every original server's keep-alive behavior.

## Diagnostic container builds

`Dockerfile.runtime` uses the historical repository's `ci` profile, native
Linux arm64, and only the `agent-runtime,gateway` features. This uses thin LTO
instead of the default release profile's fat LTO, which exceeded an 8 GiB build
environment during setup. Both comparison images must use the same profile and
features; results do not establish behavior of every distributed release build.
Compiler and Debian base images are pinned by digest. Target caches are separate
for each source revision.

From the repository root, with the two historical commits available:

```sh
SOAK_HARNESS="$PWD/tests/manual/mcp-memory-soak"
git worktree add --detach ../zeroclaw-soak-before c7f4435b95103b645a5e80585b3b2af412b0cd33
git worktree add --detach ../zeroclaw-soak-after 5bf683defcd0fd9237eb640b6425e9b978c8dd20
docker build --platform linux/arm64 -f "$SOAK_HARNESS/Dockerfile.runtime" \
  --build-arg SOAK_REVISION=c7f4435b95103b645a5e80585b3b2af412b0cd33 \
  -t zeroclaw-soak:before ../zeroclaw-soak-before
docker build --platform linux/arm64 -f "$SOAK_HARNESS/Dockerfile.runtime" \
  --build-arg SOAK_REVISION=5bf683defcd0fd9237eb640b6425e9b978c8dd20 \
  -t zeroclaw-soak:after ../zeroclaw-soak-after
docker build -f "$SOAK_HARNESS/Dockerfile.fixtures" \
  -t zeroclaw-soak:fixtures "$SOAK_HARNESS"
```

The example `config.toml` is synthetic and only suitable for the isolated network
described above. Copy it into a fresh runtime container at `/soak/config/config.toml`
and start `zeroclaw --config-dir /soak/config gateway`. Do not mount a real config
or expose its unauthenticated gateway. The runtime image sets `MALLOC_ARENA_MAX=2`
as requested by the issue's published recipe; report that allocator cap explicitly.

## Commands

Paths below are relative to this directory inside the isolated fixture container.
Container lifecycle is deliberately outside these scripts; record the exact
isolation, resource-limit and mount commands alongside the resulting data.

```sh
python3 mock.py --host 0.0.0.0 --port 9100

# Separate driver container, --pid=container:<runtime>; runtime executable is PID 1.
python3 soak.py --ws-url 'ws://runtime:42617/ws/chat?agent=soak' \
  --mock-url http://mock:9100 --pid 1 --revision FULL_COMMIT_SHA \
  --output /artifacts/before.jsonl \
  --duration-seconds 2100 --warmup-seconds 300 \
  --sample-seconds 5 --turn-timeout 120

python3 analyze.py /artifacts/before.jsonl /artifacts/after.jsonl
```

First smoke each revision with `--duration-seconds 20 --warmup-seconds 0`. Check
successful terminal responses, inventory 36, all three discovery counts nonzero,
tool calls exactly `25 * completed_turns`, provider calls exactly
`26 * completed_turns`, and zero errors. Smoke artifacts require fresh mocks just
like full measurements. Errors abort instead of silently counting incomplete
turns. The driver preserves diagnostic counters on failure when reachable.

The driver sends the historical `/ws/chat?agent=soak` protocol: receive
`session_start`, send `{"type":"connect"}`, receive `connected`, then send one
`{"type":"message","content":"..."}` at a time. It requires the `done` frame's
`full_response` to equal `SOAK_DONE:<turn>:25`, validates actual server counters,
and only then advances the completed-turn count. `error`, `aborted`, and unexpected
approval requests abort the run. A per-turn wall-clock deadline still applies if
the server emits nonterminal frames continuously. Only fixed-category WebSocket
frame counts are retained, not payloads.

Sampling starts before connection setup. Warmup is measured from the recorder's
monotonic origin. When duration expires, the current turn is allowed to finish
within its deadline, so total wall time can exceed the target by one turn. Sample
records contain elapsed/monotonic time, completed turns, real mock counters, VmRSS
and PSS in KiB. Process start ticks and executable path must remain unchanged;
metadata includes the executable SHA-256. The driver must be able to read the
target process's `smaps_rollup`; missing permission aborts rather than substituting
another process or RSS estimate. JSONL output is exclusive-create and line-flushed.

## Analysis and evidence limits

Run before and after sequentially under matched settings. Intended historical
revisions are before `c7f4435b95103b645a5e80585b3b2af412b0cd33` and after
`5bf683defcd0fd9237eb640b6425e9b978c8dd20` (PR #9208 head). Record image IDs,
architecture, libc/compiler versions, configuration and resource limits separately.
The revision flag is an operator provenance declaration, not Git verification;
the binary hash is measured directly.

The analyzer rejects errors/incomplete artifacts, checks workload/settings and
effective MCP payload equality, and restricts comparisons to overlapping completed
turn ranges after both warmups. It reports descriptive ordinary least-squares
KiB/turn and KiB/second slopes, medians, and endpoints; full post-warmup wall-time
summaries are also included. Sampling is time-based, so the overlap is a shared
turn **range**, not exact paired observations at every identical turn number.
Offline analysis loads sample rows in memory; live processes do not retain them.

Autocorrelation and one run per revision preclude independent-sample significance
claims. A flat baseline is **not reproduced**, not proof of a fix. One fresh
WebSocket session also does not reproduce the original report's multiple parallel
and restored conversations. Retain failed runs and explain deviations; do not
extend duration or add repetitions without a specific unresolved concern.

## Focused checks

```sh
python3 -m unittest discover -s tests/manual/mcp-memory-soak -v
```

Run this command from the repository root. These tests exercise actual local HTTP
requests, MCP inventory and calls, deterministic completion, SSE framing, payload
fingerprints/drift rejection, driver acceptance checks and matched-turn analysis.
They do not replace the real-runtime smoke or Linux/glibc measurements. Historical
protocol references: `crates/zeroclaw-gateway/src/ws.rs`,
`crates/zeroclaw-providers/src/compatible.rs`, and
`crates/zeroclaw-tools/src/{mcp_client,mcp_transport,mcp_tool}.rs` at the revisions
above.
