# Feature and support matrix

A single-page inventory of what ZeroClaw supports today, compared against
OpenClaw and Hermes. It exists so a reader can answer "does ZeroClaw do X?"
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
| Local CLI (`zeroclaw agent`) | ✅ Supported | [Quickstart](../getting-started/quickstart.md) |
| Daemon / OS service | ✅ Supported | [Service](../ops/service.md) |
| Gateway HTTP + web dashboard | ✅ Supported | [Gateway HTTP API](../gateway/api.md), [Web dashboard](../gateway/web-dashboard.md) |
| ZeroCode terminal UI | ✅ Supported | [ZeroCode](../getting-started/zerocode.md) |
| Container | ✅ Supported | [Container](../setup/container.md) |
| Linux / macOS / Windows | ✅ Supported | [Linux](../setup/linux.md), [macOS](../setup/macos.md), [Windows](../setup/windows.md) |
| FreeBSD / NixOS | ✅ Supported | [FreeBSD](../setup/freebsd.md), [NixOS](../setup/nixos.md) |
| SBC / edge (Raspberry Pi class) | ✅ Supported | [Hardware](../hardware/index.md) |
| VPS / cloud VM | ✅ Supported | [Network deployment](../ops/network-deployment.md) |

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
