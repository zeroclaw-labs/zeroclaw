# Provider-agnostic

The agent's brain is pluggable. Anthropic, OpenAI, Ollama, Bedrock, Gemini, Azure, OpenRouter, and any OpenAI-compatible endpoint (Groq, Mistral, xAI, and ~20 others) work out of the box. Per-agent dispatch and hint-based model routes let you run reasoning-heavy tasks on one model and cheap chat on another.

This is deliberate. We have opinions about quality but not about vendors. If a better model ships tomorrow under a different banner, the config is a one-line change.
