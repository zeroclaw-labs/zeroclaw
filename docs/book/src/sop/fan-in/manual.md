# SOP Fan-In: Manual

A manual trigger starts a run from inside an agent turn, not from an external event. The agent calls the `sop_execute` tool, naming the SOP to run. There is no listener and no event source to configure; the run begins when the agent decides to start it.

Use a manual trigger when the decision to run belongs to the agent's reasoning rather than to an external signal. This is the path the [worked example](../example.md) uses: a release arrives over a channel, the agent reasons about it, and then fires the SOP itself.

## Defining it

A SOP with a `manual` trigger has no match fields. See [Syntax](../syntax.md) for the trigger block. Validate and inspect it the same way as any other SOP:

```sh
zeroclaw sop validate
zeroclaw sop list
zeroclaw sop show <name>
```

## Starting one from the dashboard

The dashboard's run button (`POST /sops/{name}/run`) emits the same manual event, so a `manual` trigger is reachable from two sides: an agent turn, and an operator with no agent behind them. A run started from the dashboard is driven headlessly, exactly like a cron run.

That difference shows up in ownership. Through `sop_execute` the calling agent owns the run, so `agent` is optional; a dashboard-started run has no agent to inherit, so the endpoint refuses a procedure whose `execute` steps declare no owner (`422`, naming the steps) rather than starting a run that would fail its first step. Authoring warns about this instead of blocking, so a procedure meant only for `sop_execute` still saves. Set `agent` in `SOP.toml` (or on the step) to make a manual SOP startable from the dashboard.

## Approve and observe

Runs that hit a checkpoint pause as `WaitingApproval`. Clear or inspect them with the CLI (`zeroclaw sop list`, `zeroclaw sop approve`) or out-of-band over the [gateway API](../../gateway/api.md) approval endpoints (`GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`).

## See also

- [Worked example](../example.md): channel delivery plus `sop_execute`
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md): the SOP file format
