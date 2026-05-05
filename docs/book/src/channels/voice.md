# Voice & Telephony

Real-time voice input and output. Four channels cover the matrix: inbound calls, local microphone wake, outbound speech synthesis, and SIP-grade real-time conversation.

## ClawdTalk (real-time SIP)

```toml
[channels.clawdtalk]
enabled = true
api_key = "..."                                # Telnyx API key (secret)
connection_id = "..."                          # Telnyx SIP connection ID
from_number = "+14155550123"                   # caller-ID for outbound dials
allowed_destinations = ["+14155551234"]        # destinations allowed for outbound dial; empty = none
webhook_secret = "..."                         # optional: shared secret for inbound Telnyx webhook verification
```

Full-duplex SIP voice powered by Telnyx. The agent talks over a real phone call (inbound or outbound). Supports barge-in, mid-turn tool use, and regional number provisioning.

**Pair with:** a `telnyx` model provider for the brain (`crates/zeroclaw-providers/src/telnyx.rs`) and ensure your Telnyx account has a SIP connection with the correct webhook URL pointed at the ZeroClaw gateway.

## Voice Call (Twilio / Telnyx / Plivo)

```toml
[channels.voice_call]
enabled = true
provider = "twilio"                            # "twilio" (default), "telnyx", or "plivo"
account_id = "..."                             # provider-specific account identifier
auth_token = "..."                             # provider-specific auth token (secret)
from_number = "+14155550123"
webhook_port = 8090                            # default 8090; embedded webhook server
require_outbound_approval = true               # default true; require operator approval before dialing
transcription_logging = true                   # default true; persist call transcripts
tts_voice = ""                                 # optional voice ID override (provider-specific)
max_call_duration_secs = 3600                  # default 3600 (1 hour cap)
webhook_base_url = ""                          # optional public base URL when behind a tunnel/proxy
```

Traditional carrier voice — the agent picks up, transcribes the caller, replies with TTS. Higher latency than ClawdTalk but works with any regular phone number and doesn't require SIP trunk provisioning. Outbound calls hit `from_number` and require operator approval when `require_outbound_approval` is on.

## Voice Wake (local wake-word)

```toml
[channels.voice_wake]
wake_word = "hey zeroclaw"                     # default "hey zeroclaw" (case-insensitive substring match)
silence_timeout_ms = 2000                      # default 2000; ms of silence before finalising capture
energy_threshold = 0.01                        # default 0.01; RMS energy below this is treated as silence
max_capture_secs = 30                          # default 30; hard cap on capture duration
```

Runs locally, listens on the mic, triggers agent interaction when it hears the wake phrase. Useful for:

- Physical voice assistants on SBCs
- Desktop "hotword → ask" workflows
- Always-listening home-automation agents

The agent doesn't send audio anywhere — wake detection is local. Only post-wake speech is captured and (separately) transcribed before reaching the LLM.

> **Build flag:** Voice Wake is gated by the `voice-wake` cargo feature on `zeroclaw-channels`. Build with `--features voice-wake` to include it.

## TTS (outbound speech synthesis)

TTS lives at the top level under `[tts]`, not under `[channels.*]` — it's an output service that channels can call into, rather than its own inbound channel.

```toml
[tts]
enabled = true
default_provider = "piper"                     # "openai", "elevenlabs", "google", "edge", or "piper"
default_voice = "en_US-lessac-medium"          # provider-specific default voice ID
default_format = "mp3"                         # "mp3" (default), "opus", or "wav"
max_text_length = 4096                         # default 4096

[tts.openai]
api_key = "..."
model = "tts-1"                                # default "tts-1"
speed = 1.0                                    # default 1.0

[tts.elevenlabs]
api_key = "..."
model_id = "eleven_monolingual_v1"             # default "eleven_monolingual_v1"
stability = 0.5                                # default 0.5
similarity_boost = 0.5                         # default 0.5

[tts.google]
api_key = "..."
language_code = "en-US"                        # default "en-US"

[tts.edge]
binary_path = "edge-tts"                       # path to the edge-tts binary; default "edge-tts"

[tts.piper]
api_url = "http://127.0.0.1:5000/v1/audio/speech"   # OpenAI-compatible Piper HTTP endpoint
```

Only the section for the active `default_provider` needs to be filled in. Pair `[tts]` with `voice_wake` for a complete local voice assistant.

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

Speech-to-text is configured separately from the voice channels — see the `[transcription]` config in the [Config reference](../reference/config.md). Voice channels invoke whichever transcription provider is active when they need to turn audio into text.

## Hardware notes

For always-on voice on an SBC:

- USB mic: any UAC-compliant mic works. `arecord -l` to verify the OS sees it.
- Speaker: either USB audio out or the SBC's onboard jack; pick the OS default device for the user the daemon runs as.
- Microphones with built-in AEC (acoustic echo cancellation) dramatically improve wake reliability when the speaker is nearby.

See [Hardware → Android](../hardware/android-setup.md) for Android-specific audio setup.
