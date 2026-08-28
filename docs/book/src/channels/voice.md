# Voice & Telephony

Real-time voice input and output. Several channels cover the matrix: inbound calls, local microphone wake, outbound speech synthesis, SIP-grade real-time conversation, and a hosted realtime speech-to-speech broker.

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

## Speech-to-Speech Broker (Gemini Live)

A realtime bidirectional voice model (Gemini Live) owns the human-facing call
audio and speaks with the caller directly; it reaches the ZeroClaw agent
through exactly one bridge function, `consult_agent`, and relays the agent's
settled reply back in its own words. The voice model never touches tools;
only the agent does, via the normal agent/tool pipeline.

```toml
[channels.speech_to_speech.desk]
enabled = true
model_kind = "native_audio"
model = "gemini-2.5-flash-native-audio-preview-12-2025"
voice = "Autonoe"
activation = "hotkey_toggle"
broker_persona_path = "personas/broker.md"

[agents.primary]
channels = ["speech_to_speech.desk"]
```

`voice` defaults per `model_kind` when omitted (`Autonoe` for `native_audio`,
a Kore-class voice for `half_cascade`), so it's safe to drop from a minimal
config. `broker_persona_path` is resolved relative to, and confined to, the
agent's workspace; it never falls back to or reads `AGENTS.md`.

> **Build flag:** Speech-to-Speech is gated by the `channel-speech-to-speech`
> cargo feature on `zeroclaw-channels` and is **off by default**. Build with
> `--features channel-speech-to-speech` to include it. As of this writing the
> channel ships its broker session engine and config surface; the audio-frame
> transport (mic in / speaker out) lands in a follow-up release, so the
> channel stays inert even when enabled until that lands.

{{#config-fields channels.speech_to_speech}}

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
