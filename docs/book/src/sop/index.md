# Standard Operating Procedures (SOP)

SOPs are deterministic procedures executed by the `SopEngine`. They provide explicit trigger matching, approval gates, and auditable run state.

- [How SOPs run](./how-it-works.md): the runtime contract, event flow, and a getting-started walkthrough.
- [Syntax](./syntax.md): required file layout and trigger/step syntax.
- [Cookbook](./cookbook.md): reusable SOP patterns.
- [SOP Fan-In](./fan-in/overview.md): event fan-in to the SOP engine. Authenticated webhooks, MQTT, filesystem, and AMQP are wired live sources, and the daemon's periodic maintenance tick dispatches cron triggers; peripheral and calendar triggers are defined and matched but not yet routed to a live event source.
- [Observability](./observability.md): where run state and audit entries are stored.
- [Worked Example](./example.md): the stagehand StageX auto-update bot, from build to draft PR, driven by a deterministic SOP over an AMQP feed.
