# Custom Providers

Three ways to add a provider ZeroClaw doesn't ship with:

1. **Point `openai-compatible` at the endpoint.** Works for ~80% of cases.
2. **Use a first-class local-server adapter** (`llamacpp`, `sglang`, `vllm`). Dedicated provider kinds with sensible defaults and server-specific behaviour.
3. **Implement the `Provider` trait** in Rust. For anything that's not OpenAI-compatible.

## OpenAI-compatible endpoint (easiest)

If the service speaks OpenAI chat-completions, this is a config-only change:

```toml
[providers.models."custom:https://my-gateway.example.com"]
kind = "openai-compatible"
base_url = "https://my-gateway.example.com"
model = "my-model-id"
api_key = "..."                    # omit if the endpoint needs no auth
```

Then reference it:

```toml
default_model = "custom:https://my-gateway.example.com"
```

This is the same implementation used for Groq, Mistral, xAI, and every other OpenAI-compat provider in the [catalog](./catalog.md).

## First-class local-inference servers

ZeroClaw ships dedicated provider kinds for three popular local-inference stacks.
Unlike `openai-compatible`, these are purpose-built adapters — not thin wrappers.
`llamacpp` in particular routes all traffic through the OpenAI Responses API
(`/v1/responses`) rather than chat-completions, which is the only path that
supports streaming tool events correctly for local models.

### llama.cpp (`kind = "llamacpp"`)

`llamacpp` is a **dedicated provider kind**, not a variant of `openai-compatible`.
It routes all calls through llama-server's `/v1/responses` endpoint and handles
SSE streaming, chain-of-thought suppression, and tool calls natively.
`openai-compatible` pointed at a llama-server will work for basic prompts but
lacks correct tool-call streaming.

```bash
llama-server -hf ggml-org/gpt-oss-20b-GGUF --jinja -c 133000 --host 127.0.0.1 --port 8033
```

```toml
[providers.models.llama]
kind = "llamacpp"                          # alias: "llama.cpp"
base_url = "http://127.0.0.1:8033/v1"      # omit to use default http://localhost:8080/v1
model = "ggml-org/gpt-oss-20b-GGUF"
# api_key only required if llama-server was started with --api-key
```

**Optional fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `think` | `bool` | — | Sets `enable_thinking` in the request body. `false` signals thinking-capable models to skip chain-of-thought. |
| `chat_template_kwargs` | table | — | Passed verbatim as `chat_template_kwargs` to the Jinja chat template. Use for model-family-specific template variables. |
| `max_tokens` | `u32` | — | Maximum output tokens per response. |
| `timeout_secs` | `u64` | 120 | Request timeout for non-streaming calls. |

**Controlling thinking mode** varies by model family. `think = false` sets the
top-level `enable_thinking` field in the request. Some models (e.g. Qwen3) read
this flag from the Jinja template via `chat_template_kwargs` instead:

```toml
[providers.models.llama]
kind = "llamacpp"
base_url = "http://127.0.0.1:8033/v1"
model = "Qwen/Qwen3-30B-A3B-GGUF"
think = false
# Qwen3 reads enable_thinking from the Jinja template, not the top-level field:
chat_template_kwargs = { enable_thinking = false }
```

Other model families use different template variable names — check your model's
chat template and set the appropriate key under `chat_template_kwargs`.

### SGLang

```bash
python -m sglang.launch_server --model meta-llama/Llama-3.1-8B-Instruct --port 30000
```

```toml
[providers.models.sglang]
kind = "sglang"
base_url = "http://localhost:30000/v1"     # default
model = "meta-llama/Llama-3.1-8B-Instruct"
```

### vLLM

```bash
vllm serve meta-llama/Llama-3.1-8B-Instruct
```

```toml
[providers.models.vllm]
kind = "vllm"
base_url = "http://localhost:8000/v1"      # default
model = "meta-llama/Llama-3.1-8B-Instruct"
```

## Validation

Regardless of which approach:

```bash
zeroclaw models refresh --provider <name>   # list models the endpoint advertises
zeroclaw agent -m "hello"                    # smoke-test with a one-shot message
```

## Implementing a new `Provider` trait

If the endpoint isn't OpenAI-compatible and isn't one of the first-class local adapters, you need code.

The trait lives in `crates/zeroclaw-api/src/provider.rs`:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn supports_streaming(&self) -> bool { true }
    fn supports_streaming_tool_events(&self) -> bool { false }

    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        options: ChatOptions,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;
}
```

Implementation pattern:

1. Add a file to `crates/zeroclaw-providers/src/myprovider.rs`
2. Implement `Provider` — translate `Vec<Message>` to the wire format, stream the response, emit `StreamEvent` values
3. Register via the factory in `lib.rs`:
   ```rust
   factory.register("myprovider", |cfg| MyProvider::new(cfg).boxed());
   ```
4. Add a feature flag in `Cargo.toml` if the provider pulls heavy deps
5. Update `[providers.models.<name>] kind = "myprovider"` parser in the config schema

See `anthropic.rs` as a reference for a provider with a fully custom wire format. See `compatible.rs` for the SSE-streaming OpenAI-compat pattern.

## Troubleshooting

### Authentication errors

- Verify the API key matches the endpoint (many providers use key prefixes — `sk-`, `gsk_`, `sk-ant-`)
- Check the `base_url` includes the scheme (`http://` / `https://`) and the `/v1` path if the endpoint expects it
- Endpoints behind a VPN or proxy? Confirm routing from the ZeroClaw host

### Model not found

- List what the endpoint advertises:
  ```bash
  curl -sS "$BASE_URL/models" -H "Authorization: Bearer $API_KEY" | jq
  ```
- If the endpoint doesn't implement `/models`, send a direct chat request and read the error — most endpoints return the expected model family in the error body
- Check that the endpoint exposes the model you're asking for; gateway services often expose only a subset

### Connection issues

- `curl -I $BASE_URL` — does it respond?
- Firewall, proxy, egress rules? VPS providers sometimes block outbound high ports
- Provider status page if it's a hosted service

## See also

- [Overview](./overview.md) — provider model and how routing works
- [Configuration](./configuration.md) — full `[providers.*]` schema
- [Catalog](./catalog.md) — every ZeroClaw-provided provider
- [Developing → Plugin protocol](../developing/plugin-protocol.md) — if a plugin works better than a first-class crate
