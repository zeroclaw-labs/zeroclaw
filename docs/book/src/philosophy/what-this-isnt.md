# What this isn't

- **Not a SaaS.** There's no hosted version, no account system, no billing.
- **Not only a chat UI.** It ships chat front-ends, the [zerocode](../zerocode/overview.md) terminal interface, the web dashboard, and chat-platform channels, but those sit on top of an agent runtime. The runtime is the product; the chat surfaces are how you reach it, alongside the CLI, the REST gateway, and the ACP JSON-RPC interface.
- **Not a framework.** You don't build apps on top of ZeroClaw. You configure it and connect channels.
- **Not a toy.** Production deployments run 24/7 on homelab SBCs, VPSes, and cloud VMs. The `zeroclaw service` subcommand manages systemd / launchctl / Windows Service registration out of the box.
