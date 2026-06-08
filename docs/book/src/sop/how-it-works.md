# How SOPs run

## Runtime contract

- SOP definitions are loaded from `<workspace>/sops/<sop_name>/SOP.toml` plus optional `SOP.md`.
- CLI `zeroclaw sop` currently manages definitions only: `list`, `validate`, `show`.
- SOP runs are started by MQTT event fan-in or by the in-agent tool `sop_execute`. The webhook, cron, and peripheral trigger types are defined and matched but not yet wired to a live event source (see [Connectivity](./connectivity.md)).
- Run progression uses tools: `sop_status`, `sop_approve`, `sop_advance`.
- SOP audit records are persisted in the configured Memory backend under category `sop`.

## Event flow

```mermaid
graph LR
    MQTT[MQTT listener] -->|topic match| Dispatch
    TOOL[sop_execute tool] -->|manual| Dispatch
    WH[Webhook trigger] -.->|defined, unwired| Dispatch
    CRON[Cron trigger] -.->|defined, unwired| Dispatch
    GPIO[Peripheral trigger] -.->|defined, unwired| Dispatch

    Dispatch --> Engine[SOP Engine]
    Engine --> Run[SOP Run]
    Run --> Action{Action}
    Action -->|ExecuteStep| Agent[Agent Loop]
    Action -->|WaitApproval| Human[Operator]
    Human -->|sop_approve| Run
```

## Getting started

1. Set the SOP directory in `config.toml` (required for runtime SOP loading):

2. Create a SOP directory, for example:

   ```text
   ~/.zeroclaw/workspace/sops/deploy-prod/SOP.toml
   ~/.zeroclaw/workspace/sops/deploy-prod/SOP.md
   ```

3. Validate and inspect definitions:

   <div class="os-tabs-src">

   #### sh

   ```sh
   zeroclaw sop list
   zeroclaw sop validate
   zeroclaw sop show deploy-prod
   ```

   </div>

4. Trigger runs via configured event sources, or manually from an agent turn with `sop_execute`.

For trigger routing and auth details, see [Connectivity](./connectivity.md).
