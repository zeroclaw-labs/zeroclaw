# Feature and support matrix

A single-page inventory of what ZeroClaw supports today, compared against
[OpenClaw](https://github.com/openclaw/openclaw) and
[Hermes](https://github.com/NousResearch/hermes-agent). It exists so a reader can answer "does ZeroClaw do X?"
without walking the whole book.

The **ZeroClaw** column is not hand-maintained: it is regenerated on every docs
build by walking the binary's own registries (the channel inventory, the
canonical provider slots, and the default tool set), so this page cannot fall
out of sync with the code. The **OpenClaw** and **Hermes** columns come from
`docs/book/feature-matrix-parity.toml`, the single reviewable source for parity
facts the binary has no knowledge of.

## Status legend

| Status | Icon | Meaning |
|---|---|---|
| Supported | ✅ | Implemented and documented; safe to rely on |
| Partial | 🟡 | Works with caveats or covers a subset of the surface |
| Experimental | 🧪 | Present but may change; validate before depending on it |
| Planned | 📋 | Tracked but not yet implemented |
| None | ❌ | Not available on that runtime |
| Unknown | ❓ | Parity not yet recorded for that runtime |

## Runtime and deployment modes

| Mode | Status | Reference |
|---|---|---|
| Local CLI (`zeroclaw agent`) | ✅ | [Quickstart](../getting-started/quickstart.md) |
| Daemon / OS service | ✅ | [Service](../ops/service.md) |
| Gateway HTTP + web dashboard | ✅ | [Gateway HTTP API](../gateway/api.md), [Web dashboard](../gateway/web-dashboard.md) |
| ZeroCode terminal UI | ✅ | [ZeroCode](../getting-started/zerocode.md) |
| Container | ✅ | [Container](../setup/container.md) |
| Linux / macOS / Windows | ✅ | [Linux](../setup/linux.md), [macOS](../setup/macos.md), [Windows](../setup/windows.md) |
| FreeBSD / NixOS | ✅ | [FreeBSD](../setup/freebsd.md), [NixOS](../setup/nixos.md) |
| SBC / edge (Raspberry Pi class) | ✅ | [Hardware](../hardware/index.md) |
| VPS / cloud VM | ✅ | [Network deployment](../ops/network-deployment.md) |

## Channel support

Generated from the channel registry
([`ChannelsConfig::channels`](../channels/overview.md)). The canonical
per-channel setup detail lives on each channel's own page.

{{#include ../_snippets/feature-matrix-channels.md}}

## Provider slot support

Generated from the canonical model-provider slots
(`canonical_model_provider_slots`). See the
[Providers overview](../providers/overview.md) for per-provider configuration.

{{#include ../_snippets/feature-matrix-providers.md}}

## Tool support

Generated from the default tool registry (`default_tools`). The canonical tool
inventory and gating rules live in the [Tools overview](../tools/overview.md).

{{#include ../_snippets/feature-matrix-tools.md}}

## SOP support

[Standard Operating Procedures](../sop/index.md) are deterministic, trigger-matched
procedures run by the `SopEngine` with approval gates and auditable run state
([how they run](../sop/how-it-works.md), [syntax](../sop/syntax.md)). This row is
hand-recorded rather than code-walked: SOP is not part of the channel, provider,
or tool registries the tables above are generated from.

| Capability | ZeroClaw | OpenClaw | Hermes |
|---|---|---|---|
| Deterministic SOP engine (trigger match, approval gates, audited runs) | 🧪 | ❌ | 🟡 |

ZeroClaw's `SopEngine` is present but still maturing: MQTT, filesystem, and AMQP
are wired live fan-in sources, while webhook, cron, peripheral, and calendar
triggers are defined and matched but not yet routed to a live source, so the
capability is **experimental**. OpenClaw has no deterministic-procedure engine
(its exec-approval flow is a per-tool permission prompt, not a step runner), so
it is **none**. Hermes ships cron and webhook "routines" that pair a trigger with
a free-form agent prompt but no deterministic multi-step engine, approval gates,
or audited run state, so it is **partial** on the trigger side only.

## ZeroClaw, OpenClaw, and Hermes

ZeroClaw is a from-scratch runtime, not a fork. The comparison columns state
factual support status per runtime; they are not a marketing scorecard. Where a
concept maps onto OpenClaw or Hermes, the migration and parity notes live with
the relevant feature page rather than in a standalone comparison.

## Integration prerequisites

Some capabilities require external infrastructure. Where a channel or tool needs
a gateway, public webhook, tunnel, cloud account, local model, or plugin, that
requirement is stated on the linked page. Consult the specific channel, provider,
or tool reference before assuming a setup works without one.
