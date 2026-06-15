# Security-first, with escape hatches

Local-first doesn't mean consequence-free. An agent that can execute shell commands, call HTTP endpoints, and write files is a privileged process. The default autonomy level is `supervised`: medium-risk operations require approval, high-risk operations are blocked.

The runtime ships with:

- Workspace boundaries (the agent can only touch paths inside its configured workspace)
- Command allow/deny lists
- Shell-policy validation
- OS-level sandboxes (Docker, Firejail, Bubblewrap, Landlock on Linux; Seatbelt on macOS)
- Tool receipts: a cryptographically-linked audit log of every tool call
- Emergency stop (`zeroclaw estop`) and OTP-gated actions

For developers and home-lab users who understand the trade-offs, there's [YOLO mode](../getting-started/yolo.md): one config preset that disables the guardrails. It's loud, logged, and obviously named. Not the default.
