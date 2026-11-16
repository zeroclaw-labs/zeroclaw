---
id: ADR-021
title: Full-duplex voice at the edge - audio stays out of the body
date: 2026-10-30
status: proposed
relates-to:
  - ADR-011
  - ADR-013
  - ADR-014
  - ADR-017
  - ADR-019
  - ADR-020
  - crates/zeroclaw-gateway
---

# ADR-021: Full-Duplex Voice at the Edge - Audio Stays Out of the Body

## Status

The legacy Yonbo N3B1 crate has been retired from ZeroClaw source; its complete 11-file snapshot is preserved in the [Zeroclaw-Robotic archive](https://github.com/kckylechen1/Zeroclaw-Robotic/tree/main/integration/zeroclaw/source/crates/zeroclaw-yonbo-node). Future device engineering belongs there. This changes neither running devices and keys nor Gateway Node v2, and does not establish a replacement or physical acceptance.

Proposed on 2026-10-30. Not accepted until the owner approves it.

Partly superseded on 2026-11-16 by the proposed [ADR-023](./ADR-023-realtime-voice-carrier-with-body-delegation.md): when ADR-023 is accepted, for its carrier mode it replaces the §1 restriction on end-to-end speech models, the §9 benchmark-only row for cloud realtime models, the §10 one-tool rule (for provider-side search only), the §10 "It must not" rules on answering, judging simplicity and persona, the §10 "Voice and words" export rule, the storage sentence of §10 "Everything it says is reported", the third item of §12, and Acceptance item 8. See ADR-023 for the exact scope. All other decisions here are unchanged.

Merging this document records a proposal. It does not accept the architecture, activate a feature, change a protected contract, or claim device acceptance. Issue #374 remains the execution index; implementation requires its own bounded review and evidence.

Amended on 2026-11-10 with the device audio facts reported in #626: the vendor recognition engine runs on the device and is open source, so reusing it is a candidate rather than a rejected cloud path (Context, §7, §9, §12), and the microphone array's channel layout must be checked for a playback reference before an echo-cancellation design is chosen (§3). The echo-cancellation, capture-while-frozen and light-ownership questions of slice 1 are still open.

This record preserves the authority boundaries of ADR-017 and builds on the proposed ADR-019 and ADR-020. It proposes where full-duplex speech (listening while speaking, with interruption) runs, what the body must add to support it, and how an optional voice front harness with its own small model attaches (§10).

## Context

- The inspected voice path on the Yonbo X1 (#535) is half-duplex and turn-based: the vendor app recognizes speech, sends text to `GPTUrl`, which points at the owner's Mac adapter; the adapter calls the brain and returns `say` audio through `TTSUrl`. The reported owner boot chain freezes the vendor recognition processes (`asr.xapp`, `audio_capture.x`) and mutes the speaker. The vendor wake word ("元宝") did not gate capture in that probe. This is deployment evidence, not proof that the repository successor has been installed.
- The device probes in #535 report a SmartIC USB microphone that streams regardless of capture switches, a speaker, PulseAudio, an RK3568 (4 × A55, corrected by the later device-tree probe), an NPU, and an unused hardware VAD block. The RGB light is driven by the vendor playback process (`audio_play.xapp`), so audio playback and the light are coupled. This design places conversational model inference on the DGX Spark or the Mac; device feasibility and audio ownership remain acceptance work.
- The #626 survey, with the vendor capture processes thawed in an owner-attended window, observed the dialogue process reading an 8-channel, 16 kHz, 16-bit PulseAudio source from the microphone array and streaming it over a local socket to the recognition process, which is built on sherpa-onnx with its models stored on the device. Recognition is therefore local, not a cloud service. Whether any of the eight channels is a playback loopback is not yet known. The speaker sink is 48 kHz stereo.
- On the body, the voice surface (#512) renders only the final reply for voice; streamed voice chunks are empty. A spoken reply therefore cannot start until the whole reply is finished. Turn controls (#498) already allow cancelling a turn.
- ADR-020 §8 proposes publishing pre-tool acknowledgement text before the call runs. An already clear, authorized instruction does not need another confirmation merely because work is slow or delegated. Publication does not prove playback or that a person heard it.
- Full duplex needs, in order of dependency: acoustic echo cancellation (AEC) using the speaker signal as a reference, so the robot does not hear itself; voice activity detection (VAD); streaming recognition; end-of-turn detection; streaming synthesis; interruption (barge-in) handling; and scoped playback receipts separate from canonical conversation history.
- Two families of solutions exist:
  - End-to-end speech models that listen, reason, call tools, and speak in one model. Cloud: Qwen-Omni realtime, OpenAI `gpt-realtime-2.1`, Doubao realtime. Open weights that run locally: MiniCPM-o 4.5 (9B, Chinese and English, full duplex; internally Whisper, Qwen3-8B, and CosyVoice2), Qwen3-Omni-30B-A3B, and Moshi (full duplex, English only).
  - Cascaded pipelines: streaming recognition, a text brain, streaming synthesis, coordinated by a state machine. Open Chinese components exist for each stage (for example FunASR streaming recognition and CosyVoice streaming synthesis; FunSpeech packages both as one local service).
- End-to-end models can reason and call tools on their own. Allowing an independent voice brain to answer or act would bypass the body's Soul, memory, approvals, and Tachi delegation (ADR-017 §1, ADR-019 §1). Whether a bounded front harness improves first-audio latency, interruptions, and backchannels on this deployment is a hypothesis to measure.
- The owner proposed putting a small model in front as a receptionist that acknowledges quickly, forwards to the body's configured thinking model, and lets the body hand complex work to Tachi.

## Decision

### 1. Cascaded, and the body never handles audio

The brain is always the body's agent loop. Speech is converted to text before it reaches the body, and text is converted to speech after it leaves the body. End-to-end speech models are never the brain. They may be used only as a recognition or synthesis component (text out or text in), or as the model of the voice front harness under the rules of §10.

### 2. Three places, three jobs

| Where | Owns |
| --- | --- |
| Device (robot, user space only) | microphone capture, AEC with the playback reference, VAD, the local interruption fast path, playback, the mic-open light, wake trigger |
| Voice adapter (on an owner-controlled host; the current Mac adapter grows into it) | streaming recognition, end-of-turn detection, the duplex state machine, sentence buffering, streaming synthesis, the `/ws/chat` voice surface connection, and the front harness (§10) when enabled |
| Body | text turns, versioned spoken-text projection, scoped turn controls, attention projection, canonical history and bounded playback evidence |

The splits follow ADR-019 §2: AEC must run next to the speaker and microphone (place to run); recognition and synthesis models are heavy (dependencies); a broken voice pipeline must not affect text surfaces (failure isolation). Both edge processes may be written in any language (ADR-019 §4).

### 3. Echo cancellation runs on the device, continuously

- AEC runs on the device, where the microphone and the playback reference share a clock. It does not run on the adapter host, where network jitter would break the alignment.
- First establish the microphone array's channel map. If it carries a playback loopback channel, that channel is the reference and already shares the capture clock, which is simpler than reconstructing one from the playback path.
- Otherwise preferred: PulseAudio's echo-cancel module with the WebRTC method, if the vendor build supports it. Otherwise the device process runs a WebRTC audio-processing or speexdsp canceller itself, and must then own playback so it has the reference.
- AEC stays on for the whole conversation window, not only while the robot speaks, because the room's echo tail continues after playback ends.

### 4. The body streams spoken text

- The target is sentence-sized spoken-text chunks. The current #512 renderer operates on a complete private outbound reply, with whole-reply markup handling and credential checks after rendering. Calling it independently on provider chunks does not preserve that policy: markup or a secret can cross chunk boundaries, and audio already played cannot be retracted.
- A streaming renderer must retain state across provider chunks, apply security checks to the exact text about to be released, and define a reviewed policy for late markup. Until the protected-contract review establishes that policy and its tests, keep the existing buffered final-reply fallback. Do not silently replace whole-reply suppression with "speak the prefix, then a notice."
- Pre-tool text (ADR-020 §8) is committed and published before dispatch, then enters the same safe presentation path. Publication, playback acknowledgement, and a person hearing it are different facts; missing playback evidence neither grants approval nor authorizes retrying an action.
- Streaming and playback reports are proposed changes to the voice surface contract, a protected area (`AGENTS.md`). They must be versioned and remain behind the default-off `voice-surface` feature. Review includes split markup, cross-chunk secret reconstruction, late markup after a releasable prefix, cancellation, and fallback behavior.

### 5. Interruption in two steps

1. **Local and immediate.** When the device detects speech during playback, it lowers the playback volume at once, with no network round trip. Speech detection does not identify the speaker as the owner.
2. **Decided by the adapter.**
   - Sustained speech or recognized words: stop playback, request cancellation of the observed foreground turn using its scoped identity and connection generation (#498, ADR-020 §5), and submit recognized speech as unverified speaker input under the admitted conversation scope.
   - Short backchannels ("嗯", "对", "好"): restore the volume and keep speaking.
   - Explicit stop phrases ("停", "等一下", "不是"): stop immediately without waiting for more speech.

The deterministic adapter owns these protocol controls; they are not extra model tools. A cancellation request is not a successful stop receipt. Local playback stop does not roll back an external effect, and disconnect or a stale generation must not cancel a different turn. Background work, such as a running Tachi job, is not automatically cancelled by an interruption; any external cancellation remains subject to its own supported contract and truthful unknown/unsupported outcome (ADR-020 §5, §8).

### 6. Playback evidence accompanies canonical history

- Canonical assistant text and tool history remain complete under the existing history and retention rules. A device playback report cannot truncate or rewrite the shared transcript, replay storage, or another surface's view.
- The adapter appends bounded presentation evidence tied to the admitted device/surface, conversation, request and turn identity, connection generation, and spoken segment: played up to a position, interrupted, or unknown. Reconnect and duplicate reports cannot advance another session's playback position. The implementation review must identify the existing storage owner and retention policy; this proposal does not create an independent history store.
- A voice continuation may use those receipts to avoid assuming that unplayed text was presented. A receipt proves at most what the device reports playing, never that a person heard, understood, or approved it. Unknown remains unknown.
- Each recognized utterance gets a scoped request identity and payload binding from the adapter (ADR-020 §2), so a reconnect does not create a duplicate turn or reuse an identity for different input.

### 7. Listening is bounded

- Audio leaves the device only after a wake trigger (a wake word detected on the device, or a touch or button) and only during an open conversation window. The window closes after a silence timeout. The light shows whenever the microphone is open to the adapter.
- Wake-word recognition runs on the device: first on the CPU, later on the NPU if supported and measured. Hardware VAD can be a speech/activity prefilter; it is not itself a wake-word recognizer.
- Voice input is unverified speaker input (#512): it never becomes owner evidence for learning, and a spoken answer never satisfies an approval (ADR-020 §8).
- No audio goes to the vendor cloud. The vendor capture and recognition processes stay frozen by default. Thawing them, or running the same on-device engine under the device voice process (§9), is allowed only inside the wake and window bounds above, and only if the vendor dialogue process cannot forward audio or text off the device.

### 8. Voice state and attention state

The adapter's local duplex state (listening, speaker active, robot speaking, overlap) drives speaking and listening behavior directly. The body's privacy-appropriate attention projection (ADR-020 §7: thinking, working, needs-you, done, error) can drive remaining display capacity. The mic-open/privacy indicator always takes precedence while conversation audio is captured or transmitted. Attention or mood must never mask it; local wake-only processing and active conversation capture must be distinguishable.

### 9. Engines and models are endpoints outside the body

- Conversational model weights stay outside the body and adapter, behind independently configured inference services on the DGX Spark or the Mac. Recognition and synthesis also have adapter-owned endpoint contracts; implementations and supported serving frameworks are measured per candidate. Bounded device wake/VAD processing is allowed. Model changes require capability and behavior checks, not just a new endpoint string.
- Recognition and synthesis engines are chosen in the adapter's configuration, local or cloud. Candidates to measure, not decisions:

| Candidate | Possible role | Notes |
| --- | --- | --- |
| sherpa-onnx with the models already on the robot | recognition, on the device | local and open source; removes the audio uplink for recognition, at the cost of device CPU and of depending on vendor-shipped model files; measure latency and accuracy against the host candidates |
| FunASR streaming (`paraformer-zh-streaming`) or Fun-ASR-Nano | recognition | local; FunSpeech packages it with CosyVoice |
| CosyVoice streaming | synthesis | local; same family as the synthesis inside MiniCPM-o, which may help voice consistency |
| macOS `say` | synthesis fallback | available on a configured Mac with the required voice; not a portable fallback |
| MiniCPM-o 4.5 | front harness model (§10) | published 9B model, Apache-2.0; pin the inference recipe and measure memory and concurrency before selection |
| Qwen3-Omni-30B-A3B | front harness model or recognition | local, larger |
| Qwen-Omni realtime, OpenAI `gpt-realtime-2.1` | benchmark only | cloud; used to set the target feel, never as the brain |

- Cloud engines are allowed as an owner choice. Sending audio to a cloud provider is a privacy decision recorded in the configuration.

### 10. The voice front harness

#### What it is

The front harness is a small agent inside the voice adapter: its own model endpoint (for example MiniCPM-o), its own loop, and exactly one model tool. Its proposed benefit is quicker acknowledgements while the body thinks; that benefit must be measured. It is optional; the cascaded path of §2 works without it.

It is an agent, but not a subagent. In ADR-017 terms it is part of a Client:

| | Subagent (ADR-017 L1) | Front harness |
| --- | --- | --- |
| Who calls whom | the body sends it work | it forwards recognized speech to the body with its unverified provenance |
| Where it runs | inside the body | in the voice adapter, at the edge |
| How it reaches the body | an internal call | `/ws/chat` on the voice surface, like any Client (ADR-019 §4) |
| Lifetime | one task | the open conversation window |

Three harness layers result, each with its own model endpoint and loop, each reaching the next only through a fixed contract:

| Layer | Harness | Owns |
| --- | --- | --- |
| Ears and mouth | front harness (voice adapter) | bounded acknowledgements, backchannels, reading the body's text aloud; deterministic adapter owns interruption controls |
| Brain | ZeroClaw body | Soul, memory, approvals, what to do |
| Hands | Tachi and the CLI agents it starts | heavy work |

Personal identity, memory authority, and owner approval stay in the body. Tachi still enforces its own delegated grants, and devices retain local safety controls; the front harness cannot extend any of those authorities.

#### Why it is not inside the body

ADR-011 would allow a second configured agent in the body, but:

- the front harness consumes and produces audio, and the body never handles audio (§1);
- its loop is a continuous audio stream with interruptions; the body's loop is text turns that carry the Soul, memory, and approvals, so every extra hop adds latency;
- a second agent that speaks to the owner from inside the body would be a second identity (ADR-017 §1);
- it needs none of the body's memory, approvals, or scheduler.

#### One tool

Its only model tool is `ask_zeroclaw`: send recognized speech as unverified input on the voice surface, with a scoped request identity (ADR-020 §2), and receive the body's safe spoken-text projection (§4). The deterministic adapter separately implements scoped turn cancellation (#498), playback, and admitted attention subscriptions (ADR-020 §7); the model does not receive broader gateway credentials or control tools. Front-harness context and playback state are isolated per admitted device, conversation, and listening window even when inference engines are shared.

It may:

- select short acknowledgements and fillers from an explicitly approved, bounded presentation/phrase projection; until that contract exists, use separately owner-approved neutral phrases or nonverbal feedback;
- recognize backchannels and stop phrases for the deterministic adapter's bounded handling (§5);
- ask the owner to repeat when it did not hear clearly, which is a hearing problem, not an answer;
- read the body's text aloud, word for word;
- request local playback behavior through the adapter's fixed state machine, without acquiring a general device tool.

It must not:

- answer any substantive question, however simple;
- decide whether a question is simple enough to answer itself; every substantive utterance is forwarded;
- call any other tool;
- keep memory beyond the open conversation window, or write memory;
- approve anything, create owner authority from room speech, or invent a claim that delegation has started; dispatch status comes from the body's canonical receipt (ADR-020 §8), and an already authorized request does not need a repeated intent confirmation;
- have a persona of its own.

A change that gives the front harness a second tool, memory, or the ability to answer directly needs a new ADR.

#### Voice and words

- ADR-018 exemplars teach style, not reusable facts or claims, and can contain owner lines. Raw exemplars, Identity, Principles, Story, and User Model records are not exported to the front harness. A presentation/phrase projection needs its own reviewed scope, redaction, size bound, and owner approval; read-only access alone does not make raw Soul access appropriate. There is no separate persona file in the adapter.
- The robot should sound like one speaker. Either the front harness reads the body's text verbatim or it and the synthesis engine use the same owner-selected voice. Verbatim fidelity and consistency are measured before enabling the front harness; unvalidated generated wording falls back to the deterministic synthesis path.

#### Everything it says is reported

The adapter reports every phrase it selects and attempts to play, with the bounded playback evidence of §6. These are source-tagged, untrusted presentation events in the existing conversation record, not canonical body answers, owner evidence, task completion, or approval. They cannot overwrite assistant/tool history. The body can distinguish selected text, reported playback, interruption, and unknown delivery when continuing the conversation. Retention and privacy deletion apply to these derivatives too.

#### Fallback

If the front harness or its model endpoint is down, the adapter falls back to the cascaded path of §2 (recognition, body, synthesis). Text surfaces are not affected either way.

#### Building it

The front harness may be written from scratch or built on an existing voice-agent framework (Pipecat, LiveKit Agents) with the model step restricted to the one tool. This is evaluated in slice 7. Its code may live in this repository as an edge program, like `bridges/telegram`, but it is never compiled into the body.

### 11. Latency targets

Proposed targets, to be measured and adjusted by the owner:

| Moment | Target |
| --- | --- |
| Owner stops speaking to first audio, direct answer | about 1.2 s (p50) |
| Owner stops speaking to acknowledgement, slow work | about 1 s; with the front harness, about 0.5 s |
| Owner starts speaking to playback lowered | about 200 ms |
| Owner starts speaking to playback stopped, real interruption | about 500 ms |

### 12. Not adopted

- **An end-to-end speech model as the brain.** It would bypass the Soul, memory, approvals, and delegation.
- **The front harness inside the body** as a second configured agent (ADR-011). Audio, latency, and a second identity.
- **Letting the front harness answer simple questions directly.** Deciding what is simple is a decision, and decisions belong to the body.
- **AEC on the adapter host.** The reference and microphone would not share a clock.
- **Streaming the room to the host all the time.** Listening is bounded by the wake trigger and the conversation window.
- **Any vendor cloud path** for ZeroClaw conversations. The vendor's on-device recognition engine is a candidate under §9, not part of this exclusion.

## Consequences

Positive:

- The owner can interrupt the robot naturally, and the robot does not answer itself.
- The Soul, memory, approvals, and delegation stay the only brain, whatever voice engines or front model are used.
- The front harness may reduce acknowledgement latency if its bounded behavior and resource use pass the measured trial.
- Engines and models can be swapped, local or cloud, without a body change.
- Text surfaces keep working when the voice pipeline is down.
- Canonical history stays complete; scoped playback evidence describes device presentation without claiming human hearing or changing another surface's history.

Negative:

- More edge pieces to run and upgrade: the device voice process, the grown adapter, and the front harness model service.
- The voice surface contract change touches a protected area.
- Echo cancellation quality depends on the vendor audio stack, which cannot be replaced at the kernel level.
- Taking over playback may also mean taking over the light, because the vendor couples them.
- Running the thinking model, the front harness model, recognition, and synthesis on one DGX Spark may exceed its capacity.
- The front harness is a standing temptation to add features; the one-tool rule has to be defended in review.

## Implementation slices

Each slice is one PR:

1. **Device audio probe.** On the robot: can a new process capture the microphone while the vendor capture processes are frozen; is PulseAudio echo cancellation available; who owns playback and the light; measure how much echo remains. Evidence only, no product change.
2. **Device voice process.** User-space process under `/userdata`: capture, AEC, VAD, playback, light, and an authenticated audio link to the adapter. Its credential follows the ADR-019 revocation rule. Audio transmission remains disabled until an explicit wake/button window and the privacy indicator of §7–§8 are active; earlier probes use a bounded operator-opened window.
3. **Adapter streaming, half-duplex first.** Streaming recognition, end-of-turn detection, sentence buffering, streaming synthesis. No interruption yet.
4. **Body streaming voice surface.** Review a versioned stateful renderer and late-markup policy, exact-output credential checks across chunk boundaries, safe pre-tool publication, bounded playback receipts, and source-tagged front-spoken reports. Preserve canonical history and buffered fallback; keep `voice-surface` default-off.
5. **Interruption.** Local volume drop, adapter decision, backchannel handling, stop phrases, scoped turn cancellation, and truthful external outcomes.
6. **Bounded listening.** Wake trigger, conversation window, mic-open light.
7. **Front harness trial.** Run MiniCPM-o on the DGX Spark next to the thinking model. Measure: verbatim reading without added words, voice consistency with the synthesis engine, time to first audio, and whether both models fit together. Compare a from-scratch loop with Pipecat and LiveKit Agents. Optionally record a cloud benchmark (`gpt-realtime-2.1`) for the target feel. Evidence only.
8. **Front harness.** One model tool; separately approved phrase projection with no raw Soul/User Model export; per-conversation/window isolation; source-tagged presentation reports; fallback to the cascaded path.

This order expresses dependencies, not an instruction to start work ahead of issue #374. Schedule slices 3 and 4 with the owning voice work; slice 5 follows measured half-duplex use, and slices 7 and 8 follow slice 5. Before each slice, check upstream for equivalent work and record the result, per `AGENTS.md`.

## Acceptance

This ADR is implemented when:

1. the body links no audio library and embeds no model weights, and stopping the adapter leaves text surfaces working;
2. a long reply played in a quiet room produces no owner turn (the robot does not hear itself);
3. interrupting mid-sentence lowers and stops playback within measured targets, reports the scoped cancellation outcome truthfully, and records the interrupted playback position while preserving complete canonical assistant/tool history and other surfaces' replay; stale or duplicate receipts cannot affect a different turn;
4. a short backchannel during playback does not stop it;
5. for a slow request, safe acknowledgement text is committed and published before tool dispatch (ADR-020 §8); physical playback ordering is separately measured, and missing receipts never authorize action replay or imply that a person heard it;
6. no audio leaves the device outside an open conversation window; the privacy light is visible whenever conversation capture or transmission is active and cannot be masked by attention or mood;
7. voice input never becomes owner evidence and never satisfies an approval, with tests;
8. when the front harness is enabled: it exposes exactly one model tool; substantive questions, including simple ones, are always forwarded with unverified provenance; its own words come only from the approved presentation/phrase projection; no raw Soul/User Model data or other conversation leaks into its context; its reports remain untrusted presentation events; and endpoint failure falls back to the cascaded path;
9. split-markup and cross-chunk secret cases cannot leak through the spoken projection; late markup follows the explicitly reviewed policy, the default-off gate and buffered fallback work, and playback reports cannot mutate canonical history or grant approval.

## Open questions

- Can a new process capture the microphone while the vendor capture processes are frozen, or do they hold the device? (Slice 1.)
- Does the 8-channel microphone source include a playback loopback channel? (Slice 1.)
- Is PulseAudio echo cancellation available on the vendor build? (Slice 1.)
- If the device takes over playback, how is the light driven? (Slice 1.)
- Which audio transport: an extension of the N3B1 link, or a separate authenticated stream?
- Local or cloud recognition and synthesis, and can the DGX Spark run the thinking model, the front harness model, recognition, and synthesis together? (Slice 7.)
- Can MiniCPM-o read the body's text verbatim without adding words, and can it match the synthesis voice? (Slice 7.)
- What bounded presentation/phrase projection can be approved without exporting raw exemplars or personal records? Can ADR-014's presentation-only rules be reused without expanding that contract?
- What late-markup policy preserves the protected voice guarantees while permitting useful streaming, and which existing storage owner retains bounded playback events?
- Build the front harness from scratch, or on Pipecat or LiveKit Agents? (Slice 7.)
- Keep "元宝" as the wake word, or use the duck's own name?
- How long should the conversation window stay open after the last speech?

## References

- [ADR-011: Multi-agent runtime boundaries](./ADR-011-multi-agent-runtime-boundaries.md)
- [ADR-014: Reviewed AgentSoul projection](./ADR-014-reviewed-agentsoul-projection.md)
- [ADR-017: One personal agent body, edge devices, and delegation through Tachi](./ADR-017-personal-agent-body-edges-and-delegation.md)
- [ADR-018: Soul character model](./ADR-018-soul-character-model.md)
- [ADR-019: One body, many hands](./ADR-019-one-body-many-hands.md)
- [ADR-020: Durable turns, replay classes, and attention](./ADR-020-durable-turns-replay-and-attention.md)
- [Issue #535](https://github.com/kckylechen1/zeroclaw/issues/535) (robot state, audio and light ownership), including the [RK3568 device-tree correction](https://github.com/kckylechen1/zeroclaw/issues/535#issuecomment-6519736146); PR #512 (voice surface), #498 (turn controls), #499 (adapter acceptance)
- [Current private voice projection](https://github.com/kckylechen1/zeroclaw/blob/47baee19659e9031d1e938715b5fb0dcef42547f/crates/zeroclaw-gateway/src/ws/surface.rs)
- [OpenBMB MiniCPM-o guide](https://openbmb.cn/docs/en/guides/minicpm-o) and [MiniCPM-o 4.5 model card](https://huggingface.co/openbmb/MiniCPM-o-4_5); [OpenAI GPT-Realtime-2.1](https://developers.openai.com/api/docs/models/gpt-realtime-2.1). These describe candidates, not measured ZeroClaw latency or memory budgets.
- Candidate engines and models: FunASR (`modelscope/FunASR`), CosyVoice, FunSpeech (`zaigie/FunSpeech`), MiniCPM-o 4.5 (`openbmb/MiniCPM-o-4_5`), Qwen3-Omni (`QwenLM/Qwen3-Omni`)
- Voice-agent frameworks to evaluate: Pipecat, LiveKit Agents
