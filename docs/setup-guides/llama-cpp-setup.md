# llama.cpp Integration Guide

ZeroClaw supports running local GGUF models through two paths: the native `llama.cpp` server
(`llamacpp` provider) and Ollama acting as an OpenAI-compatible wrapper over llama.cpp models
(`ollama` provider). Both expose an OpenAI-compatible REST API, so the config is nearly
identical — the choice depends on operational preference.

## When to Use Which

| Approach | Provider ID | Best For |
|---|---|---|
| `llama-server` direct | `llamacpp` | Maximum control, GPU flags, speculative decoding, custom GGUF |
| Ollama wrapper | `ollama` | Simple model management (`ollama pull`), automatic restarts |

## Approach 1: Direct llama.cpp Server

### Prerequisites

- [llama.cpp](https://github.com/ggerganov/llama.cpp) built from source or via a release binary
- A GGUF model file (e.g., from Hugging Face)

### Starting the Server

```bash
# CPU only
llama-server --model /path/to/model.gguf --port 8080

# Apple Silicon (Metal)
llama-server --model /path/to/model.gguf --port 8080 -ngl 99

# NVIDIA GPU (CUDA)
llama-server --model /path/to/model.gguf --port 8080 --n-gpu-layers 40

# Vulkan
llama-server --model /path/to/model.gguf --port 8080 --n-gpu-layers 40 --device vulkan0

# With authentication (optional)
llama-server --model /path/to/model.gguf --port 8080 --api-key my-secret
```

The server exposes `http://localhost:8080/v1` once running.

### ZeroClaw Config

```toml
[provider]
name = "llamacpp"
model = "local"   # or the model name llama-server reports (check /v1/models)

[provider.options]
api_url = "http://localhost:8080/v1"
# api_key = "my-secret"   # only if --api-key was set on the server
```

To verify the connection:

```bash
zeroclaw doctor
zeroclaw models refresh --provider llamacpp
zeroclaw agent -m "hello"
```

### GPU Tuning

| Flag | Purpose | Recommended Starting Value |
|---|---|---|
| `-ngl` / `--n-gpu-layers` | Layers offloaded to GPU | `99` for full offload on Apple Silicon; tune on NVIDIA |
| `--ctx-size` | Context window (tokens) | `8192` – `131072` depending on VRAM |
| `--threads` | CPU inference threads | Number of physical CPU cores |
| `--batch-size` | Prompt evaluation batch size | `512` – `2048` |
| `--flash-attn` | FlashAttention (reduces VRAM) | Enable on supported hardware |
| `--mmap` | Memory-map model file | Enabled by default; disable on slow storage |
| `--numa` | NUMA-aware memory (multi-socket) | Enable on multi-socket servers |

Full offload on Apple Silicon:

```bash
llama-server \
  --model /path/to/model.gguf \
  --port 8080 \
  -ngl 99 \
  --ctx-size 32768 \
  --flash-attn \
  --batch-size 512
```

NVIDIA (40 GB A100, 70B Q4 model):

```bash
llama-server \
  --model /path/to/llama-3-70b-instruct.Q4_K_M.gguf \
  --port 8080 \
  --n-gpu-layers 80 \
  --ctx-size 16384 \
  --flash-attn \
  --batch-size 512 \
  --threads 8
```

### Speculative Decoding

Speculative decoding uses a small draft model to speculatively generate tokens that are then
verified by the main model in parallel. This reduces latency at the cost of memory.

```bash
llama-server \
  --model /path/to/llama-3-70b-instruct.Q4_K_M.gguf \
  --model-draft /path/to/llama-3-8b-instruct.Q4_K_M.gguf \
  --port 8080 \
  -ngl 99 \
  -ngld 99 \            # GPU layers for the draft model
  --draft-max 16 \      # max speculative tokens per step
  --draft-min 5 \       # min before validation
  --draft-p-min 0.8     # minimum acceptance probability
```

Draft model guidelines:
- Use the same model family as the main model (same tokenizer).
- Draft model should be 4–10× smaller than the main model.
- Speculative decoding works best with repetitive or predictable text; gains vary by workload.

---

## Approach 2: Ollama Wrapper

Ollama internally uses llama.cpp for GGUF inference. This approach trades fine-grained GPU
control for simpler model management.

### Prerequisites

- [Ollama](https://ollama.com) installed and running (`ollama serve`)
- Target model pulled: `ollama pull llama3.3:70b`

### ZeroClaw Config

```toml
[provider]
name = "ollama"
model = "llama3.3:70b"

[provider.options]
api_url = "http://localhost:11434"
```

For a fine-tuned GGUF loaded via a custom Modelfile:

```bash
# Create Modelfile pointing at your GGUF
cat > Modelfile <<'EOF'
FROM /path/to/my-finetuned.gguf
PARAMETER num_gpu 99
PARAMETER num_ctx 32768
EOF

ollama create my-finetuned -f Modelfile
```

Then in `config.toml`:

```toml
[provider]
name = "ollama"
model = "my-finetuned"
```

### GPU Tuning via Modelfile

Ollama exposes llama.cpp parameters through `PARAMETER` directives in the Modelfile:

```
FROM /path/to/model.gguf
PARAMETER num_gpu    99        # GPU layers (-ngl)
PARAMETER num_ctx    32768     # context size
PARAMETER num_batch  512       # batch size
PARAMETER num_thread 8         # CPU threads
```

After editing, rebuild: `ollama create <name> -f Modelfile`

### Reasoning Toggle

Control thinking/reasoning behavior for models that support it:

```toml
[runtime]
reasoning_enabled = true   # sends think: true to Ollama
```

---

## Choosing a Quantization

| Quant | Size vs Q8 | Quality | Use Case |
|---|---|---|---|
| Q2_K | ~30% | Lowest | Emergency low-VRAM |
| Q4_K_M | ~50% | Good | Default recommendation |
| Q5_K_M | ~60% | Better | When VRAM allows |
| Q6_K | ~70% | Very good | Near-lossless |
| Q8_0 | 100% | Near-exact | Benchmarking / best quality |

For most workloads, `Q4_K_M` offers the best quality-per-VRAM tradeoff.

---

## Troubleshooting

**`zeroclaw doctor` shows connection refused**
The server is not running or is on the wrong port. Confirm `llama-server` or `ollama serve` is
up and that `api_url` in `config.toml` matches.

**CUDA out of memory**
Reduce `--n-gpu-layers` (offload fewer layers) or switch to a smaller quantization.

**Slow time-to-first-token**
Increase `--batch-size`. On Apple Silicon, ensure `-ngl 99` is set and Metal is not falling
back to CPU (check llama-server startup logs for "Metal" confirmation).

**Speculative decoding not helping**
Confirm the draft model uses the same tokenizer family. Try reducing `--draft-max` to 8.

**Model name not found after `models refresh`**
For `llamacpp`, the reported model name comes from the GGUF metadata. Check
`zeroclaw models list` and use the exact string shown under the `llamacpp` provider.
