# Custom Providers

Three ways to add a provider ZeroClaw doesn't ship with:

1. **Use the `custom` slot.** For any OpenAI-compatible endpoint not covered by an existing canonical slot.
2. **Use the first-class local-server slots** (`lmstudio`, `llamacpp`, `sglang`, `vllm`, `osaurus`, `litellm`). Thin wrappers with sensible defaults.
3. **Implement the `ModelProvider` trait** in Rust. For anything that's not OpenAI-compatible.

## OpenAI-compatible endpoint — use the `custom` slot

If the service speaks OpenAI chat-completions, this is a config-only change:

```toml
[providers.models.custom.gateway]
uri     = "https://my-gateway.example.com/v1"
model   = "my-model-id"
api_key = "..."                          # omit if the endpoint needs no auth
```

The `custom` slot requires `uri` (the family's endpoint enum has no default). Reference it from an agent:

```toml
[agents.default]
enabled        = true
model_provider = "custom.gateway"
```

This is the same `OpenAiCompatibleModelProvider` runtime impl used by `groq`, `mistral`, `xai`, and every other vendor with its own canonical slot in the [catalog](./catalog.md). The difference is which family slot you use — `custom` is the catch-all for endpoints not represented by a vendor slot.

## First-class local-inference servers

ZeroClaw ships canonical slots for popular local-inference stacks. They're all OpenAI-compatible under the hood but with default `uri` values pre-applied so you can usually omit `uri` entirely.

### llama.cpp — slot `llamacpp`

```bash
llama-server -hf ggml-org/gpt-oss-20b-GGUF --jinja -c 133000 --host 127.0.0.1 --port 8033
```

```toml
[providers.models.llamacpp.default]
uri   = "http://127.0.0.1:8033/v1"       # omit to use default http://localhost:8080/v1
model = "ggml-org/gpt-oss-20b-GGUF"
# api_key only required if llama-server was started with --api-key
```

### SGLang — slot `sglang`

```bash
python -m sglang.launch_server --model meta-llama/Llama-3.1-8B-Instruct --port 30000
```

```toml
[providers.models.sglang.default]
uri   = "http://localhost:30000/v1"      # default
model = "meta-llama/Llama-3.1-8B-Instruct"
```

### vLLM — slot `vllm`

```bash
vllm serve meta-llama/Llama-3.1-8B-Instruct
```

```toml
[providers.models.vllm.default]
uri   = "http://localhost:8000/v1"       # default
model = "meta-llama/Llama-3.1-8B-Instruct"
```

### LM Studio, Osaurus, LiteLLM

Slots `lmstudio`, `osaurus`, `litellm` follow the same pattern — see the [catalog](./catalog.md).

## Validation

Regardless of approach:

```bash
zeroclaw config validate                        # confirms every agent's model_provider resolves
zeroclaw models refresh --provider <type>.<alias>   # list models the endpoint advertises
zeroclaw chat -a default -m "hello"             # smoke-test against the agent named `default`
```

## Implementing a new `ModelProvider` trait

If the endpoint isn't OpenAI-compatible and isn't one of the local-server slots, you need code.

The trait lives in `crates/zeroclaw-api/src/model_provider.rs`:

```rust
#[async_trait]
pub trait ModelProvider: Send + Sync {
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

1. Define the typed config in `crates/zeroclaw-config/src/schema.rs`:
   ```rust
   pub struct MyProviderModelProviderConfig {
       #[serde(flatten)]
       pub base: ModelProviderConfig,
       pub endpoint: MyProviderEndpoint,
       // family-specific fields
   }

   pub enum MyProviderEndpoint { Default }
   impl ModelEndpoint for MyProviderEndpoint {
       fn uri(&self) -> &'static str {
           match self { Self::Default => "https://my-provider.example.com/v1" }
       }
   }
   ```
2. Add the slot to `for_each_model_provider_slot!` in `crates/zeroclaw-config/src/providers.rs`. Every helper picks up the new slot automatically.
3. Add the runtime impl in `crates/zeroclaw-providers/src/myprovider.rs`. Translate `Vec<Message>` to the wire format, stream the response, emit `StreamEvent` values.
4. Wire the factory branch in `crates/zeroclaw-providers/src/lib.rs::create_provider_with_url_and_options`.
5. Add a feature flag in `Cargo.toml` if the provider pulls heavy deps.

See `anthropic.rs` as a reference for a provider with a fully custom wire format. See `compatible.rs` for the SSE-streaming OpenAI-compat pattern.

## Troubleshooting

### Authentication errors

- Verify the API key matches the endpoint (many vendors use key prefixes — `sk-`, `gsk_`, `sk-ant-`).
- Check that `uri` includes the scheme (`http://` / `https://`) and the `/v1` path if the endpoint expects it.
- Endpoints behind a VPN or proxy? Confirm routing from the ZeroClaw host.

### Model not found

- List what the endpoint advertises:
  ```bash
  curl -sS "$URI/models" -H "Authorization: Bearer $API_KEY" | jq
  ```
- If the endpoint doesn't implement `/models`, send a direct chat request and read the error — most endpoints return the expected model family in the error body.
- Gateway services often expose only a subset of upstream models.

### Connection issues

- `curl -I $URI` — does it respond?
- Firewall, proxy, egress rules? VPS providers sometimes block outbound high ports.
- Vendor status page if it's a hosted service.

## See also

- [Overview](./overview.md) — provider model and how per-agent dispatch works
- [Configuration](./configuration.md) — full `[providers.*]` schema, Azure typed config, regional and OAuth variants
- [Catalog](./catalog.md) — every canonical slot with a worked TOML example
- [Developing → Plugin protocol](../developing/plugin-protocol.md) — if a plugin works better than a first-class crate
