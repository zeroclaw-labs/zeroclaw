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

## Approve and observe

Runs that hit a checkpoint pause as `WaitingApproval`. Clear or inspect them with the CLI (`zeroclaw sop list`, `zeroclaw sop approve`) or out-of-band over the [gateway API](../../gateway/api.md) approval endpoints (`GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`).

## See also

- [Worked example](../example.md): channel delivery plus `sop_execute`
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md): the SOP file format
