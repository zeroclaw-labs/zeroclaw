# FND-006: Zero Compromise in Practice — Code Health, Error Discipline, and the Production Readiness Standard

### Starting v0.7.0 · Type: Quality · Rev. 1

> **Canonical reference** · Ratified by the team · Rev. 1
> Discussion thread and full revision history: [#5653](https://github.com/zeroclaw-labs/zeroclaw/issues/5653)


--------

> A note to the team before you read this.
>
> This is the sixth document in ZeroClaw's maturity framework. The five before it
> addressed architecture, documentation, governance, engineering infrastructure, and
> collaboration — the structural and human scaffolding that surrounds the work. Each
> one answered a different question about how we build this project together. If you
> have read them all, you may have noticed a question none of them answered: yes, but
> how do we actually write it well? The architecture RFC told you what shape to build
> in. The documentation RFC told you how to record it. The governance RFC told you how
> to coordinate. The CI/CD RFC told you how to gate it. The culture RFC told you how
> to work with the people around you. None of them told you what quality looks like at
> the sentence level — inside a function, at the moment you are making a choice.
>
> That is what this document is for.
>
> The specific topics here — error handling, API documentation, test design, technical
> debt — are Rust topics on the surface. The skills they develop are not. Technology
> changes. It changes faster with each iteration than it did the time before. The tools
> you are using today — this language, this framework, this AI assistant — will be
> superseded. Some of them within the lifetime of this project. The judgment this
> document is trying to help you build will not be superseded. It will compound quietly
> in the background of every decision you make, in every language you will ever write,
> in every system you will ever build, and in work that may have nothing to do with
> software at all. That is the investment we are making in you. Not in your ability to
> write Rust. In your ability to think about quality, failure, and craft — and to carry
> that thinking with you into every tool you ever pick up, including the AI tools you
> are using today and the ones that do not exist yet.
>
> Take your time with it.

--------

## The Maturity Framework Suite

This RFC is the sixth in a set of documents that together form ZeroClaw's maturity
framework. They are designed to be read as a whole, though each stands on its own.

| RFC | Scope | Issue |
|-----|-------|-------|
| Intentional Architecture — Microkernel Transition | What we are building and how it is structured | #5574 |
| Documentation Standards and Knowledge Architecture | How we document what we build | #5576 |
| Team Organization and Project Governance | How we coordinate and make decisions | #5577 |
| Engineering Infrastructure — CI/CD Pipeline | How we build, test, and ship reliably | #5579 |
| Contribution Culture — Human Collaboration, AI Partnership, and Team Growth | How we work together and grow | #5615 |
| Zero Compromise in Practice — Code Health, Error Discipline, and the Production Readiness Standard | How we write code that lasts | this RFC |

The first five RFCs answer structural and human questions. This one answers the question
that sits inside all of them: given the structure, given the team, given the tools —
what does it mean to write the code well?

--------

## Table of Contents

1. A Development Philosophy: The Investment in Judgment
2. Honest Assessment: What the Codebase Is Telling Us
   - 2.1 The Evidence
   - 2.2 What the Numbers Do Not Show
   - 2.3 What Is Already Good
3. Gates and Standards: The Central Distinction
4. The Seven Disciplines
   - 4.1 Error Handling as a Design Concern
   - 4.2 Public API Surface as a Promise
   - 4.3 Tests as Design Feedback
   - 4.4 Technical Debt Triage
   - 4.5 Security at the Application Layer
   - 4.6 Observability as Debuggability
   - 4.7 Working Above the Floor
5. What This Means for AI-Assisted Development
6. The Portability of Craft
7. What This Means for Contributors

--------

## Revision History

| Rev | Date | Summary |
|-----|------|---------|
| 1 | 2026-04-12 | Initial draft |

--------

## 1. A Development Philosophy: The Investment in Judgment

The architecture RFC introduced a decision hierarchy that describes how every choice
in this project should flow:

```
Vision
  └── Architecture
        └── Design
              └── Implementation
                    └── Testing
                          └── Documentation
                                └── Release
```

That hierarchy answers the question of *what* to build at each layer. This RFC lives
inside the Implementation and Testing layers and asks a different question: *how well?*

The answer to "how well" is not a checklist. Checklists can be satisfied without being
understood, and in software, understanding is what creates durable results. A contributor
who has memorized the rules will follow them until the situation is slightly different.
A contributor who has internalized the judgment behind the rules will apply it correctly
to situations the rules did not anticipate — including the situations that matter most,
which are always the ones nobody planned for.

This distinction matters especially in this project's context. ZeroClaw is operated in
an environment of powerful tools: AI code generation, CI gates that catch a wide range
of common errors, IDE linters, automated security scanners. These tools are genuinely
valuable. They define a floor — a minimum below which code should not be merged. But
what they cannot do is think. They cannot decide whether an error is operational or a
programmer error. They cannot evaluate whether a test is asserting the right behavior.
They cannot tell whether a public API is documented clearly enough for a future
contributor to implement against correctly. They can only check what they were
programmed to check.

The gap between "what the tools can verify" and "quality that serves users, contributors,
and the project over time" is filled by judgment. That judgment is what this document
is trying to help you build — not to replace the tools, but to direct them.

--------

## 2. Honest Assessment: What the Codebase Is Telling Us

This section is not criticism. It is a diagnosis. The same framing that applied in the
architecture RFC applies here: you cannot improve what you cannot name, and the
specifics are useful precisely because they are specific.

### 2.1 The Evidence

The workspace decomposition from RFC §5574 succeeded. The crates exist, the trait
boundaries are real, and the compiler enforces the dependency direction. That is
genuinely good work. And within those new crates, the same patterns that characterized
the original monolith have been carried forward — because the codebase moved before
the team had a shared model for what "quality at the implementation level" looks like.

These are measured facts, not estimates:

| Metric | Value | What It Indicates |
|--------|-------|-------------------|
| `zeroclaw-config/src/schema.rs` | 16,800 lines | Now the largest file in the codebase; the original `loop_.rs` was called out at 9,500 lines in the architecture RFC — this surpasses it |
| `zeroclaw-channels/src/orchestrator/mod.rs` | 11,813 lines | Second-largest file; a single module carrying concentrated responsibility |
| `zeroclaw-runtime/src/onboard/wizard.rs` | 7,988 lines | A single workflow in a single file |
| `zeroclaw-runtime/src/agent/loop_.rs` | 6,101 lines | Reduced from ~9,500 in the monolith — real, measurable progress; still large |
| `zeroclaw-channels/src/orchestrator/telegram.rs` | 5,122 lines | One channel implementation; one file |
| `.unwrap()` / `.expect()` calls in crates | 5,630 | Each one is a deferred judgment call about error handling — see §4.1 |
| `.unwrap()` / `.expect()` calls in legacy `src/` | 240 | The migration carried the pattern forward at scale |
| Public functions in `zeroclaw-api` | 371 | The entire foundational API surface — every other crate depends on this |
| Doc comment lines in `zeroclaw-api` | ~27 | Roughly 14:1 ratio of undocumented public API — see §4.2 |
| `#[allow(unused_imports)]` / `#[allow(dead_code)]` in legacy `src/` modules | ~30+ instances | The compiler has identified code that is no longer being used; it has been asked not to say so |
| `TODO` / `FIXME` / `todo!()` / `unimplemented!()` across the full codebase | 20 | Notably low — suggests most debt is silent rather than marked |

The last row deserves its own note. Twenty explicit markers of incomplete work in a
codebase of this size is not a sign that the work is nearly finished. It is a sign that
most of the incomplete work is not being labeled as such. Unmarked debt is harder to
find, harder to prioritize, and harder to assign than debt that has been named. Silence
is not the same as completeness.

### 2.2 What the Numbers Do Not Show

These numbers measure what is countable. The more consequential quality questions cannot
be counted:

- Whether the 5,630 `.unwrap()` calls are in critical paths or in test utilities
- Whether the tests that exist are testing behavior or testing implementation details
- Whether the public functions in `zeroclaw-api` can be correctly implemented by someone
  reading only the signature and type
- Whether a log message emitted during a production failure would contain enough context
  to diagnose the failure
- Whether a contributor working in the security module understands which data has crossed
  a trust boundary and which has not

These are judgment questions. They do not have a CI gate. They have the standards this
document is proposing to name, and the culture of review and mentorship we are building
together.

### 2.3 What Is Already Good

The diagnosis should not obscure what is genuinely well-built.

The trait layer in `zeroclaw-api` is the right architecture. `Provider`, `Channel`,
`Tool`, `Memory`, `Observer`, `RuntimeAdapter`, and `Peripheral` are clean, well-reasoned
abstractions. They are the right seams. The problem is not the design — it is that the
design is not yet fully expressed in documentation, test coverage, and error handling
discipline. This RFC is about closing that gap.

The security model is thoughtful. Pairing codes, autonomy levels, sandboxing layers, and
policy enforcement show real design intent. That intent needs to be understood by every
contributor who writes code near a trust boundary — and this RFC exists partly to give
contributors the vocabulary to recognize where those boundaries are.

The observability infrastructure is mature. OpenTelemetry, Prometheus, and DORA metrics
are all implemented against a clean `Observer` trait. The infrastructure is in place.
The teaching gap is in how contributors use it so that it actually helps when something
goes wrong.

The test suite is not absent. The existing test investment is real. The work this RFC
describes is about the quality and distribution of that investment — what gets tested,
how, and whether the tests prove what they appear to prove.

`ADR-004-tool-shared-state-ownership.md` is an excellent piece of architectural record.
It proves the team can produce high-quality design documentation when the expectation
is clear. This RFC is proposing an equivalent expectation for the code itself.

--------

## 3. Gates and Standards: The Central Distinction

This is the organizing idea of the entire document. Understanding it clearly matters
more than any specific technique in §4.

A **gate** is binary. Pass or fail. It is automated, enforced by tooling, and defines
the minimum below which no code merges. The CI/CD RFC built the gates. They are real
and working.

| Gate | What It Checks |
|------|----------------|
| `cargo fmt --check` | Code is formatted consistently across the workspace |
| `cargo clippy --workspace --all-targets -D warnings` | No Clippy-known antipatterns; workspace-wide |
| `cargo deny check` | No unacknowledged security advisories; license and source compliance |
| `cargo nextest run --workspace` | Tests that exist, pass |

A **standard** is aspirational. It describes what quality looks like above the floor.
It is enforced by judgment, peer review, and the habits the team builds together.

| Standard | What It Describes |
|----------|-------------------|
| Error handling discipline | Failures are categorized; operational errors surface with context at the right layer |
| API documentation | Every public item has enough documentation to use correctly without reading the implementation |
| Test quality | Tests assert behavior, not implementation; test difficulty is treated as design feedback |
| Debt triage | Unaddressed debt is labeled, located, and risk-weighted; high-risk debt has an owner |
| Security posture | Trust boundaries are explicit at the implementation level, not only at the policy level |
| Observability discipline | Log messages answer the diagnostic question; spans bound meaningful units of work |

Gates and standards are not in competition. They are complementary layers. Gates without
standards produce code that passes every check and still fails users. Standards without
gates are unenforceable. You need both. The project currently has good gates and
underdeveloped standards.

> A codebase can pass every gate and still be incomprehensible to the next contributor,
> silent where it should surface errors, impossible to test in isolation, and insecure
> at the boundary where user input meets business logic. The green checkmark answers the
> question "did this code pass the rules we wrote down?" It does not answer the question
> "is this code good?" Those are not the same question.

This is not a criticism of the gates. The gates are valuable precisely because they
define a shared, enforceable baseline that every contributor works within. The goal of
this document is to build the shared vocabulary and judgment that defines what good looks
like above that baseline — and to explain clearly why that judgment cannot be delegated
to a tool.

--------

## 4. The Seven Disciplines

### 4.1 Error Handling as a Design Concern

Every `.unwrap()` call is a decision. Most of the 5,630 in the codebase were not made
consciously — they were made by default, because `.unwrap()` is the path of least
resistance when you need a value out of a `Result` or `Option` and want to move on. The
problem with decisions made by default is that they are not decisions — they are
deferrals. And what they defer is a real question: *what should happen here when this
fails?*

The answer depends on what kind of failure you are dealing with. There are three kinds,
and they have three different correct responses.

**Programmer errors** are violations of invariants that should be impossible in correct
code. A function that requires a non-empty `Vec`, called with an empty one. An enum
match that reaches an arm the type system should have made unreachable. These represent
bugs — not operational failures, but incorrect logic. `panic!` is the correct response,
because the goal is to find these at development time, not in front of a user at runtime.
`assert!` and `debug_assert!` are the right tools. `.expect()` with a message explaining
why this state is impossible is also appropriate here — it makes the reasoning explicit
and searchable, so the next person who reads the code understands why the panic was
intentional.

**Operational errors** are expected failure modes. Network timeouts. Files that do not
exist. API keys that have expired. Provider responses that carry an error status. Users
who provide malformed input. These are not bugs — they are the normal operating
conditions of a system that interacts with the world. The correct response is
`Result<T, E>`. The `?` operator propagates the failure to a caller who is in a better
position to decide what to do about it. A `.unwrap()` on an operational error is a
deferred panic: it will fire, eventually, under real conditions, in front of a real user,
with no useful context and no opportunity to recover.

**Configuration errors** are malformed or missing configuration discovered at startup.
The correct response is to fail fast — but specifically. Not a panic with a stack trace,
not a vague "invalid config" message. A message that points at the specific field,
explains what was expected, and tells the operator what to provide. A user who cannot
start ZeroClaw because of a misconfiguration should leave the process with a clear
understanding of exactly what to fix.

| Failure kind | What it means | Correct response |
|---|---|---|
| Programmer error | Invariant violated; should be impossible in correct code | `panic!`, `assert!`, `.expect("reason this is safe")` |
| Operational error | Expected failure mode; the world is not cooperating | `Result<T, E>`, `?`, structured error type with context |
| Configuration error | Invalid or missing startup configuration | Fail fast with a specific, actionable message |

Before every `.unwrap()` or `.expect()`, ask yourself: which kind of failure is this?
If the answer is "programmer error — this state cannot occur in correct code," then
`.expect()` with a comment explaining why is the right choice, and it communicates your
reasoning to every future reader. If the answer is anything else, use `?` or handle the
failure explicitly.

The `?` operator is worth understanding for what it *says*, not just what it does. It
says: I acknowledge this operation can fail. I am explicitly propagating that failure to
my caller, who is better positioned to decide what to do about it. That acknowledgment
is architecturally meaningful — it makes the error handling contract visible at the call
site and pushes decisions to the layer that has the most context.

The goal is not zero `.unwrap()` calls. Some are correct. The goal is that every one
represents a conscious decision, with the reasoning visible to anyone who reads the code.
The difference between `.unwrap()` and `.expect("this vec is guaranteed non-empty by the
caller — see §4.2 of the SOP engine invariants")` is not just style. It is the
difference between deferred judgment and documented judgment.

### 4.2 Public API Surface as a Promise

`pub` is a contract.

When you mark a function, struct, trait, or module as public, you are making a promise
to every caller. That includes the contributor who implements against it next month with
no memory of your original intent. It includes the AI assistant that reads your crate
to generate an implementation. It includes the person debugging a production incident
who needs to understand what this was supposed to do. It includes yourself, returning
to this code after two months working on something else.

A public item without documentation is a promise with no terms. The caller has no way
to know what assumptions you made when you wrote it, what error conditions it can return
and under what circumstances, what side effects it has, whether it is safe to call
concurrently, or what the subtle difference is between two functions with similar names.
They are left to infer — from the name, the type signature, and the implementation body
— something that you could have told them in three sentences.

The `zeroclaw-api` situation is specific enough to name directly. This is the one crate
the entire architecture depends on. Every provider, channel, tool, memory backend,
observer, runtime adapter, and peripheral implementation in the workspace is built
against these traits and types. An undocumented interface in this foundation propagates
confusion into every crate that implements it, every test that exercises it, and every
AI-generated code that works with it. The 14:1 ratio of undocumented public API surface
is not a documentation style preference — it is a gap in the contract that the
architecture RFC said was the most important layer of the system.

The AI dimension here is practical and direct: when you ask an AI assistant to implement
a trait or call a function that has no documentation, the AI infers intent from the name
and the type signature. Sometimes that inference is correct. More often, it produces
code that compiles, passes the type checker, and behaves incorrectly under specific
conditions that the AI did not know to anticipate — because nobody wrote them down.
Documentation is not just for humans. It is the specification you provide to every tool
that will ever work with your code, and to every person who will ever depend on it.

At minimum, every public item in `zeroclaw-api` should carry:

- **One sentence describing what it does.** Not what it is — what it does.
- **An `# Errors` section** (if it returns `Result`): under what conditions does this
  fail, and what error variants does the caller need to handle?
- **A `# Panics` section** (if it can panic): under what conditions, and why?
- **Preconditions** (if any are non-obvious): what must be true before calling this?

A three-sentence doc comment on a public trait method is worth more to the next
implementor than a hundred lines of implementation with no explanation. The
implementation tells them what the code does. The documentation tells them what it is
supposed to do — which is what matters when the two diverge.

### 4.3 Tests as Design Feedback

The goal of a test is not to produce a green checkmark. The goal is to create a precise,
executable record of what a piece of code is *supposed to do* — a record that fails
loudly if that behavior ever changes.

This distinction matters because there are two fundamentally different kinds of tests,
and only one of them achieves that goal.

A test that reaches into a struct's internal state, sets values directly, calls a method,
and asserts on return values is testing the *implementation*. If the implementation
changes — if the same behavior is achieved through a different mechanism — the test
breaks, even though nothing the user cares about changed. This creates friction against
refactoring without creating safety. It also tends to pass when the behavior is wrong in
ways the test did not anticipate.

A test that constructs values through public interfaces, exercises behavior through
public methods, and asserts on observable outcomes is testing the *behavior*. If the
implementation changes but the behavior is preserved, the test passes. If the behavior
changes in a way that matters to users, the test fails. This is what makes confident
refactoring possible: the tests are checking that you got the right answer, not that
you got it a particular way.

The more important principle is the diagnostic one:

> A test that is hard to write is usually telling you something about the design.

If writing a unit test for a function requires standing up a database connection, mocking
six dependencies, building a full configuration object, and starting an async runtime
explicitly — that function is probably doing too much, depending on too much, or sitting
at the wrong layer of the architecture. The difficulty is not a nuisance to work around.
It is feedback. The test is being honest about something the code is not yet honest
about.

This connects directly to the crate structure the architecture RFC established. One of
the purposes of crate decomposition was to create components that can be tested in
isolation. `zeroclaw-tool-call-parser` should be testable with a `&str` input and no
runtime. `zeroclaw-config` should be testable by constructing config structs directly.
Trait implementations in `zeroclaw-api` should be testable against fake implementations
of the trait — not against the full production stack. When you find yourself unable to
test a component without its entire environment, ask whether a dependency has entered
the implementation that the architecture did not intend. The test is giving you the
answer; the question is whether you are listening to it.

A practical approach to growing test quality over time:

- When you fix a bug, write a test that would have caught it. This one habit, practiced
  consistently, moves the test suite toward the failure modes that actually matter.
- When you add behavior, write a test that proves the behavior exists and can be
  verified in isolation.
- When a test is hard to write, spend time asking *why* before reaching for a mock. The
  answer to that question is usually more valuable than the test you were about to write.

### 4.4 Technical Debt Triage

The word "debt" is useful because it carries the right implication: it accrues interest.
Debt left unexamined in a high-traffic area of the codebase compounds — new code adapts
to its presence, new assumptions build on top of old ones, and the cost of addressing
it grows with every layer added above it.

The most common mistake teams make with technical debt is treating it as binary: either
everything is debt and nothing can be done about it, or nothing is debt and no time
should be spent on it. Both positions are wrong. The useful question is: *which debt,
in which location, carries the most risk right now?*

Two axes determine priority.

**Proximity to a trust boundary.** Code that handles user input, enforces security
policy, executes tools, manages authentication, or processes data from external sources
is operating near a trust boundary. Failures here can be exploited, silently corrupt
state, or produce incorrect behavior with security consequences. Debt near trust
boundaries carries disproportionate risk relative to its size.

**Blast radius.** Debt in `zeroclaw-api` — the foundation everything else depends on —
has a larger blast radius than debt in a single channel implementation. A wrong
assumption in a foundational type propagates wherever that type is used. Debt in a leaf
crate affects only that crate's consumers.

| | High blast radius | Low blast radius |
|---|---|---|
| **Near a trust boundary** | Address in the current cycle | Address in the next planned cycle |
| **Far from a trust boundary** | Address in a planned refactor | Address opportunistically, as adjacent work passes through |

This framework means that a `.unwrap()` in the security policy enforcement path is not
the same problem as a `.unwrap()` in a CLI display formatter. Both appear in the count
of 5,630. The count tells us the scope. The triage tells us the priority.

When you are working in a file and you notice debt — an `.unwrap()` that represents an
unhandled operational error, a function that has grown to handle four separate concerns,
a `#[allow(dead_code)]` silencing something that nobody calls — you do not need to fix
everything. You need to ask: is this in a high-risk location? If it is, address it in
this PR or file a follow-up issue with the specific location, the risk, and a proposed
owner. If it is not, you can mark it with a `// TODO(debt): <description>` comment that
makes it visible without making it urgent. What you should not do is leave it completely
unmarked — because silence is how 5,630 deferred decisions accumulate without anyone
noticing the trend.

The Strangler Fig pattern applies at this level too. The architecture RFC applied it at
the crate level: build the new structure around the old one, migrate inward over time.
The same pattern works inside a large file. You do not rewrite `schema.rs` in a single
PR. You identify the functions that are closest to trust boundaries, most frequently
changed, or hardest to test — and you extract them first, improving the structure
incrementally, leaving the rest to follow at a pace the team can sustain.

### 4.5 Security at the Application Layer

The CI/CD RFC established the security posture for the *supply chain*: `cargo deny`
finds known vulnerabilities in dependencies, enforces license compliance, and ensures
dependencies come from approved sources. That is the immune system for what enters the
project. This section is about the security posture of the code that runs.

`cargo deny` cannot find a vulnerability your application logic creates. It cannot tell
you whether user input is being validated before it reaches your business logic. It
cannot tell you whether a tool execution is respecting the autonomy level it is supposed
to enforce. It cannot tell you whether an error path is silently swallowing a security
check failure. These require a contributor who understands where the trust boundaries
are and what responsible code looks like on either side of them.

Three principles that should guide any code written near a trust boundary:

**Trust boundaries are explicit, not assumed.** A trust boundary is any point where data
arrives from outside your direct control: user input from any channel, API responses
from providers, file contents from the filesystem, plugin outputs, tool results, hardware
readings. At every trust boundary, validate before you process. Do not assume the shape,
size, type, or content of data you did not produce. The ZeroClaw security model defines
these boundaries at the policy level. The implementation should reflect them at the code
level — not because the policy will fail, but because defense in depth means each layer
of the system is doing its part, rather than trusting that every other layer did theirs.

**Minimum footprint.** A function that needs to read a file should not be able to write
one. A trait implementation that handles one channel's messages should not have access
to another channel's state. A tool running at autonomy level 1 should not be in a
position to exercise capabilities that require level 3. The security model already
defines these constraints. The discipline is in writing implementations that do not
acquire more capability than they require for the task at hand — and in noticing when an
implementation is reaching for something outside its intended scope.

**Fail loudly near security boundaries.** An error in a security check — a failed policy
evaluation, a signature verification failure, an unauthorized tool call attempt, a
pairing code mismatch — should never be silently swallowed. It should be logged,
propagated, and handled explicitly. An error in a display helper can be recovered from
gracefully with a log message. An error in an authorization path cannot. Know which kind
of function you are writing, and let that determination drive how aggressively you
surface failures from it.

These are not advanced security principles. They are foundational hygiene that applies
to any code that touches something a user can influence. The architecture RFC described
the security model as "thoughtful." The work this RFC is asking for is to make that
thoughtfulness legible at the implementation level — in the functions that validate
inputs, in the error paths that handle policy failures, in the boundaries between what
the system was asked to do and what it actually does.

### 4.6 Observability as Debuggability

The observability infrastructure is mature: OpenTelemetry tracing, Prometheus metrics,
DORA tracking, and a clean `Observer` trait are all in place. This is production-quality
work. The teaching gap is between having the infrastructure and using it in a way that
actually helps when something goes wrong — ideally before you know what went wrong.

Consider two log messages. Both compile. Both pass CI. Both are syntactically correct.

```rust
error!("request failed");
```

```rust
error!(
    provider = %provider_name,
    model    = %model_id,
    user     = %sender_id,
    tool     = %tool_name,
    attempt  = attempt,
    elapsed  = ?elapsed,
    err      = %e,
    "provider request failed — retries exhausted"
);
```

The first is a record. It confirms that something went wrong. The second is a
*diagnostic*. It answers the questions that matter: what were we trying to do, in what
context, with what parameters, and exactly what went wrong. The difference between them
is not technical sophistication — it is whether the person writing the message was
thinking about the person who will one day need to read it.

The question to ask before writing any log message at `warn` or above:

> What does the person who needs to diagnose this failure at the worst moment need to know?

That person might be you, six months from now, with no memory of writing this code. It
might be another contributor who has never seen this module. It might be a user filing a
bug report with a log excerpt they copied from their terminal. Write for them. The fields
that almost always matter: what were we trying to do, what context was in scope at the
time, and what specifically went wrong.

The same principle governs tracing span design. A span should represent a meaningful
unit of work, carry the context needed to understand that work, and have a name that
makes sense when you read it in a flame graph or a trace viewer.

```rust
// A record
let _span = span!(Level::INFO, "process");

// A diagnostic
let _span = span!(
    Level::INFO,
    "agent.tool_call",
    tool = %tool_name,
    turn = turn_number,
    sender = %sender_id,
);
```

Structured logging and meaningful span design are not style preferences. They are what
make the observability infrastructure you have actually useful — not just during
development, but in the hands of users running ZeroClaw on hardware you will never see,
in configurations you did not anticipate, encountering errors you did not plan for. The
infrastructure creates the capability. The discipline in how contributors use it
determines whether that capability translates into diagnosable systems.

### 4.7 Working Above the Floor

The previous six disciplines each address a specific domain. This section synthesizes
them into a single picture of what "above the floor" looks like in practice — what a
reviewer, a future contributor, or a user actually experiences when they encounter code
that meets the standards described in this RFC.

| Dimension | At the floor — gates pass | Above the floor — standard met |
|---|---|---|
| Error handling | Code compiles; no Clippy warnings | Failures are categorized; operational errors surface with context; panics are intentional and documented |
| Documentation | Doc tests pass if they exist | Every public item can be understood and used correctly without reading the implementation |
| Testing | Tests that exist pass | Tests assert behavior; test difficulty is treated as design feedback; the failure modes that matter are covered |
| Debt | No compiler errors or warnings (with `#[allow]` silencing the rest) | Debt is labeled, located, and risk-weighted; high-risk debt has an owner and a timeline |
| Security | `cargo deny` passes | Trust boundaries are explicit; security failures surface loudly; implementations respect their intended scope |
| Observability | Code runs and emits something | Log messages answer the diagnostic question; spans bound meaningful units of work with useful context |
| Code organization | File compiles; module structure exists | Functions do one thing; files group related concerns; large files are candidates for extraction, not the norm |

None of these are achievable entirely through automation. All of them are achievable by
contributors who understand why they matter and have built the judgment to apply them
consistently. That is what this document is working toward.

--------

## 5. What This Means for AI-Assisted Development

The culture RFC addressed how to work with AI tools as part of a collaborative team.
This section addresses something more specific: what happens when AI-generated code
encounters the standards described above — and what it takes to recognize and close the
gap when it does not.

AI tools are genuinely good at passing gates. They generate code that compiles,
satisfies the type checker, passes Clippy, and often produces tests alongside the
implementation. This is real value, and it is not the point of this section to minimize
it. The problem is not that AI tools are unreliable. The problem is that they are
reliable at the wrong thing: producing code that passes checks, rather than code that
meets standards.

The reason is structural. AI generates code against what it can infer. If a function
has no documentation, the AI infers intent from the name and signature — and sometimes
that inference is correct, and sometimes it produces subtly wrong behavior that only
surfaces under conditions nobody tested. If an error type has no documentation of when
it is returned, the AI handles it based on the name of the variant. If a test suite
tests implementation rather than behavior, the AI generates implementations that match
those tests — which may or may not match the intended behavior that the tests were
supposed to capture. The quality ceiling of AI output is set by the quality of the
context you provide. Better context — clearer documentation, more specific error types,
behavior-focused tests — produces better output. Underdeveloped context produces output
that passes the gates and defers the judgment to whoever reviews it next.

This creates a specific and non-optional responsibility for contributors working with
AI tools.

**Review is not optional because AI wrote it.** The culture RFC named this clearly, and
it bears repeating with specifics: when reviewing AI-generated code, the gate questions
— does it compile, do the tests pass — are the beginning of the review, not the end. The
standard questions are: does this handle operational errors correctly, or does it
`.unwrap()` them? Is the new public API documented? Does the test assert the behavior or
the implementation? Is this near a trust boundary, and if so, does it validate its
inputs? These questions are your responsibility regardless of who wrote the code or what
tools were used to produce it.

**AI amplifies your judgment, not your absence of it.** A contributor who does not yet
have a mental model for what good error handling looks like will accept AI-generated
error handling at face value — `.unwrap()` and all. A contributor who has internalized
§4.1 can look at the same output and direct the tool: "this is an operational error
path; use `?` and propagate the failure to the caller with context." The tool will
produce a corrected version. The same pattern applies to every discipline in §4. The
tool is powerful in the hands of someone who knows what to ask for. Without that
direction, it produces code that satisfies the compiler and defers the real decisions
to the next person in the chain.

**This relationship compounds in both directions.** A team that understands the standards
gets progressively more value from AI tooling as the tools improve, because they can
direct more capable tools more precisely. The gap between "what the tool produced" and
"what the standard requires" becomes something they can close with direction rather than
manual rewriting. A team that does not build that judgment gets a faster path to the
same quality floor, without the ability to push past it. The investment described
throughout this document is also, directly, an investment in the long-term effectiveness
of every AI tool the team will ever use — because the value of those tools scales with
the clarity of the judgment directing them.

--------

## 6. The Portability of Craft

Technology changes. It changes faster with each iteration than it did the time before,
and that rate is accelerating. The specific tools in this document — Rust, `cargo`,
`clippy`, the OpenTelemetry SDK, the AI assistants the team uses today — will be
superseded. Some of them within the lifetime of this project. The platforms will change.
The languages will evolve. The tooling ecosystem will look different in five years than
it does today, and different again in ten.

The mental models in this document will not change.

The question "what should happen here when this fails, and who needs to know?" does not
expire when the language changes. You will ask it in the next language you learn. You
will ask it when designing a distributed system where the "language" is a wire protocol.
You will ask it when building anything that other people depend on and that you cannot
personally supervise. The specific Rust mechanism for answering it — `Result<T, E>`, the
`?` operator, structured error types with context — is one answer to a question that
exists everywhere.

The question "what is the public interface I am promising, and does my documentation
reflect that promise?" — you will ask this when designing an API, when writing a
technical specification, when defining the scope of a team's responsibilities, when
communicating requirements to another team, to an AI tool, to a client, to a contractor.
The promise-and-terms model of public interfaces extends far beyond Rust and far beyond
software.

The question "what does my test actually prove?" extends beyond software into any domain
where you need to verify that a system behaves as intended. The instinct to ask it — to
distinguish between evidence that your implementation exists and evidence that the right
thing happens — is the skill. The syntax for expressing it in Rust is incidental.

The question "what would the person who needs to diagnose this failure need to know?" is
an engineering question that applies to anything you build that other people depend on.
It is also, at a deeper level, a question about empathy — about remembering that the
person on the other side of your work is a real person with a real problem, at a moment
you cannot predict, with context you will not be there to provide.

> You are not learning Rust. You are, through the vehicle of Rust, learning to build
> things that can be trusted. That is portable. It will compound for as long as you
> practice it — across every language, every system, every team, and every domain you
> ever work in.

This is the investment the project is making in you. Not in your specific technical
skills, but in your ability to bring judgment, craft, and care to whatever you build
next. And it is, in turn, the investment you make in every person who will one day
depend on something you built.

--------

## 7. What This Means for Contributors

**If you are new to Rust or new to software development:**

The seven disciplines in §4 are not requirements to master before you can contribute.
They are a map of the territory — things you will encounter as you work, named clearly
enough that you know what you are looking at when you see them.

Start with §4.1. The error handling mental model is the single highest-leverage thing
you can internalize early, and it is not Rust-specific. When you read existing code and
encounter `.unwrap()`, ask yourself which of the three categories it falls into. When
you write new code, ask the same question about your own choices. That single habit,
practiced consistently, improves every file it touches and develops a judgment that will
follow you everywhere.

Do not wait until you feel ready to apply these standards. Apply them imperfectly, ask
questions when you are unsure which category something falls into, and treat the
feedback you receive in review as the teaching it is intended to be. Nobody arrived
knowing these things. They were learned, slowly, through exactly the kind of work you
are doing here.

**If you are using AI tools to help you contribute:**

The standards in this document are what a careful review will evaluate AI-generated code
against. They are also, practically, the context that makes AI output more correct before
it reaches review. Before asking an AI to implement something, check whether the
interfaces it will implement against are documented. If they are not, document them
first — or include the documentation as part of what you ask the AI to produce. The
output will be more correct, you will have closed a real gap in the foundation, and the
next contributor who comes along will benefit from both.

When you receive review feedback on AI-generated code, treat it as feedback on the code,
not as feedback on your choice to use AI. The standards apply equally regardless of
authorship. The question is always: does this code meet the standard? If it does not,
what needs to change, and why?

**If you are reviewing pull requests:**

The gate questions — does it compile, do the tests pass, does Clippy accept it — are
the floor, not the ceiling. A review that only answers those questions is an incomplete
review. Use the framework in §3 and the disciplines in §4 to structure your observations.
Name the standard you are applying, explain why it matters, and clearly separate
blocking concerns from non-blocking suggestions.

The goal of a review is not to find fault. It is to transfer understanding. Every
specific piece of feedback that includes an explanation — "this is an operational error
path; here is why `.unwrap()` creates a production risk here and what to use instead" —
is an investment in the contributor you are reviewing. That investment compounds. The
contributor who understands the principle will apply it correctly to the next ten
situations where it matters, without needing to be told again.

**If you are a maintainer or more experienced contributor:**

You are in the best position to make these standards real — not by enforcing them from
above, but by modeling them in your own code and naming them by name in review. The most
effective teaching in an open source project happens in PR threads and code comments,
not in documents. This document provides the vocabulary. Using it consistently in
everyday review is what moves it from words on a page to shared practice.

When you see a `.unwrap()` on an operational error path, name it as such. When you see
a public function without documentation, ask the question: what does a future implementor
need to know here? When you see a test that would break on a valid refactor, explain why
that matters. These are not corrections — they are the ongoing mentorship that the
culture RFC identified as one of the most important things a more experienced contributor
can offer.

--------
