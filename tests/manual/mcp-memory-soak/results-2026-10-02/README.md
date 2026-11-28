# Linux/glibc comparison on 2026-10-02

## Result and scope

The sustained baseline RSS growth described in
[issue #8642](https://github.com/zeroclaw-labs/zeroclaw/issues/8642) was **not
reproduced in this bounded experiment**. Both historical binaries completed a
35-minute run, and both had slightly negative descriptive RSS/PSS slopes over
the common post-warmup work range. This does **not** establish that the original
problem is fixed, that the builds are equivalent, or that the issue can close.

This contributes a runnable diagnostic and the observed negative reproduction
result to the remaining verification request. The production repair in
[PR #9208](https://github.com/zeroclaw-labs/zeroclaw/pull/9208) predates this work;
no production Rust code is changed here.

## Environment and comparison

| Control | Value |
| --- | --- |
| Before source | `c7f4435b95103b645a5e80585b3b2af412b0cd33` |
| After source | `5bf683defcd0fd9237eb640b6425e9b978c8dd20` |
| Harness used for both runs | `5a527bb137b23c87d31b854fa7dd14a0ad75ffa1` |
| Platform | Native Linux aarch64, Docker Desktop 29.7.2, glibc 2.41 |
| Compiler | rustc 1.96.1, LLVM 22.1.2 |
| Build | Repository `ci` profile, thin LTO, 16 codegen units; `agent-runtime,gateway` only |
| Allocator setting | `MALLOC_ARENA_MAX=2`, as in the published recipe |
| Runtime resource limits | 2 GiB memory, 2 CPUs |
| Mock limits | 128 MiB memory, 0.5 CPU, at most 8 connection handlers |
| Driver limit | 128 MiB memory |
| Workload | One fresh WebSocket session; 36 HTTP MCP tools; 25 actual MCP calls and 26 provider requests per completed turn |
| Duration / warmup / sampling | 2,100 s / 300 s / 5 s; 120 s maximum per turn |

The before source is the verified merge-base with the after source, isolating the
historical repair from later master changes. The pinned build inputs and
synthetic configuration are in the parent directory. The runtime binaries were
measured directly, not identified solely by the operator-supplied revision flag:

- Before SHA-256: `3eed79fd176d205db244cc832c5ab8c29e9e2408fd5c18cdc7829f4371495b15`
- After SHA-256: `b44cf62af18e1353a07dc3eb95f572ed87a36714553961da22f332123814875b`

Both runs used the same internal Docker network without published ports or
external egress, synthetic provider credentials, fresh processes and fresh data.
They ran sequentially, with no concurrent compilation. Four unrelated local
application containers remained running throughout; background host activity was
not randomized or eliminated. The fixture and configuration did not change
between these two runs.

## Completion and workload checks

| Observed quantity | Before | After |
| --- | ---: | ---: |
| Elapsed seconds | 2,100.156 | 2,100.106 |
| Completed turns | 9,277 | 10,614 |
| Actual MCP calls | 231,925 | 265,350 |
| Provider requests | 241,202 | 275,964 |
| RSS/PSS sample rows | 422 | 422 |
| Driver exit code | 0 | 0 |
| Error records / final mock errors | 0 / 0 | 0 / 0 |

Every completed-turn record was checked for cumulative `25 * turns` MCP calls,
`26 * turns` provider requests, and 25 tool-call, 25 tool-result and one done
WebSocket frame. The live driver also required the expected terminal response
and stable target process identity. The provider-facing MCP inventory remained
36 definitions, 113,869 canonical JSON bytes, SHA-256
`5b7bb3dd26175da100a9136156898506e5482567fb65f2893aa86b7de5913052` in both runs.

## Matched-work descriptive results

After discarding each run's first 300 seconds, the common completed-turn range
is **1,512–9,277**. Sampling is time-based: the rows span the same work range but
are not exact pairs at identical turn counts. This retains 355 before samples
(335.011–2,100.156 s) and 305 after samples (300.009–1,820.009 s).

For start/end medians, the first and last 500-turn windows of that range are
1,512–2,011 and 8,778–9,277, inclusive. They contain 21/27 before samples and
21/20 after samples. These windows are an explicit descriptive reporting choice,
not a preregistered statistical test.

| Metric (KiB) | Before | After |
| --- | ---: | ---: |
| RSS start-window median | 51,232 | 48,716 |
| RSS end-window median | 51,436 | 48,728 |
| RSS median-window delta | +204 | +12 |
| PSS start-window median | 50,017 | 47,509 |
| PSS end-window median | 50,221 | 47,521 |
| PSS median-window delta | +204 | +12 |
| RSS median across the matched range | 51,180 | 48,824 |
| PSS median across the matched range | 49,965 | 47,617 |
| RSS OLS slope (KiB/min) | -4.092 | -2.110 |
| PSS OLS slope (KiB/min) | -4.098 | -2.110 |

Small positive differences between the start/end-window medians coexist with
slightly negative whole-range slopes: the series are not monotonic. Neither
summary is suppressed. The lower after median is an observation from one
sequential pair, not an estimated causal memory saving. The unequal turn totals
are not presented as a throughput benchmark.

## Recompute from the included sample exports

From the repository root:

```sh
python3 tests/manual/mcp-memory-soak/analyze.py \
  tests/manual/mcp-memory-soak/results-2026-10-02/before.samples.jsonl \
  tests/manual/mcp-memory-soak/results-2026-10-02/after.samples.jsonl
```

The exports retain all metadata, sample rows and the completion record, removing
the local monotonic origin, process-identification fields and output filename.
Per-turn trace rows are omitted from the public exports; the completion/counter
validation above was performed on the original full artifacts. The exports are
sufficient to recompute the memory summaries, not to independently replay every
per-turn frame check. Runtime container logs were size-capped and only retained
tails; they are not full-run logs or the basis for claiming that every turn
passed. The full JSONL artifacts provide that validation evidence.
Original full-artifact SHA-256 values:

- Before: `f09647ecdcb7b089ce27c8d0aa3e44e8b28a5e62bef15dab22b0a3f7518781a9`
- After: `f8a49cba6cafc9d22fa22d4e48eb96ba9735751e527b5482f4636e643428c672`

Multiply the analyzer's KiB/second slopes by 60 for the table. To reproduce the
median-window rows, select sample rows in each inclusive completed-turn window
above and take the median of `rss_kib` or `pss_kib` separately.

## Deviations, excluded attempt and next diagnostic boundary

- The default release/fat-LTO build exceeded the 8 GiB Docker build environment.
  Both measured binaries instead used the same `ci` profile. These results do
  not establish behavior of all distributed release builds.
- The initial before attempt stopped at about 1,219 seconds after a metrics HTTP
  timeout. It has no completion marker and was excluded entirely, not spliced
  into this pair. An accepted idle connection deterministically blocked the
  original single-threaded mock. Bounded concurrency fixed that demonstrated
  fixture weakness; the observed historical timeout is compatible with it, but
  its unique cause was not proved. Both reported runs use the repaired fixture.
- Schemas have three simple properties and a padded description, not the shape
  of deeply nested real-world schemas. HTTP/1.0 closure differs from some real
  MCP server keep-alive behavior. No live model or paid provider was used.
- The configured history target of 16 preserves whole turns in these historical
  binaries; it is not an observed hard limit of 16 messages. The active turn can
  be larger. Session persistence was disabled.
- This is one fresh session and one run per revision, not the originally
  reported parallel/restored conversations. Time samples are dependent. There
  are no independent-run confidence intervals, p-values, equivalence tests or
  leak-freedom claims. Heap allocation attribution was not measured.

The next useful investigation is to identify which missing workload property
(schema allocation shape, parallel/restored conversations, allocator settings or
release profile) recreates growth on the baseline. This report does not choose
one as the cause or claim that a longer repetition of this same fixture will
resolve that question. Keep issue #8642 open for that remaining evidence.
