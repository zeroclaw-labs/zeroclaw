# ZeroClaw Config Reference (Operator-Oriented)

This is a high-signal reference for common config sections and defaults.

Last verified: **February 18, 2026**.

Config file path:

- `~/.zeroclaw/config.toml`

## Core Keys

| Key | Default | Notes |
|---|---|---|
| `default_provider` | `openrouter` | provider ID or alias |
| `default_model` | `anthropic/claude-sonnet-4-6` | model routed through selected provider |
| `default_temperature` | `0.7` | model temperature |

## `[agent]`

| Key | Default | Purpose |
|---|---|---|
| `max_tool_iterations` | `10` | Maximum tool-call loop turns per user message across CLI, gateway, and channels |

Notes:

- Setting `max_tool_iterations = 0` falls back to safe default `10`.
- If a channel message exceeds this value, the runtime returns: `Agent exceeded maximum tool iterations (<value>)`.

## `[gateway]`

| Key | Default | Purpose |
|---|---|---|
| `host` | `127.0.0.1` | bind address |
| `port` | `3000` | gateway listen port |
| `require_pairing` | `true` | require pairing before bearer auth |
| `allow_public_bind` | `false` | block accidental public exposure |

## `[memory]`

| Key | Default | Purpose |
|---|---|---|
| `backend` | `sqlite` | `sqlite`, `lucid`, `markdown`, `none` |
| `auto_save` | `true` | automatic persistence |
| `embedding_provider` | `none` | `none`, `openai`, or custom endpoint |
| `vector_weight` | `0.7` | hybrid ranking vector weight |
| `keyword_weight` | `0.3` | hybrid ranking keyword weight |

## `[cognitive]`

Controls cognitive processing for the agent, including sentiment prediction and preference tracking with emotional state persistence.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Enable/disable cognitive processing (sentiment prediction, preference tracking) |
| `persistence_path` | string | `data/cognitive` | Path for emotional state persistence |
| `save_interval` | u64 | `10` | Save emotional state every N turns |

Example:

```toml
[cognitive]
enabled = true
persistence_path = "data/cognitive"
save_interval = 10
```

## `[cosmic_brain]`

Enables and configures the 17-module cognitive architecture with memory graph, spreading activation, free energy modeling, and multi-agent integration.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Enable/disable the 17-module cognitive architecture |
| `graph_max_nodes` | usize | `10000` | Maximum nodes in cosmic memory graph |
| `graph_prune_threshold` | usize | `8000` | Prune graph when it exceeds this node count |
| `spreading_activation_decay` | f32 | `0.7` | Decay factor for spreading activation (0.0–1.0) |
| `spreading_activation_max_hops` | u32 | `4` | Maximum hops for spreading activation propagation |
| `free_energy_capacity` | usize | `1000` | Prediction buffer size for free energy modeling |
| `free_energy_update_threshold` | f64 | `0.3` | Surprise threshold for model update |
| `free_energy_act_threshold` | f64 | `0.5` | Free energy threshold for action triggering |
| `integration_tick_secs` | u32 | `60` | Integration meter tick interval (seconds) |
| `persistence_dir` | string | `data/cosmic` | Directory for cosmic state persistence |
| `multi_agent_pool_size` | usize | `4` | Agent pool size for parallel execution |
| `policy_conflict_resolution` | string | `highest_layer` | Policy conflict resolution strategy |
| `counterfactual_max_scenarios` | usize | `10` | Maximum counterfactual scenarios to explore |
| `consolidation_interval_secs` | u32 | `3600` | Memory consolidation interval (seconds) |
| `drift_window_size` | usize | `50` | Belief drift detection window size |
| `drift_threshold` | f64 | `0.1` | Belief drift detection threshold |
| `thalamus_threshold` | f64 | `0.3` | Sensory thalamus salience threshold |
| `workspace_max_active` | usize | `5` | Maximum active items in global workspace |

Example:

```toml
[cosmic_brain]
enabled = true
graph_max_nodes = 10000
graph_prune_threshold = 8000
spreading_activation_decay = 0.7
spreading_activation_max_hops = 4
free_energy_capacity = 1000
free_energy_update_threshold = 0.3
free_energy_act_threshold = 0.5
integration_tick_secs = 60
persistence_dir = "data/cosmic"
multi_agent_pool_size = 4
policy_conflict_resolution = "highest_layer"
counterfactual_max_scenarios = 10
consolidation_interval_secs = 3600
drift_window_size = 50
drift_threshold = 0.1
thalamus_threshold = 0.3
workspace_max_active = 5
```

## `[consciousness]`

Enables and configures the consciousness orchestrator, which runs multi-agent debate rounds to reach consensus before committing to actions.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Enable/disable consciousness orchestrator |
| `debate_rounds` | usize | `3` | Number of multi-agent debate rounds per tick |
| `approval_threshold` | f64 | `0.85` | Consensus threshold for early debate exit |
| `bus_capacity` | usize | `256` | Shared message bus capacity |

Example:

```toml
[consciousness]
enabled = false
debate_rounds = 3
approval_threshold = 0.85
bus_capacity = 256
```

## `[channels_config]`

Top-level channel options are configured under `channels_config`.

Examples:

- `[channels_config.telegram]`
- `[channels_config.discord]`
- `[channels_config.whatsapp]`
- `[channels_config.email]`

See detailed channel matrix and allowlist behavior in [channels-reference.md](channels-reference.md).

## Security-Relevant Defaults

- deny-by-default channel allowlists (`[]` means deny all)
- pairing required on gateway by default
- public bind disabled by default

## Validation Commands

After editing config:

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

## Related Docs

- [channels-reference.md](channels-reference.md)
- [providers-reference.md](providers-reference.md)
- [operations-runbook.md](operations-runbook.md)
- [troubleshooting.md](troubleshooting.md)
