# ZeroClaw Architecture

This document provides an overview of ZeroClaw's architecture and design principles.

## Overview

ZeroClaw is built on a **microkernel architecture** with clear separation of concerns:

- **Gateway**: HTTP/gRPC server and web dashboard
- **Runtime**: Agent loop, security, cron, skills, observability
- **Channels**: 30+ messaging platform integrations
- **Providers**: LLM and embedding model backends
- **Tools**: Shell, file, browser, memory operations
- **Config**: Schema-driven configuration with migration
- **Memory**: Markdown, SQLite, vector embeddings
- **Plugins**: WASM plugin system
- **Hardware**: USB discovery, GPIO, peripherals

## Workspace Structure

The repository is organized as a Cargo workspace:

```
crates/
├── zeroclaw-api/          # Public traits (Provider, Channel, Tool, Memory)
├── zeroclaw-config/       # Config schema and loading
├── zeroclaw-providers/    # Model provider implementations
├── zeroclaw-channels/     # Channel integrations
├── zeroclaw-tools/        # Tool execution surface
├── zeroclaw-runtime/      # Agent runtime and security
├── zeroclaw-memory/       # Memory backends
├── zeroclaw-gateway/      # Webhook/gateway server
├── zeroclaw-tui/          # Onboarding wizard
├── zeroclaw-plugins/      # WASM plugin system
├── zeroclaw-infra/        # Shared utilities
├── zeroclaw-hardware/     # Hardware peripherals
├── zeroclaw-tool-call-parser/ # Tool call parsing
├── zeroclaw-macros/       # Custom derive macros
└── robot-kit/             # Robotics extensions
```

## Key Design Principles

- **Zero overhead**: Minimal binary size (6.6 MB without default features)
- **100% Rust**: Safety and performance
- **Modular**: Each crate has a single responsibility
- **Trait-driven**: Extension via implementing core traits
- **Security-first**: Sandboxing, approval flows, least privilege

## Detailed Documentation

- [Security architecture](security/agnostic-security.md)
- [Contributing guidelines](contributing/README.md)
- [Architecture decisions](architecture/decisions/)
- [Full API reference](reference/api/)

For a complete map of the codebase, see [AGENTS.md](AGENTS.md).
