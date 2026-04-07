# CLAUDE.md — ZeroClaw (Claude Code)

> **Shared instructions live in [`AGENTS.md`](./AGENTS.md).**
> This file contains only Claude Code-specific directives.

## Claude Code Settings

Claude Code should read and follow all instructions in `AGENTS.md` at the repository root for project conventions, commands, risk tiers, workflow rules, and anti-patterns.

## Hooks

_No custom hooks defined yet._

## Slash Commands

_No custom slash commands defined yet._

## Active Technologies
- Rust (edition 2021, stable toolchain) + tokio, serde, reqwest, teloxide (Telegram), slack-morphism (Slack) (001-simplify-channels-providers)
- SQLite (session_sqlite), JSONL (session_store), in-memory (001-simplify-channels-providers)

## Recent Changes
- 001-simplify-channels-providers: Added Rust (edition 2021, stable toolchain) + tokio, serde, reqwest, teloxide (Telegram), slack-morphism (Slack)
