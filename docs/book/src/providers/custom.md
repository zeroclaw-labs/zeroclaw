# Custom Providers

Three ways to add a provider ZeroClaw doesn't ship with:

1. **Use the `custom` slot.** For any OpenAI-compatible endpoint not covered by an existing canonical slot.
2. **Use the first-class local-server slots** (`lmstudio`, `llamacpp`, `sglang`, `vllm`, `osaurus`, `litellm`). Thin wrappers with sensible defaults.
3. **Implement the `ModelProvider` trait** in Rust. For anything that's not OpenAI-compatible.

## OpenAI-compatible endpoint: use the `custom` slot

If the service speaks OpenAI chat-completions, this is a config-only change. The `custom` slot requires `uri` (the family's endpoint enum has no default); reference it from an agent's `model_provider`.

This is the same `OpenAiCompatibleModelProvider` runtime impl used by `groq`, `mistral`, `xai`, and every other vendor with its own canonical slot in the [catalog](./catalog.md). The difference is which family slot you use: `custom` is the catch-all for endpoints not represented by a vendor slot.

## First-class local-inference servers

ZeroClaw ships canonical slots for popular local-inference stacks. They're all OpenAI-compatible under the hood but with default `uri` values pre-applied so you can usually omit `uri` entirely.

### llama.cpp: slot `llamacpp`

<div class="os-tabs-src">

#### sh

```sh
llama-server -hf ggml-org/gpt-oss-20b-GGUF --jinja -c 133000 --host 127.0.0.1 --port 8033
```

</div>

**Optional fields** apply to any compat-slot family (including `llamacpp`). The
full set, derived from the schema:

{{#config-fields providers.models.custom}}

**Controlling thinking mode** varies by model family. `think = false` sets the top-level `enable_thinking` field in the request. Some models (e.g. Qwen3) read this flag from the Jinja template via `chat_template_kwargs` instead:

Other model families use different template variable names, check your model's chat template and set the appropriate key under `chat_template_kwargs`.

### SGLang: slot `sglang`

<div class="os-tabs-src">

#### sh

```sh
python -m sglang.launch_server --model meta-llama/Llama-3.1-8B-Instruct --port 30000
```

</div>

### vLLM: slot `vllm`

<div class="os-tabs-src">

#### sh

```sh
vllm serve meta-llama/Llama-3.1-8B-Instruct
```

</div>

### LM Studio, Osaurus, LiteLLM

Slots `lmstudio`, `osaurus`, `litellm` follow the same pattern, see the [catalog](./catalog.md).

## Wire protocol: `wire_api = "responses"`

Bring-your-own-endpoint slots default to the OpenAI chat-completions wire. An endpoint that only speaks the OpenAI **responses** wire (some self-hosted vLLM / TGI deployments) needs an explicit `wire_api = "responses"` opt-in on the alias entry.

When set to `"responses"`, the provider is built as an `OpenAiResponsesModelProvider` (full streaming tool calls over the responses protocol) instead of a chat-completions provider. Omit the field, or set `"chat_completions"`, for the default wire.

`wire_api` is only honored by the bring-your-own-endpoint families where the wire is operator-configurable: `openai`, `llamacpp`, and `custom` (plus the generic openai-compatible path). Branded vendor slots (`groq`, `mistral`, `deepseek`, …) have a fixed wire protocol and ignore the field. The setting governs both the primary agent path and delegate targets, so a delegate whose target alias declares `wire_api = "responses"` reaches the endpoint over the responses wire.

## Validation

Regardless of approach:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw config list                          # loads config; any validation failures print to stderr
zeroclaw models refresh --provider <type>.<alias>   # list models the endpoint advertises
zeroclaw agent -a <alias> -m "hello"          # smoke-test against the agent at `[agents.<alias>]`
```

</div>

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

- Verify the API key matches the endpoint (many vendors use key prefixes: `sk-`, `gsk_`, `sk-ant-`).
- Check that `uri` includes the scheme (`http://` / `https://`) and the `/v1` path if the endpoint expects it.
- Endpoints behind a VPN or proxy? Confirm routing from the ZeroClaw host.

### Model not found

- List what the endpoint advertises:
<div class="os-tabs-src">

#### sh

```sh
  curl -sS "$URI/models" -H "Authorization: Bearer $API_KEY" | jq
  ```

</div>
- If the endpoint doesn't implement `/models`, send a direct chat request and read the error, most endpoints return the expected model family in the error body.
- Gateway services often expose only a subset of upstream models.

### Connection issues

- `curl -I $URI`, does it respond?
- Firewall, proxy, egress rules? VPS providers sometimes block outbound high ports.
- Vendor status page if it's a hosted service.

### Gateway rejects `temperature`

Some gateways (e.g. a LiteLLM proxy fronting `claude-opus-4-7`) return an error
when a `temperature` field is present at all. ZeroClaw honors the `Option`
contract: if you leave `temperature` unset in config, the field is **omitted**
from the request body entirely and the backend picks its own default. Only set
`temperature` explicitly when the endpoint accepts it.

## See also

- [Overview](./overview.md): provider model and how per-agent dispatch works
- [Configuration](./configuration.md): full `[providers.*]` schema, Azure typed config, regional and OAuth variants
- [Catalog](./catalog.md): every canonical slot with a worked TOML example
- [Developing → Plugin protocol](../developing/plugin-protocol.md): if a plugin works better than a first-class crate
