# Standard Operating Procedures (SOP)

SOPs are deterministic procedures executed by the `SopEngine`. They provide explicit trigger matching, approval gates, and auditable run state.

- [How SOPs run](./how-it-works.md): the runtime contract, event flow, and a getting-started walkthrough.
- [Syntax](./syntax.md): required file layout and trigger/step syntax.
- [Cookbook](./cookbook.md): reusable SOP patterns.
- [Connectivity](./connectivity.md): event fan-in to the SOP engine. MQTT is the wired live source; webhook, cron, and peripheral triggers are defined and matched but not yet routed to a live event source.
- [Observability](./observability.md): where run state and audit entries are stored.
