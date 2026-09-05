# Voice & Telephony

Real-time voice input and output. The available paths cover carrier calls, SIP-grade conversation, local microphone wake, external voice hosts, and outbound speech synthesis.

## ClawdTalk (real-time SIP)

Full-duplex SIP voice powered by Telnyx. The agent talks over a real phone call (inbound or outbound). Supports barge-in, mid-turn tool use, and regional number provisioning.

{{#config-fields channels.clawdtalk}}

`api_key` (Telnyx) and `webhook_secret` are secrets:

{{#secret-config channels.clawdtalk.<alias>.api_key}}

**Pair with:** a `telnyx` model provider for the brain and ensure your Telnyx account has a SIP connection with the correct webhook URL pointed at the ZeroClaw gateway.

## Voice Call (Twilio / Telnyx / Plivo)

Traditional carrier voice: the agent picks up, transcribes the caller, replies with TTS. Higher latency than ClawdTalk but works with any regular phone number and doesn't require SIP trunk provisioning. Outbound calls hit `from_number` and require operator approval when `require_outbound_approval` is on.

{{#config-fields channels.voice_call}}

## Voice Wake (local wake-word)

Runs locally, listens on the mic, triggers agent interaction when it hears the wake phrase. Useful for:

- Physical voice assistants on SBCs
- Desktop "hotword → ask" workflows
- Always-listening home-automation agents

The agent doesn't send audio anywhere; wake detection is local. Only post-wake speech is captured and (separately) transcribed before reaching the LLM.

{{#config-fields channels.voice_wake}}

> **Build flag:** Voice Wake is gated by the `voice-wake` cargo feature on `zeroclaw-channels`. Build with `--features voice-wake` to include it.
> On Android, Voice Wake requires Android 8 (API level 26) or newer.

## VoiceHost (external real-time audio stack)

VoiceHost connects ZeroClaw as a WebSocket client to a process that owns the complete audio path: microphone capture, VAD, ASR, TTS, and speaker playback. ZeroClaw receives text and control events and remains responsible for the agent, model provider, RAG, MCP, tools, and approvals. Raw audio never enters this channel.

This boundary works well when the audio stack has its own lifecycle or hardware requirements. A host can run FunASR or SenseVoice, sherpa-onnx, CrispASR, or a host-side Wyoming adapter without adding that engine to ZeroClaw.

```toml
[channels.voicehost.office]
enabled = true
backend = "wyoming-events-ws" # "native" is the default
url = "ws://127.0.0.1:8765/ws"
api_key = "replace-through-secret-management"
voice = "en-US"
forward_partials = false
approval_timeout_secs = 300
excluded_tools = ["shell"]

[agents.assistant]
channels = ["voicehost.office"]
```

{{#config-fields channels.voicehost}}

`api_key` is sent as `Authorization: Bearer ...` only during the WebSocket upgrade:

{{#secret-config channels.voicehost.<alias>.api_key}}

When `api_key` is configured, non-loopback endpoints must use secure WebSocket (`wss://`). Bearer credentials over the plaintext WebSocket scheme are accepted only for `localhost`, `127.0.0.0/8`, or `::1` development endpoints. An unauthenticated remote plaintext endpoint is allowed but has no transport confidentiality; prefer secure WebSocket off-machine.

> **Build flag:** VoiceHost is gated by `channel-voicehost`, is off by default, and is included by `channels-full`.

### Host contract

The connection is one-to-one. Each connection attempt times out after 15 seconds. Every outbound WebSocket write has a 5-second deadline; a stalled writer is disconnected and retried without blocking inbound control handling. ZeroClaw reconnects with bounded backoff and sends WebSocket ping frames while connected.

| Direction | Native backend | `wyoming-events-ws` profile | Effect |
|---|---|---|---|
| Host → ZeroClaw | `speech_end { transcript, event_id? }` | `transcript` with `data.text` and optional `data.event_id` | Submit a final transcript |
| Host → ZeroClaw | Not supported | `transcript-chunk` with `data.text` | Add passive context when `forward_partials = true` |
| Host → ZeroClaw | `barge_in` | `user-event` named `barge_in` | Cancel the current turn without starting another |
| ZeroClaw → host | `say { text, voice? }` | `synthesize` | Synthesize and play the agent response |
| ZeroClaw → host | `tts_cancel` | `user-event` named `tts_cancel` | Stop current playback |
| ZeroClaw → host | `transcript_ack { event_id? }` | `user-event` named `transcript_ack` | The final was accepted into ordered local delivery |
| ZeroClaw → host | `error { code: "transcript_replay_required", event_id?, retryable: true, reconnect: true }` | `user-event` named `transcript_replay_required` | The final was not accepted; reconnect and replay it |
| Both | `user-event` approval request/response | Same | Map approve, deny, and always-approve to the standard tool approval path |

`wyoming-events-ws` is a custom text-only WebSocket profile: each WebSocket text frame contains one JSON object shaped like a Wyoming event. It is not the Wyoming peer-to-peer TCP protocol, which uses newline-terminated JSON headers followed by optional data and payload bytes. Standard Wyoming servers and satellites cannot connect directly; place an adapter in the host process to translate between Wyoming TCP framing and this WebSocket profile.

Approval requests contain a generated request ID, tool name, and compact argument summary. Raw tool arguments are not sent. Unknown, malformed, binary, and server-direction events are ignored without reaching the model.

Final and partial transcripts are limited to 16 KiB after trimming, and the WebSocket decoder rejects messages or frames larger than 20 KiB before materializing the JSON event. When partial forwarding is enabled, ZeroClaw forwards at most one partial every 250 ms and at most 32 partials between accepted finals, including across reconnects. It drops partials while the shared ingress queue is full. Passive partials are recorded as context only; they never trigger an SOP workflow.

Final transcripts preserve arrival order. If the shared ingress queue is full, ZeroClaw keeps up to 32 accepted finals in a local FIFO that survives socket reconnects while continuing to read interruption events. Every accepted final receives `transcript_ack`; an unacknowledged final must be replayed. Hosts should include a stable `event_id` so acknowledgements and replay requests can be correlated; recently accepted IDs are acknowledged without dispatching a duplicate turn. When the FIFO is full, ZeroClaw sends `transcript_replay_required` for the rejected final, flushes the notice, and closes the socket; the host must reconnect and replay that final. Barge-in uses a bounded priority path, and duplicate interruption controls and remote TTS-cancel requests are coalesced while one is pending.

VoiceHost is an audio delivery surface. A text-only `SendMessage` is rejected rather than reported as delivered; callers such as `send_via` receive a failed result and can choose a channel that supports text.

### FunASR and SenseVoice deployment

Keep the model runtime in the host process. A production host typically runs this pipeline:

1. Capture and echo-cancel audio near the microphone.
2. Run VAD and windowing in the host.
3. Stream or batch the window through FunASR or SenseVoice and emit normalized transcript events.
4. Receive `say` or `synthesize`, run the selected TTS engine, and play audio locally.
5. Emit `barge_in` as soon as new speech is detected during playback.

For multilingual local recognition, SenseVoiceSmall is suitable for Chinese, English, Japanese, Korean, Cantonese, and code-switching. Fun-ASR-Nano can be served by a GPU host when its language-model-assisted recognition is needed. The ZeroClaw agent model remains independent and can run through llama.cpp, vLLM, or another configured model provider.

Use `wss://` when the host is not on the same trusted machine. Keep acoustic data and model-specific timestamps inside the host unless the application explicitly needs normalized metadata.

VoiceHost continues the deferred full-duplex work in [#5896](https://github.com/zeroclaw-labs/zeroclaw/issues/5896). ESP32 satellites and physical approval controls are tracked in [#7944](https://github.com/zeroclaw-labs/zeroclaw/issues/7944).

## TTS (outbound speech synthesis)

TTS is an output service channels call into, not its own inbound channel. Global defaults live under `tts`. TTS provider instances are configured under `providers.tts.<type>.<alias>` (OpenAI, ElevenLabs, Google, Edge, Piper) and selected per agent via the agent's `tts_provider`. See [Model Providers](../providers/overview.md) for the provider entries and per-agent wiring. Provider API keys are secrets; set them through the gateway, zerocode, or `zeroclaw config set`, never in plaintext.

---

## Latency budget

Speech feels real-time below ~500 ms end-to-end. Practical budgets:

| Component | Typical latency |
|---|---|
| Wake detection (local) | <100 ms |
| STT (Whisper local) | 300–800 ms per utterance |
| LLM first-token | 100–2000 ms (model dependent) |
| TTS first-audio | 200–700 ms |
| Network (cellular / PSTN) | 100–300 ms RTT |

ClawdTalk shortcuts several of these by keeping the audio stream live; regular `voice_call` incurs STT + LLM + TTS sequentially.

## STT

Speech-to-text is configured separately from the voice channels; see the `[transcription]` config in the [Config reference](../reference/config.md). Voice channels invoke whichever transcription provider is active when they need to turn audio into text.

## Hardware notes

For always-on voice on an SBC:

- USB mic: any UAC-compliant mic works. `arecord -l` to verify the OS sees it.
- Speaker: either USB audio out or the SBC's onboard jack; pick the OS default device for the user the daemon runs as.
- Microphones with built-in AEC (acoustic echo cancellation) dramatically improve wake reliability when the speaker is nearby.

See [Hardware → Android](../hardware/android-setup.md) for Android-specific audio setup.
