# Custom Providers

Three ways to add a provider ZeroClaw doesn't ship with:

1. **Point `openai-compatible` at the endpoint.** Works for ~80% of cases.
2. **Use a first-class local-server adapter** (`llamacpp`, `sglang`, `vllm`). Thin wrappers with sensible defaults.
3. **Implement the `Provider` trait** in Rust. For anything that's not OpenAI-compatible.

## OpenAI-compatible endpoint (easiest)

If the service speaks OpenAI chat-completions, this is a config-only change:

```toml
[providers.models.my-endpoint]
kind = "openai-compatible"
base_url = "https://my-gateway.example.com"
model = "my-model-id"
api_key = "..."                    # omit if the endpoint needs no auth
```

Then reference it:

```toml
default_model = "my-endpoint"
```

This is the same implementation used for Groq, Mistral, xAI, and every other OpenAI-compat provider in the [catalog](./catalog.md).

## First-class local-inference servers

ZeroClaw ships tight adapters for three popular local-inference stacks. They're `openai-compatible` under the hood but with defaults and quality-of-life tuning pre-applied.

### llama.cpp

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
