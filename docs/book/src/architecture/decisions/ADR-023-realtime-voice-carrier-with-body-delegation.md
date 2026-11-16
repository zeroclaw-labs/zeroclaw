---
id: ADR-023
title: Realtime voice carrier with delegation to the body
date: 2026-11-16
status: proposed
relates-to:
  - ADR-014
  - ADR-015
  - ADR-016
  - ADR-017
  - ADR-019
  - ADR-020
  - ADR-021
  - ADR-022
  - crates/zeroclaw-gateway/src/ws.rs
  - crates/zeroclaw-gateway/src/ws/surface.rs
  - crates/zeroclaw-gateway/src/voice_duplex.rs
  - crates/zeroclaw-config/src/bridge_access.rs
  - crates/zeroclaw-runtime/src/agent/persona_projection.rs
  - crates/zeroclaw-memory/src/companion/user_model.rs
---

# ADR-023: Realtime Voice Carrier With Delegation to the Body

## Status

Proposed on 2026-11-16 by the owner's decision. It is not accepted until the owner approves it. Merging this document records a proposal. It does not change a protected contract or activate a feature.

### What it supersedes in ADR-021 (on acceptance)

This ADR is the new ADR that ADR-021 §10 requires before a front harness may answer directly or gain another tool. For the carrier mode defined here, it replaces these parts of ADR-021:

- §1: the restriction that end-to-end speech models may be used only for recognition, synthesis, or the §10 front harness;
- §9, engine table: the row that limits cloud realtime models (`gpt-realtime-2.1`, Qwen-Omni realtime) to benchmarks;
- §10, "One tool": the single-model-tool rule, for the carrier's provider-side search only (§2 below);
- §10, "It must not": "answer any substantive question" and "decide whether a question is simple enough", which are replaced by the routing rule in §2 below; and "have a persona of its own", which is replaced by the kernel projection in §3 below;
- §10, "Voice and words": the rule that Identity, Principles and User Model records are not exported to the front harness, which is replaced by §3 below;
- §10, "Everything it says is reported": the statement that front-spoken reports live in the existing conversation record, which is replaced by §5 below;
- §12, third item: "Letting the front harness answer simple questions directly";
- Acceptance item 8, for carrier mode, which is replaced by this ADR's Acceptance.

All other ADR-021 decisions stay in force, including §2 (device and adapter split), §3 (echo cancellation), §4 (body streaming), §5 (interruption), §6 (playback evidence), §7 (bounded listening; voice input is never owner evidence), §8 (attention), the rest of §9, the §10 fallback to the cascaded path, and the §11 latency targets.

### What it amends elsewhere (on acceptance)

- **ADR-017 §3 and §4.** ADR-017 places spoken everyday questions such as the weather in the body's L0 and states that "the owner talks only to the body". Under this ADR, public, non-personal questions may be answered by the carrier while it speaks with the body's projected persona. Everything personal or consequential still goes to the body. ADR-017 is annotated when this ADR is accepted.
- **ADR-019, Yonbo Client.** The rule that "the adapter injects no persona or memory of its own" is tightened. The adapter's carrier instructions contain only routing and delegation rules; identity comes only from the kernel projection (§3).

## Context

- The owner tested two realtime speech-to-speech services as the robot's voice: MOSS Live (`moss-live-1.0`), which the Robotic adapter already integrates, and OpenAI `gpt-realtime`.
- MOSS Live cannot turn off automatic replies and has no forced tool choice. It does offer native client delegation:
  - the service emits `session.delegation.created` with an objective, bounded context, and a purpose of `task` or `reasoning`;
  - the client answers with `session.commentary.append` (speakable progress or result), `session.thinking.append` (silent facts) and `session.instructions.append`;
  - it also accepts a persona `{name, aliases, prompt}` and instructions through `session.update`, and emits `input.transcript.done` with the final user text.
- OpenAI Realtime can disable automatic responses (`create_response: false`). That would allow the strict cascaded mode of ADR-021 §1.
- The owner rejected the strict mode for the robot. Making every greeting or weather question wait for a full body turn is the wrong experience. Small, public, non-personal requests should be handled by the voice service directly, including with its own search. Everything else should be delegated, and the robot should say so ("好的，我去查查") instead of falling silent.
- The owner also requires that the body stay the single source of identity and memory, and that the robot know the owner: the Soul and owner-approved parts of the User Model.
- While preparing this record, the owner's operator session inspected the deployment on 2026-11-16, reading configuration section and key names only. It found three facts:
  1. The Robotic voice adapter connects to `/ws/chat` without `surface=voice` (`Zeroclaw-Robotic/crates/yonbo-voice/src/gateway.rs`).
  2. It authenticates with a paired token. `[gateway]` holds only `paired_tokens` and there is no `[gateway.bridges]` entry.
  3. No `gateway.operator.token_hashes` are configured. Paired tokens therefore keep legacy operator authority (`crates/zeroclaw-gateway/src/operator_auth.rs`, which logs a WARN for this case).

  Together, these mean the adapter's text may be recorded as operator input with owner ingress (`crates/zeroclaw-gateway/src/ws.rs`, owner-ingress handling), and the voice credential can call management APIs.
- The adapter currently writes its own identity into the carrier instructions (`Zeroclaw-Robotic/crates/yonbo-voice/src/moss.rs`, the default instructions naming the robot and the owner).

## Decision

### 1. The realtime voice service is a carrier

The realtime speech-to-speech service (MOSS Live, OpenAI Realtime, or a successor) is a **carrier**. It hears, speaks, and handles a bounded set of requests by itself. It is not the brain:

- it holds no memory beyond the open conversation window and writes none;
- its only capabilities are speech, the provider's built-in public search, and delegation to the body; it has no tool that acts on the owner's accounts, devices, files, messages, or schedules;
- it cannot approve anything;
- it receives no gateway credential; the adapter holds all credentials.

The carrier and its provider are chosen in the adapter's configuration. Replacing the carrier does not change the body.

### 2. What the carrier answers, and what it delegates

The carrier decides the routing itself, under these rules.

It may answer directly:

- greetings, small talk, and conversational repair (asking the owner to repeat);
- public, non-personal facts, including results from its built-in search: weather, time, public knowledge, simple arithmetic;
- acknowledgements that it is delegating ("好的，我去查查"). These never claim that work has started or finished; status comes from the body's receipts (ADR-020 §8).

It must delegate to the body anything involving:

- the owner's memory, history, preferences, relationships, or private data;
- tasks, reminders, devices, messages, purchases, or any action;
- the body's commitments, opinions about the owner, or decisions;
- anything it is unsure about.

The boundary is enforced by the carrier's instructions and by measurement (§7), not by the carrier's good will; prompting is not enforcement. This residual risk is accepted and bounded by §1: a misrouted request can produce wrong words, but it cannot act, approve, or write memory.

### 3. The kernel projects a voice persona; nothing raw leaves the body

**Which agent.** The voice surface connects to one configured agent alias. The projection and every delegated turn use that same alias, so the carrier sounds like the agent that answers delegated requests. The owner chooses the alias. Whether it is the owner's main agent or a dedicated restricted agent is an open question below.

**Where it is delivered.** When a voice-surface connection opens (`/ws/chat` with `surface=voice`, using the device bridge `chat` operation; the credential also holds `cancel` for §4), the body returns a versioned `voice_projection` field on the `connected` acknowledgement.

- This changes the protected `/ws/chat` contract. It is versioned and stays behind the default-off `voice-surface` feature, like ADR-021 §4.
- No new bridge operation is added.
- The adapter places the projection in the carrier's persona and instructions for that window only. It is never written to disk on the adapter or the device.
- The adapter's own instructions contain only routing and delegation rules (§2, §4), never an identity or a form of address.

**What it contains.** The projection renders the same bytes as the corresponding `persona_projection` layers for that alias:

- Identity: name, self-description, primary language, pronouns, and the fixed honesty line;
- Tone: the five Tone levels as rendered;
- Principles: the owner-authored list;
- owner facts for voice: only User Model heads that carry the owner's voice export consent (below).

**What it never contains:**

- the Story layer, both `self` and `bond` entries;
- Tone exemplars, which are owner lines;
- any User Model head without voice export consent;
- session history, memory search results, approvals, or credentials.

**Voice export consent.** User Model heads get a separate, owner-only export-consent field. It is distinct from `Scope`: `Scope` decides where a head applies, and consent decides whether its text may leave the body to a carrier.

- Consent is recorded per head and bound to the carrier provider identity. Changing provider requires fresh consent.
- Nothing has consent by default.
- This is a change to the protected User Model store. It needs its own reviewed slice.

**Fail closed.** If the projection cannot be produced, the adapter does not open the carrier session. That covers an unreadable Soul store; the degraded "identity unavailable" rendering; legacy `SOUL.md`/`IDENTITY.md` without an owner Identity revision; and an unknown alias. The robot gives a fixed local cue instead of speaking without a Soul.

### 4. Delegation goes through the voice surface

When the carrier delegates:

1. The adapter submits one `/ws/chat` turn on the voice surface, with a scoped request identity (ADR-020 §2). The turn carries two separately labelled inputs:
   - the carrier-authored objective and context, labelled carrier-authored;
   - the raw final transcript of the window's recent user speech, labelled unverified speaker transcript.
2. Neither input is owner evidence, and neither satisfies an approval (ADR-021 §7).
3. Carrier-handled exchanges from the same window (§5) may be included in the turn as quoted, labelled context. The body does not read them from storage.
4. The body's spoken-text result goes back to the carrier as speakable commentary; silent facts go back as thinking.
5. If the carrier cancels a delegation, the adapter requests cancellation of that scoped turn (ADR-021 §5).

The adapter authenticates with a device bridge credential (`[gateway.bridges]`, with the `chat` and `cancel` operations), not with a paired token that holds operator authority.

### 5. What the carrier handled is reported, not remembered

Each exchange the carrier handled itself is reported to the body as an untrusted presentation event. It carries the user's words, labelled unverified speaker transcript, and the carrier's reply, labelled carrier output.

- These events are stored in the voice session's presentation-evidence store from ADR-021 §6, with the same storage owner, retention and deletion. That store is identified in the implementation review.
- A carrier-handled exchange has no body turn, so its event is bound to the admitted device, conversation, listening window and exchange sequence number instead of a request and turn identity. Duplicates and reconnects cannot attach it to another window.
- They are not `user` or `assistant` session rows.
- They have no owner ingress. They never reach owner learning, User Model candidates, or the ADR-016 weekly reflection. Household members, television and false wakes can all produce transcripts.
- They may enter long-term memory only through a later owner-reviewed path, which is out of scope here.

### 6. Failure behavior

- **Body or gateway unreachable when a window opens.** There is no projection, so no carrier session opens (§3).
- **Body lost mid-window.** The carrier says it cannot reach the body and the window closes. It does not continue small talk without the body.
- **Carrier unreachable.** The adapter falls back to the ADR-021 cascaded path if that path is configured; otherwise the window does not open.

### 7. Measured before enabling

The following are measured on a fixed test set before carrier mode is enabled for daily use:

- **Delegation routing.** Count personal or action requests that the carrier answered itself. The target is zero on the test set, and every miss is reviewed.
- **Fabrication while waiting.** Check whether the carrier invents an answer while a delegation is pending. MOSS cannot disable automatic replies, so this must be observed, not assumed.
- **Latency.** Measure from the owner stopping speaking to first audio for a direct answer, and to the acknowledgement for a delegated request. ADR-021's targets are the reference.

### 8. Scope

- **Device Node.** The robot's Node role, meaning capabilities over `/ws/nodes`, is out of scope. It follows ADR-022 and the kernel capability catalog (#630). One inventory identity may group the robot's Node credential and the adapter's bridge credential, but the two are not interchangeable and are revoked separately (ADR-022).
- **`voice_duplex`.** The speaker Client designed here uses `ChatSurface::Voice` and does not use `gateway/src/voice_duplex.rs`, which only logs events. This settles the ADR-017 §5 "keep until the speaker Client is designed" row: the module may be removed in its own reviewed PR.

## Consequences

Positive:

- Small talk and public lookups feel immediate. Personal questions and actions still go through the body.
- The robot knows the owner through a reviewed projection, without raw Soul or unconsented User Model access.
- Carriers can be swapped between providers without a body change.
- The voice-credential and owner-ingress defects found in Context are corrected as part of adopting the voice surface.

Negative:

- The routing boundary in §2 depends on the carrier following instructions. A misroute can produce wrong words but no action.
- Projected persona text and consented owner facts go to a third-party provider.
- There are two voice paths to maintain: carrier mode and the cascaded fallback.
- The protected `/ws/chat` contract and the User Model store both change.

## Implementation slices

1. **Credential and surface correction.** This is configuration and build only; there are no source changes:
   - build the gateway with `voice-surface`;
   - give the adapter a `[gateway.bridges]` device credential with the `chat` and `cancel` operations, and connect with `surface=voice`;
   - configure operator token hashes so that paired tokens lose management authority;
   - before the cutover, list the clients that rely on paired-token management.
2. **Voice export consent** on User Model heads: owner-only, per head, bound to the provider.
3. **Handshake projection:** add a versioned `voice_projection` to the voice-surface `connected` acknowledgement, with tests for fail-closed behavior and for the exclusion of the Story layer, exemplars and unconsented heads.
4. **Adapter delegation** (Robotic repository):
   - map carrier delegation to scoped voice-surface turns with separately labelled inputs, commentary and thinking replies, and cancellation;
   - remove the adapter's own identity text from the carrier instructions.
5. **Presentation-event intake** for carrier-handled exchanges into the ADR-021 §6 store, with tests that no owner ingress and no session rows are created.
6. **Measurement** under §7, recorded as evidence before daily use.

Issue #374 remains the execution index. Each slice is one reviewed PR.

## Acceptance

1. A delegated voice turn records the carrier-authored objective and the speaker transcript as separately labelled, unverified inputs, never as operator input, with tests.
2. The voice credential cannot call operator-gated APIs.
3. The projection matches the corresponding `persona_projection` layers byte for byte, including pronouns and the honesty line. It contains no Story, exemplar, or unconsented User Model content. Every degraded or unknown state fails closed.
4. Carrier-handled exchanges create no session rows and no owner ingress.
5. The §7 measurements are recorded, and the owner accepts them before daily use.

## Open questions

- Which agent alias the voice surface uses: the owner's main agent, which gives one Soul for every surface, or a dedicated restricted agent with its own Soul. This decides whether the robot and the desktop share one personality.
- Whether carrier-handled exchanges should ever be offered for an owner-reviewed memory path.
