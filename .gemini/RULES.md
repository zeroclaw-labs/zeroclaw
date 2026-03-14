# ZeroClaw AI Rules & Constraints

Follow these rules strictly when modifying the ZeroClaw codebase.

## Rust Style & Idioms
- **Result Handling**: Always use `Result` for fallible operations. Avoid `unwrap()` and `expect()` in production code.
- **Traits**: Prefer trait-based abstractions for extensibility (refer to `src/**/traits.rs`).
- **Dependencies**: Do not add heavy dependencies for minor convenience.
- **Clippy**: Ensure code passes `cargo clippy --all-targets -- -D warnings`.

## Security Constraints
- **Secrets**: Never log secrets. Use `src/security/` abstractions for sensitive data.
- **Access Control**: Do not bypass or weaken existing security policies.
- **Input Validation**: Sanitize all external inputs (e.g., from channels or tools).

## PR & Workflow Rules
- **One Concern**: Each PR should address exactly one feature or fix.
- **Risks**: Classify changes by risk (Low/Medium/High) as per `AGENTS.md`.
- **Minimalism**: No speculative abstractions or unused config keys.

## Skill & Hook Integration
- **Hooks**: Use `HookHandler` for cross-cutting concerns (logging, audit, etc.).
- **Skills**: When adding capabilities, prefer the unified `Skill` trait over ad-hoc tool additions.
