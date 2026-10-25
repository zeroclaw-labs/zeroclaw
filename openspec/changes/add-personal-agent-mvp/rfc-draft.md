# RFC draft: Personal Agent MVP subsystem (for a human sponsor to file)

> Status: DRAFT for review by the prospective sponsor. Not filed, not
> ratified, and not authorized for external posting by this artifact alone.
> AI-drafted with the human sponsor accountable per RFC #5615 once filed.
> Filing: a GitHub issue tagged `type:rfc` titled
> `RFC: Personal Agent MVP: visitor-facing knowledge-and-request subsystem`.
> Supporting artifacts: the OpenSpec change `add-personal-agent-mvp`
> (proposal/design/specs/tasks), [native-token-audit.md](native-token-audit.md),
> [benchmark-freeze-protocol.md](benchmark-freeze-protocol.md), PRD v0.6 in
> `prd/`. This body summarizes; the OpenSpec artifacts carry the detail and
> are the implementation contract if ratified.

## Problem

ZeroClaw today is a single-tenant personal assistant: one operator, pairing
as the shared native operator, a loopback-bound gateway, and no concept of a
second, limited trust tier. Teams repeatedly interrupt a knowledgeable person
for the same answers and incomplete requests, and the PRD v0.6 process (three
review rounds) validated a product shape for fixing that: a local-network
personal agent that answers from explicitly published, reviewable knowledge
and collects structured collaboration requests — without exposing private
assistant context and without the agent ever making commitments.

Serving that product requires a decision this repository treats as
RFC-shaped: a **new subsystem** (`zeroclaw-personal`) plus a **new,
externally reachable trust boundary** — a TLS listener that serves
non-operator visitors on the LAN, authorized by a new person-bound identity
model, touching the shared pairing authority and the gateway composition.

## Proposal

Add an opt-in, default-disabled Personal Agent MVP to the existing ZeroClaw
instance:

- A `zeroclaw-personal` domain crate (SQLite persistence, scheduling,
  retention, business authorization), composed by the root application and
  exposed through gateway transport adapters. No new business subsystem in
  `zeroclaw-runtime`; the outward Q&A turn reuses the proven `agent_turn`
  entry point with an empty sealed tool registry and no private memory
  (proved by fixtures, see tasks 1.2 evidence).
- A separately composed TLS visitor listener with its own allowlisted
  router, fully isolated from the loopback-only owner control plane.
- Person-bound visitor authorization (max 5 authorized people, per-device
  browser sessions, 72-hour expiring applications), owner native-credential
  records gaining mint/expiry metadata, and compromise-recovery flows.
- Versioned knowledge publication with review intervals, a 7-day time-only
  grace, invalidity quarantine, and citation-bound answers validated by
  deterministic server-side checks (no semantic claim of correctness).
- Structured collaboration requests with an explicit state machine,
  idempotency, optimistic concurrency, retention, and self-service deletion.
- One verified SMTP binding with three independent category switches,
  timezone-anchored digests, bounded retries, and fail-closed intake gating.
- Local-only inference by default; cloud egress requires a separately
  disclosed and confirmed data-category authorization.

Full P0 scope and acceptance mapping: A01-A23 in PRD v0.6 with the OpenSpec
traceability table.

## Design

The OpenSpec change's `design.md` is the design record (10 decision sections
with canonical-source tables, alternatives, and failure boundaries). Key
points for this body:

1. **Composition**: domain crate depends on `zeroclaw-api` contracts only;
   gateway/root inject clock, execution, current-authority, and mail
   adapters. Background work uses `zeroclaw-spawn` with generation drain.
2. **Storage**: private SQLite at `<data_dir>/personal/personal.db` (WAL,
   FKs, owner-only permissions, versioned migrations); eligibility and
   deadlines are views over canonical records + injected clock; no mirrored
   state (`is_stale`/`is_overdue`/person-count snapshots forbidden).
3. **Transport separation**: loopback owner control plane unchanged; visitor
   listener mounts only personal routes (`/api/personal/*`), rejects
   startup on TLS/binding violations; visitor auth rejects operator tokens
   and vice versa; CSRF/origin enforced on mutations; separate cookie names
   and server-side hashed sessions.
4. **Pairing authority migration** (the shared-subsystem change): the
   canonical hash-only set becomes records with hash + mint time + optional
   expiry, serialized compatibly. Personal activation invalidates ambiguous
   legacy tokens **instance-wide** (including the owner's private-assistant
   and CLI clients) with an explicit pre-activation warning; disabled
   deployments keep expiry-free behavior. RPC paths already enforce
   `Principal.expires_at` at every live-grant checkpoint, so record expiry
   flows through existing enforcement once admission stamps it; direct
   hash-set consumers on gateway HTTP/SSE/WS surfaces are enumerated in the
   audit and migrate onto the record check (task 3.2 tests each).
5. **Answer isolation**: dedicated outward identity; no personality, skills,
   MCP, attachments, other visitors' history, or tools; one generation round
   with buffered publication behind deterministic citation/permission/grace
   validation; malformed output maps to a failure/transfer, never a repair
   generation.
6. **Fair scheduling**: per-person round-robin with all five first-waiting
   slots reserved at startup; approval never conflicts with queued questions.

## Alternatives considered

- **Everything in `zeroclaw-runtime` / gateway `AppState`**: rejected — the
  runtime holding-crate contract forbids new subsystems, and the gateway
  must stay a transport owner.
- **WASM plugin implementation**: deferred — the required visitor auth,
  separate listener, and durable scheduling contracts are not demonstrated
  by current plugin APIs.
- **Parallel expiry table beside the existing hash set**: rejected — two
  authorities for one fact; the record migration is the single-source fix.
- **A separate standalone service instead of an integrated subsystem**:
  rejected for the MVP — it would duplicate provider/model/config/session
  infrastructure the instance already owns; the integrated crate keeps one
  authority per fact. Revisit if isolation requirements outgrow the
  shared-host model (PRD §15 keeps per-owner instances as the deployment
  boundary).
- **Cookie-only owner sessions without touching native tokens**: rejected —
  the raw Bearer would outlive the browser session; the audit shows every
  accepting transport must bind the same canonical expiry.

## Non-goals

P1 discovery, unread badges, agent-to-agent transfer, chat-platform
integrations, enterprise login, per-document visitor ACLs, arbitrary tool
execution for visitors, task-system integrations, per-answer semantic
correctness certification, multi-factor authentication (first version),
automatic attrition detection for departing colleagues, and the separately
discussed work-progress module (not present in PRD v0.6). No performance SLA
is implied; appendix B values stay candidates until the benchmark protocol
freezes them.

## Risks and mitigations

- **Instance-wide blast radius of native credential expiry**: documented
  activation warning + recovery preparation (tasks 3.1, 12.1, 12.5);
  rollback never resurrects invalidated tokens.
- **Visitor listener as a new attack surface**: allowlisted router only,
  fail-closed startup, injection-isolation corpus (A14), egress intercept
  tests (A06), and 0-event expectation for unauthorized disclosure in the
  pilot metrics.
- **Citations mistaken for truth**: structural checks are explicitly not
  semantic verification; offline evaluation and human review with separate
  attribution (A10, A18).
- **Maintenance consuming the saved time**: grace labels, review digests,
  daily time accounting including tunnel/reauthentication costs (PRD §12).
- **SMTP cannot prove reading or exactly-once**: at-least-once ledger with
  stable Message-IDs, truthful overdue visibility, health-gated intake.
- **Latency candidates miss on CPU-only hardware**: measure first (benchmark
  protocol), renegotiate the published deadline or model before pilot; never
  weaken checks to meet a number.

## Rollout

1. Ratify this RFC (snapshot = the OpenSpec change as filed).
2. Land additive typed config + DB migrations, disabled by default;
   pairing-token records land compatibly for disabled installs.
3. Owner interface on loopback with verified TLS; visitor listener with
   minimal routes and intake closed; establish identity/knowledge/people.
4. Run A01-A23 at real HTTP/browser boundaries with the benchmark protocol;
   freeze appendix B values with artifacts.
5. Verify mail, open intake explicitly, run the two-week pilot against the
   PRD §12 metrics.
6. Rollback: close intake + listener, disable personal mode, drain jobs,
   revoke personal sessions; no legacy-token resurrection, no deleted-content
   restore; backups replay deletion policy on restore.

Feature-flagged by `Config.personal_agent.enabled` (default off,
loopback-bound default installation). Schema migrations are additive and
versioned.
