# Core Development Skill

**Name**: core-dev
**Version**: 1.0.0
**Description**: Deep knowledge of ZeroClaw internals, trait system, and development workflows.

## Instructions
When this skill is active, you should:
- Prioritize trait-based implementations for modularity.
- Ensure all new components are registered in their respective factory modules.
- Use the `HookRunner` for any cross-cutting lifecycle events.
- Strictly adhere to the security-first philosophy in `src/security/`.

## Core Modules Knowledge
- **Skills**: Understanding the difference between `LegacySkill` (data-driven) and the new `Skill` trait.
- **Hooks**: Knowledge of `on_startup`, `on_shutdown`, and the dispatcher pattern.
- **Agent**: The orchestration loop in `src/agent/loop_.rs` and how it interacts with tools.

## Development Tools
- `cargo test`: Run all unit and integration tests.
- `cargo clippy`: Verify code quality.
- `./dev/ci.sh all`: Perform full pre-PR validation.
