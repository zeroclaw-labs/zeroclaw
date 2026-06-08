# FND-005: Contribution Culture — Human Collaboration, AI Partnership, and Team Growth
### Starting v0.7.0 · Type: Culture · Rev. 1

> **Canonical reference** · Ratified by the team · Rev. 1
> Discussion thread and full revision history: [#5615](https://github.com/zeroclaw-labs/zeroclaw/issues/5615)


---

> **A note to the team before you read this.**
>
> This is the fifth document in ZeroClaw's maturity framework. The other four address
> architecture, documentation, governance, and engineering infrastructure — the structural
> layers that make a project work. This one addresses something those four take for granted
> but never explicitly teach: how to work together.
>
> The tools and processes in the other RFCs only function as well as the team using them.
> A perfect CI pipeline does not help a team that cannot give honest feedback. A clean
> architecture does not survive a team that cannot disagree productively. A governance
> model does not build ownership in people who have never been taught what ownership means.
>
> This document is about building that team — not just technically capable individuals,
> but people who know how to give and receive feedback, how to ask for help, how to use
> powerful tools responsibly, and how to grow together over time. These are learnable
> skills. Nobody arrives with them fully formed. This document names them clearly enough
> that you can start practicing them deliberately, here, in the context of real work that
> matters.
>
> Nothing in this document is criticism of who you are or where you started. It is a map
> for where we are trying to go together.

---

## The Maturity Framework Suite

This RFC is the fifth in a set of five documents that together form ZeroClaw's maturity
framework. They are designed to be read as a whole, though each stands on its own.

| RFC | Scope | Issue |
|-----|-------|-------|
| Intentional Architecture — Microkernel Transition | What we are building and how it is structured | #5574 |
| Documentation Standards and Knowledge Architecture | How we document what we build | #5576 |
| Team Organization and Project Governance | How we coordinate and make decisions | #5577 |
| Engineering Infrastructure — CI/CD Pipeline | How we build, test, and ship reliably | #5579 |
| **Contribution Culture — Human Collaboration and AI Partnership** | **How we work together and grow** | **FND-005** |

The first four RFCs answer structural questions. This one answers a human question: given
the structure, how do the people inside it behave toward each other and toward their tools?
That question does not have a compiler, a linter, or a CI gate. It has only the habits we
build, the examples we set, and the intentionality we bring to it.

---

## Table of Contents

1. [Why this document exists](#1-why-this-document-exists)
2. [The work before the work](#2-the-work-before-the-work)
3. [Working with people](#3-working-with-people)
   - [Giving feedback](#giving-feedback)
   - [Receiving feedback](#receiving-feedback)
   - [Asking for help](#asking-for-help)
   - [Disagreeing productively](#disagreeing-productively)
   - [Ownership](#ownership)
   - [Supporting someone who is struggling](#supporting-someone-who-is-struggling)
4. [Working with AI](#4-working-with-ai)
   - [The delegation mental model](#the-delegation-mental-model)
   - [AI works at the implementation layer](#ai-works-at-the-implementation-layer)
   - [Amplification is not magic](#amplification-is-not-magic)
   - [The review discipline](#the-review-discipline)
   - [What this means for your career](#what-this-means-for-your-career)
5. [The feedback taxonomy](#5-the-feedback-taxonomy)
6. [A note to reviewers and mentors](#6-a-note-to-reviewers-and-mentors)

---

## 1. Why this document exists

Most contributing guides tell you how to open a PR. They tell you what labels to use,
how to run the test suite, and what goes in the commit message. Those things matter, and
we have documents that cover them.

This document covers something different: the skills that determine whether a group of
talented people becomes a functional team or a collection of individuals who happen to
share a repository.

These are learnable skills. They are not personality traits you either have or do not
have. They are not things that come automatically with technical ability. They are
practiced — slowly, with feedback, over time — the same way any other skill is learned.
Most software engineering education focuses almost entirely on the technical layer and
leaves the human layer to chance. The result is that a lot of technically capable people
end up in teams that do not work well together, without any clear understanding of what
is missing or how to fix it.

The goal of this document is to name those skills clearly enough that you can start
practicing them deliberately, here, in the context of real work that matters.

---

## 2. The work before the work

Before you write a line of code, open a PR, or ask an AI to generate anything, there is
a set of questions you should be able to answer. This project uses a decision hierarchy
to describe them:

```
Vision
  └── Architecture
        └── Design
              └── Implementation
                    └── Testing
                          └── Documentation
                                └── Release
```

The hierarchy is described in full in the architecture RFC (#5574). What matters here is
the principle behind it: **every decision you make should be traceable back up to the top.**

In practice, this means asking yourself before you start building:

- **What problem am I solving?** Not "what ticket am I closing" — what actual problem
  does this solve for someone?
- **Does this fit the architecture?** If you cannot describe where this belongs in the
  system structure, you do not yet understand the system well enough to change it.
- **What does done look like?** Before you write the code, write the acceptance criteria.
  "It works" is not an acceptance criterion. "A user can install a plugin without a Rust
  toolchain and it runs correctly" is.
- **Who needs to know about this?** Changes that touch other people's work, or that make
  decisions the whole team should make, need visibility before implementation — not after.

This is not bureaucracy. It is the difference between building something and building the
right thing. It also applies directly to how you work with AI tools, which we cover in
Section 4.

The honest version of what happens when you skip this step: you build something that
works, open a PR, and then learn in the review that it solves the wrong problem, or
solves the right problem in a way that conflicts with a decision that was already made
somewhere else. That wastes your time, the reviewer's time, and delays the people who
depend on the work. The pre-work is not extra — it is how you protect your own effort.

---

## 3. Working with people

### Giving feedback

Feedback is one of the highest-leverage things you can do for another engineer. A
well-written review comment can teach something that takes years to learn on your own.
A poorly written one can discourage someone from contributing again.

**Be specific.** Vague feedback creates anxiety without direction.

> ❌ "This is hard to read."
>
> ✅ "This function is handling three separate concerns — input validation, business logic,
> and formatting the response. Consider splitting them so each function does one thing.
> That makes it easier to test each piece and easier to understand at a glance what each
> one does."

The second version is longer, but it teaches something. The reader now knows what the
problem is, why it matters, and what to do about it.

**Explain the principle, not just the verdict.** If you ask someone to change something,
tell them why. "Change X to Y" produces a fix. "Change X to Y because Z" produces
understanding that applies to the next ten situations where the same principle applies.

**Separate the work from the person.** "This approach has a problem" and "you made a
mistake" are not the same statement. The first is about the code. The second is about
the person. Keep your feedback pointed at the work.

**Name what is good.** This is not about being nice — it is about being useful. When you
tell someone what they got right and explain why it is right, you teach them what
patterns to repeat. Generic praise ("great work!") teaches nothing. Specific praise
("extracting this into its own crate was the right call because it means we can now test
this logic in isolation without standing up the whole agent loop") teaches the principle
and reinforces the decision.

**Use the feedback taxonomy.** The taxonomy in Section 5 gives every comment a clear
weight. Reviewers who mix blocking issues with minor suggestions without distinguishing
between them force the author to guess which things actually need to change. Do not make
people guess.

---

### Receiving feedback

This is harder than giving feedback for most people, and it is worth being honest about
why.

When you have spent hours on something — working through a problem, making decisions,
writing the code — and someone tells you it has issues, the natural human response is
to feel like the criticism is about you. It is not. It is about the work. Learning to
hold those two things as separate is a skill, and it takes practice.

A few things that help:

**Read the feedback before you respond to it.** Not just the summary line — the whole
comment, including the explanation. Many feedback responses are written in reaction to
the verdict before the person has absorbed the reasoning. Read the why before you decide
how you feel about the what.

**Distinguish between "I disagree" and "I do not understand."** These require different
responses. If you do not understand the feedback, ask a clarifying question. If you
understand it and disagree, say so with evidence. Both are good outcomes. What is not
useful is staying silent when you have questions, or saying "ok fine" when you actually
disagree.

**You do not have to agree with every piece of feedback to learn from it.** Sometimes
feedback is wrong. Sometimes it reflects a different set of tradeoffs than the ones you
were optimising for. You are allowed to push back — see Disagreeing productively below.
But even feedback you ultimately reject is worth understanding fully before you decide
to reject it.

**Close the loop.** When someone takes time to review your work, tell them when you have
addressed their feedback. You do not have to thank them effusively — a simple "addressed
in the latest commit" is enough. It tells them their time was worthwhile and keeps the
PR moving.

**Feedback on your code is not feedback on your worth.** This sounds obvious. It is not
obvious when you are in the middle of it. Every experienced engineer has code reviewed by
people who are more experienced, and that process is uncomfortable every time. The
discomfort is the sensation of learning. It does not go away; you just get better at
sitting with it.

---

### Asking for help

In school, asking for help can feel like admitting you are behind, or that you do not
belong. In a team, asking for help is one of the most professional things you can do.

**The cost of being stuck and not asking is almost always higher than the cost of
asking.** Three hours of spinning on a problem that a five-minute conversation would
resolve is three hours of your time and your team's time that is gone. Knowing when to
ask is a skill, not a weakness.

A good help request has three parts:

1. **What you are trying to do.** Not just "it's broken" — what is the goal?
2. **What you have already tried.** This shows you have engaged with the problem and
   gives the person helping you a starting point that is not zero.
3. **Where you are stuck specifically.** "I don't know what's wrong" is a different
   problem than "I know what's wrong but I don't know how to fix it" and "I fixed it
   but I don't know why my fix works."

A help request that has these three components gets answered faster and teaches you more,
because the person helping you can calibrate to exactly where you are.

**Ask publicly when you can.** A question asked in a shared channel or on a PR benefits
everyone who has the same question later. A question asked privately benefits only you.
There are times when private is right — sensitive feedback, personal circumstances — but
technical questions about the codebase are almost always better asked in the open.

**Not knowing something is not shameful.** Nobody knows everything. The engineers who
appear to know everything have asked a lot of questions over a long time, and the answers
accumulated. The only way to get there is to start asking.

---

### Disagreeing productively

Architecture disagreements are healthy. They mean people care about how the system is
built and are paying attention to the decisions being made. A team where nobody disagrees
is not a team where everyone agrees — it is a team where people have stopped engaging.

The difference between a productive disagreement and an unproductive one is usually in
the framing.

**Lead with the concern, not the verdict.**

> ❌ "This approach is wrong."
>
> ✅ "I have a concern about this approach — specifically, if we wire the gateway directly
> into the runtime here, we break the dependency rule in RFC §4.2. Can we talk through
> whether there is a way to achieve the same result without that coupling?"

The second version opens a conversation. The first closes one.

**Bring evidence.** An architecture disagreement backed by a measured fact, a specific
RFC section, or a concrete failure scenario is a contribution. An architecture
disagreement backed by "I just feel like" is an opinion. Both are worth expressing, but
only one moves the conversation forward quickly.

**Be genuinely open to being wrong.** If you go into a disagreement having already
decided you are right, you are not having a conversation — you are lobbying. People can
tell the difference, and it makes them less likely to engage seriously with your concerns.
The goal is the best outcome for the project, not being right.

**When the team decides, move with the team.** You can note your dissent on the record —
in the issue, in the RFC comments, in the PR thread — and then you build what was
decided. This is not capitulation. It is how teams function. A team that keeps
relitigating settled decisions does not ship.

**Some decisions are reversible and some are not.** Know which kind you are arguing about.
A naming decision is reversible. A wire protocol decision that will be in production
binaries for two years is not. Weight your energy accordingly.

---

### Ownership

Ownership is one of those words that gets used a lot without a clear definition. Here
is what it means in practice on this project:

**Ownership means you see the problem before you are asked to.** It means reading a PR
that touches your area and noticing a side effect the author did not notice. It means
seeing a follow-up issue sitting without an assignee and picking it up. It means not
waiting to be told.

**Ownership means your word means something.** If you file a follow-up issue with your
name on it, that issue is your commitment. Not "someone should do this" — you will do
this. If circumstances change and you cannot, you say so early and you find a handoff.
A tracker full of filed-and-forgotten issues with names attached is a broken trust
register.

**Ownership is not "I did my part."** It is "I care whether the whole thing works." You
can own a crate without being indifferent to whether the system that crate lives in is
healthy. You can own a feature without being indifferent to whether users can actually
use it. Narrow ownership — "I did my bit, the rest is someone else's problem" — produces
systems that technically have owners for every piece and functionally have no one
responsible for anything.

**Ownership includes the follow-through.** Shipping code is not the end of ownership.
It is the beginning of the responsibility to make sure it works, to fix what breaks,
and to teach the next person who works in that area what you learned.

---

### Supporting someone who is struggling

At some point in this project you will be more experienced than someone else in a thread.
Maybe you have been here longer. Maybe you happen to know the part of the codebase they
are working in. Maybe you have seen this particular failure mode before.

What you do with that position matters.

**Do not just fix it for them.** Giving someone a working solution without explaining
what was wrong or why your solution works produces a merged PR and zero learning. The
next time they hit a similar problem, they will be in the same place. Take the extra
five minutes to explain what you saw and why the fix works.

**Review with intent to teach.** A bad PR is not just a problem to close — it is a
teaching opportunity. A dismissive review ("this doesn't follow the architecture") is
less useful than a review that names what was missed, explains the principle it violates,
and points to where the contributor can learn more. The extra effort is an investment in
a contributor who writes better PRs from that point forward.

**If someone is blocked and not asking for help, say something.** Sometimes people do
not ask because they do not want to look like they are struggling. Sometimes they are not
sure who to ask. Sometimes they have been struggling long enough that they have stopped
noticing how stuck they are. A quiet "looks like this one has been open for a while —
is there anything I can help unblock?" costs almost nothing and can mean everything to
someone who is spinning.

**Make it safe to not know things.** If people in your team feel judged for not knowing
something, they will pretend to know things. That produces worse decisions, not better
ones. The team that makes it safe to say "I don't know, let me find out" makes better
decisions than the team where everyone performs confidence.

---

## 4. Working with AI

This section is about something that most contributing guides do not cover: how to work
with AI coding tools in a way that makes you better, not just faster.

### The delegation mental model

Here is the most useful reframe for working with AI effectively:

**Working with an AI is the same skill as delegating to a person.**

When you delegate work to a colleague or a junior engineer, you provide context. You
explain the goal, the constraints, what good looks like, and what the boundaries are.
You do not just say "build me a feature." You say: here is what the user is trying to
do, here is how it fits into the system, here is how we will know it is done, and here
are the things you should not do.

Then — critically — you review what comes back. You do not accept a junior engineer's
PR without reading it. You check whether it does what was asked, whether it fits the
architecture, whether it has test coverage, whether the error handling is correct. You
give feedback. You may iterate.

AI tools work exactly the same way. The quality of what you get back is determined
almost entirely by the quality of what you put in. A vague prompt produces vague output.
A prompt with clear context, specific constraints, and concrete acceptance criteria
produces output that is actually useful as a starting point.

The engineers who struggle with AI tools are usually the ones who are still learning
to give clear direction to anything — human or AI. The engineers who thrive with them
are the ones who already know what they want before they ask for it.

This mental model also means that the output is your responsibility. You cannot submit
a PR and say "the AI wrote it." You reviewed it. You opened the PR. It is your work.

---

### AI works at the implementation layer

This is the most important technical limitation to understand.

AI code generation works at the **implementation layer** of the decision hierarchy:

```
Vision          ← AI cannot set this. You must.
Architecture    ← AI cannot make these decisions. You must.
Design          ← AI will sometimes guess. You must verify.
Implementation  ← AI can help here.
Testing         ← AI can help, but you define what to test.
Documentation   ← AI can draft. You must review for accuracy.
Release         ← Human judgment required.
```

An AI tool will generate a function that does what you described. It will not tell you
whether that function belongs in this crate or a different one. It will not flag that
the approach contradicts an architectural decision made three months ago. It will not
ask whether you have thought through the security implications. It will not notice that
you are solving the wrong problem.

ZeroClaw itself is a useful example. The initial codebase was bootstrapped with AI
assistance. The result, as the architecture RFC describes it, is "impressively functional
but architecturally accidental." The code does what it needs to do today — but it was
not designed, it accumulated. That is not a failure of AI tools. It is a predictable
outcome of using implementation-layer tooling without first doing the vision,
architecture, and design work that gives implementation its direction.

The solution is not to use AI less. It is to do the top-of-hierarchy work yourself,
always, before you ask the AI to build anything.

---

### Amplification is not magic

AI tools amplify your existing capabilities. That is the honest description of what
they do.

If you have a clear vision, a defined architecture, quality criteria you can articulate,
and the ability to evaluate output critically — AI is a genuine force multiplier. You
move faster. You explore more options. You write more tests. You draft more documentation.

If you do not have those things — AI generates a lot of code that looks convincing and
does not hold together. It generates tests that pass without testing anything meaningful.
It generates documentation that describes the code but not the intent. It generates
architecture that is locally consistent and globally incoherent.

The amplification is neutral. It amplifies good inputs and bad inputs with equal
enthusiasm.

This means the most valuable skill in an AI-assisted workflow is not prompt engineering.
It is the ability to evaluate the output. That requires knowing what good looks like
before you ask for anything. Which brings you back, every time, to the top of the
decision hierarchy.

A useful self-check before using an AI tool to implement something:

- Can I describe the problem in one sentence without mentioning implementation details?
- Can I name the RFC section or design decision that this implementation serves?
- Can I describe what a correct implementation looks like before I see one?
- Can I explain why a generated implementation is or is not correct after I see one?

If the answer to any of these is no, you are not ready to implement yet. You are still
in the design phase.

---

### The review discipline

AI-generated code requires the same review discipline as human-written code. In some
ways it requires more, because the surface area of issues you are checking for is wider.

When you review AI-generated output — your own or someone else's — check for:

**Architectural fit.** Does this respect the dependency rules? Does it live in the right
crate? Does it introduce a coupling that the design explicitly avoids?

**Correctness at the boundary.** AI models are very good at the common case and
frequently wrong at the edge case. Check what happens when inputs are empty, null,
malformed, or at the maximum expected size. Check what happens when a dependency is
unavailable.

**Security implications.** AI tools do not have a security mindset by default. They
will generate code that accepts user input without validation, that logs sensitive
values, that uses deprecated cryptographic primitives, that opens file paths without
checking them. You have to bring the security lens explicitly.

**Test quality.** AI-generated tests frequently test the implementation rather than the
behaviour. A test that asserts a function returns a specific internal struct value is
not a behaviour test — it is a snapshot of the implementation that will break whenever
the implementation changes. Ask: does this test verify that the system does what the
user or caller needs, or does it verify that the code does what it currently does?

**Completeness.** AI tools optimise for plausible-looking completeness. They will
generate code that handles the happy path thoroughly and the error path superficially.
Check that errors are propagated, handled, or surfaced in a way that is actually useful
to the caller.

---

### What this means for your career

The skills being described here — giving direction clearly, evaluating output critically,
understanding where a component fits in a larger system, knowing what good looks like
before you build — are not AI-specific skills. They are the skills that make someone
an effective engineer, an effective tech lead, and eventually an effective engineering
manager.

The engineers who will be most valuable in a world saturated with AI-generated code are
not the ones who can write the most code fastest. They are the ones who can tell whether
the code is right. That requires system thinking, architectural judgment, and the
ability to evaluate work against a standard you have internalised.

Everything you practice here — understanding the RFC before you implement, asking "why"
before you build, reviewing AI output with the same eye you would bring to a junior
engineer's PR — is practice for that kind of judgment. It compounds. Every PR where you
engage seriously with the architecture is a data point that makes the next architectural
decision easier.

The contributors on this project have an unusual advantage: you are building these habits
on a real system, with real architectural constraints, with people who will review your
work and explain why. That combination is rare. It is worth taking seriously.

---

## 5. The feedback taxonomy

Every review comment on this project carries an explicit weight. Using those weights
consistently means reviewers communicate clearly and authors know exactly what requires
action.

The categories below describe the project's review intent. PR reviews render
that intent through the review protocol's emoji headings: 🔴 blocking,
🟡 warning, 🔵 suggestion, 🟢 praise, and ✅ resolved. Use
`docs/book/src/contributing/pr-review-protocol.md` for the exact PR-review
format.

---

### ✅ Commendation

Something the author got right, named specifically and explained so the pattern gets
repeated.

This is not politeness. Generic praise ("nice work!") teaches nothing. Specific praise
with an explanation teaches the principle behind what was done well, which applies to
every future decision in the same category.

**Commendations require no action.** Their purpose is to reinforce.

> *Example: "Extracting the tool call parser into its own crate was the right call — this
> code has zero dependencies on agent state and is now independently testable. The 91
> tests you added are exactly the kind of coverage that would be impossible to achieve
> when this logic lived inside `loop_.rs`."*

---

### 🔴 Blocking

Something that must be resolved before the PR merges. Blocking items fall into two
categories:

- **Architectural violations**: code that crosses a dependency boundary the design
  explicitly prohibits, or that contradicts a decision recorded in an RFC or ADR.
- **Quality regressions**: missing test coverage for new behaviour, security issues,
  broken contract compatibility, or code that introduces a defect.

A blocking comment explains what the issue is, why it matters, and — where possible —
what a resolution path looks like. A blocking comment is not a judgment of the author.
It is the reviewer's responsibility to the codebase and the users who depend on it.

**Authors should not interpret a blocking comment as rejection.** It is a specific,
resolvable problem. Address it and move forward.

---

### 🟡 Conditional

Something acceptable to defer, but only with a committed tracked issue and an assignee.
A conditional item is the reviewer saying: *I trust that this will be addressed, but I
need that commitment on record before we merge.*

The distinction between blocking and conditional is often about timing and risk. A
missing feature that will be delivered in the next PR is conditional. A missing feature
that creates a security gap is blocking.

**A conditional deferral without an assignee is not a deferral — it is a wish.** Tracked
issues with no owner tend to stay open indefinitely. When a reviewer marks something
conditional, they are asking for a named commitment, not a theoretical future intention.

---

### 🔵 Team Decision

A question the PR surfaces that no single reviewer or author should answer unilaterally.
Team decisions involve tradeoffs that affect the project's direction, its architecture,
or its users — and they belong to the group.

Using this label is how reviewers avoid holding up individual contributors with questions
that are really about shared direction. It surfaces the decision, frames the tradeoffs,
and asks the team to weigh in — without making the author feel like their PR is blocked
on something that is not in their control.

**Team decisions should be answered in the PR thread, on the record, by the people who
need to own the outcome.** A decision answered in a side conversation that does not
appear in the PR thread does not exist for anyone who reads the history later.

---

## 6. A note to reviewers and mentors

If you are in a position of reviewing someone else's work — whether as a code owner, a
more experienced contributor, or simply someone who has been here longer — this section
is for you.

**You are modelling what collaboration looks like.** Every review you write teaches the
author how to review. Every question you ask in a PR thread teaches newer contributors
what questions are worth asking. You cannot opt out of this — the only choice is whether
to do it intentionally or accidentally.

**Thoroughness is respect.** A thorough review that explains its reasoning is more
respectful of the author's effort than a quick approval. The author put time into the
work. They deserve to understand why it is or is not ready to merge, and what they can
take forward from the interaction.

**The goal of every review interaction is to leave the author better equipped than they
were before.** Not just to produce a merged PR. Not to demonstrate your own knowledge.
Not to enforce rules. To leave the author with something they can use — a principle,
a pattern, an understanding of a tradeoff — that applies beyond the immediate PR.

**Name the pattern, not just the instance.** When you ask for a change, explain the
principle behind it. "Rename this variable to something that describes what it contains"
is less useful than "variable names should describe their purpose from the caller's
perspective, not the implementation's — what does the caller of this function actually
care that this value represents?" The second version applies to every variable in every
function the author will ever write.

**Be honest about what is your preference and what is a requirement.** "I would write
this differently" is not the same as "this must change." If you are expressing a
preference, say so. If you are citing a hard requirement — architecture, security,
compatibility — cite the specific reason. Authors who cannot tell the difference between
reviewer preference and architectural necessity will either change everything or change
nothing. Neither serves them well.

**The team you are helping build is the team you will work in.** The investment you make
in a careful, educational review today compounds into a contributor who writes better
code, opens better PRs, and reviews others more thoughtfully. That makes the project
better. It also makes your own work easier, because the people around you are growing.

This is not a soft skill. It is engineering work.

---
