# ZeroClaw Project Context

ZeroClaw is an autonomous agent runtime optimized for performance, security, and hardware extensibility.

## Core Philosophy
- **Security-First**: Built-in access control, secret management, and deterministic guardrails.
- **Trait-Driven**: Highly modular architecture using Rust traits for Providers, Channels, Tools, Memory, and Skills.
- **Hardware Native**: Direct support for peripherals (STM32, RPi GPIO) through dedicated traits.
- **Performance**: Rust-first implementation for minimal latency and high efficiency.

## Repository Structure
- `src/agent/`: Central orchestration and tool execution loop.
- `src/skills/`: Unified skill system for extending agent capabilities.
- `src/hooks/`: Lifecycle events for startup, shutdown, and validation.
- `src/security/`: Policy enforcement and secret storage.
- `src/peripherals/`: Hardware abstraction layer.

## Architecture Highlights
The system uses a `HookRunner` to fire events at various stages of the agent's lifecycle, ensuring modularity without core logic pollution. The `Skill` trait allows for dynamic injection of tools and prompt instructions based on the current context.
