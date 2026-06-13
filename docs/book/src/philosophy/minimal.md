# Minimal: in binary size, dependencies, and surface area

ZeroClaw is written in Rust and optimised for a small binary and fast startup. The microkernel split ([RFC #5574](https://github.com/zeroclaw-labs/zeroclaw/issues/5574)) factors functionality behind feature flags so you only ship what you use: the foundation builds with `--no-default-features`, and channels, hardware, and the gateway are opt-in. A typical release build lands around 26 MiB; a minimal feature set trims it further.

The same discipline applies to the agent's prompt surface. Tool descriptions are [Fluent](https://projectfluent.org/)-localised and terse. There are no hidden system prompts injecting personality. The model sees what you configure.
