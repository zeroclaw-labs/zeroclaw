# Provider capability ownership

Provider-neutral ASR, TTS, embedding and decision contracts live in
`zeroclaw-api`. Their concrete adapters and HTTP clients live in
`zeroclaw-providers`, alongside chat adapters.

Channels consume speech capabilities; memory consumes embeddings; SOP dispatch
consumes decisions and retains its gate, confidence and execution-mode policy.
These consumers do not own vendor wire protocols. This extraction preserves
the released configuration and existing endpoint behavior.
