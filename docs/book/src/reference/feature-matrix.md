# Feature and support matrix

A single-page inventory of what ZeroClaw supports today, what is experimental
or partial, and what is planned or intentionally out of scope. It exists so a
reader can answer "does ZeroClaw do X?" without walking the whole book. Each row
links to the canonical page, so the setup detail lives there, not here.

This page describes factual support status. It is not a marketing comparison,
and it does not mark roadmap items as implemented unless the linked docs or code
back that claim.

## Status legend

| Status | Meaning |
|---|---|
| Supported | Implemented and documented; safe to rely on |
| Partial | Works with caveats or covers a subset of the surface |
| Experimental | Present but may change; validate before depending on it |
| Planned | Tracked but not yet implemented; linked to its issue or RFC |
| Not planned | Explicitly out of scope |

When a row's status is uncertain, it is marked Partial with a note rather than
overstated.

## Runtime and deployment modes

| Mode | Status | Reference |
|---|---|---|
| Local CLI (`zeroclaw agent`) | Supported | [Quickstart](../getting-started/quickstart.md) |
| Daemon / OS service | Supported | [Service](../ops/service.md) |
| Gateway HTTP + web dashboard | Supported | [Gateway HTTP API](../gateway/api.md), [Web dashboard](../gateway/web-dashboard.md) |
| ZeroCode terminal UI | Supported | [ZeroCode](../getting-started/zerocode.md) |
| Container | Supported | [Container](../setup/container.md) |
| Linux / macOS / Windows | Supported | [Linux](../setup/linux.md), [macOS](../setup/macos.md), [Windows](../setup/windows.md) |
| FreeBSD / NixOS | Supported | [FreeBSD](../setup/freebsd.md), [NixOS](../setup/nixos.md) |
| SBC / edge (Raspberry Pi class) | Supported | [Hardware](../hardware/index.md) |
| VPS / cloud VM | Supported | [Network deployment](../ops/network-deployment.md) |
| Hardware / peripheral use | Partial | `--features hardware`; see [Hardware](../hardware/index.md) |

## Provider and model support

| Capability | Status | Reference |
|---|---|---|
| Hosted providers | Supported | [Providers overview](../providers/overview.md) |
| OpenAI-compatible / custom endpoint | Supported | [Custom providers](../providers/custom.md) |
| Local models (Ollama, self-hosted) | Supported | [Providers overview](../providers/overview.md) |
| Routing and fallback | Supported | [Routing](../providers/routing.md) |
| Streaming (tokens, tool calls, reasoning) | Supported | [Streaming](../providers/streaming.md) |
| Tool calling (native and text protocol) | Supported | [Tools overview](../tools/overview.md) |
| Vision input | Partial | Per-provider; configurable vision route |
| Reasoning / thinking models | Supported | [Streaming](../providers/streaming.md) |
| Voice (TTS) and speech-to-text | Partial | Per-agent voice routing; [Voice](../channels/voice.md) |

## Channel support

The canonical per-channel list and feature flags live in the
[Channels overview](../channels/overview.md). Summary:

| Channel | Status | Reference |
|---|---|---|
| CLI | Supported | [Quickstart](../getting-started/quickstart.md) |
| Discord | Supported | [Discord](../channels/discord.md) |
| Slack | Supported | [Slack](../channels/slack.md) |
| Telegram | Supported | [Other chat platforms](../channels/chat-others.md) |
| Matrix | Supported | [Matrix](../channels/matrix.md) |
| Mattermost | Supported | [Mattermost](../channels/mattermost.md) |
| Nextcloud Talk | Supported | [Nextcloud Talk](../channels/nextcloud-talk.md) |
| Signal | Supported | [Signal](../channels/signal.md) |
| LINE | Supported | [LINE](../channels/line.md) |
| WhatsApp (Cloud API and Web) | Supported | [WhatsApp](../channels/whatsapp.md) |
| Email (IMAP/SMTP, Gmail push) | Supported | [Email](../channels/email.md) |
| Webhook (inbound HTTP) | Supported | [Webhooks](../channels/webhook.md) |
| ACP (editor / IDE sessions) | Supported | [ACP](../channels/acp.md) |
| MQTT | Supported | [MQTT](../channels/mqtt.md) |
| AMQP | Supported | [AMQP](../channels/amqp.md) |
| Filesystem | Supported | [Filesystem](../channels/filesystem.md) |
| Other platforms (iMessage, WeChat, DingTalk, Lark, QQ, IRC, Notion) | Partial | [Other chat platforms](../channels/chat-others.md) |

## Tool families

The canonical tool inventory lives in the [Tools overview](../tools/overview.md).
Summary:

| Family | Status | Reference |
|---|---|---|
| Shell / file / search | Supported | [Tools overview](../tools/overview.md) |
| Browser / web search / fetch | Supported | [Browser automation](../tools/browser.md) |
| HTTP request | Supported | [Tools overview](../tools/overview.md) |
| Long-term memory | Supported | [Tools overview](../tools/overview.md) |
| Scheduling (cron, schedule) | Supported | [Tools overview](../tools/overview.md) |
| Subagents / delegation | Supported | [Delegation and SubAgents](../agents/delegation.md) |
| Relationship memory (knowledge) | Supported | [Relationship memory](../tools/relationship-memory.md) |
| Skills | Supported | [Skills](../tools/skills.md) |
| MCP (external tool servers) | Supported | [MCP](../tools/mcp.md) |
| Hardware probes (GPIO/I2C/SPI) | Partial | `--features hardware` |
| PDF read | Partial | `--features rag-pdf` |
| SOP tools | Supported | Registered when `sop.sops_dir` is configured |

## ZeroClaw and OpenClaw

ZeroClaw is a from-scratch runtime, not a fork. Where a concept maps onto
OpenClaw, the migration and parity notes live with the relevant feature page
(providers, channels, tools) rather than in a standalone comparison. This page
does not assert parity that the linked docs do not support.

## Integration prerequisites

Some capabilities require external infrastructure. Where a channel or tool needs
a gateway, public webhook, tunnel, cloud account, local model, or plugin, that
requirement is stated on the linked page. Consult the specific channel, provider,
or tool reference before assuming a setup works without one.
