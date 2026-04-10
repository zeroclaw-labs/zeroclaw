---
name: wiki-management
version: 1.0.0
description: >
  Maintain a structured markdown wiki as your primary knowledge store. Use this
  skill whenever you learn something worth keeping, ingest a document or URL,
  answer a question that requires accumulated knowledge, or need to check what
  you know about a topic. Also use when Dan says "remember this", "what do we
  know about", "add this to the wiki", "look up", or asks about any topic where
  your wiki might have relevant pages. If you are about to use memory_store for
  anything other than a quick transient fact, use the wiki instead.
always: false
---

# Wiki Management

Your wiki is a persistent, structured knowledge base at /data/workspace/wiki/.
It grows as you ingest sources, answer questions, and learn from tasks.
The wiki is your primary knowledge store. Use memory_store only for transient
session-level facts. Anything worth keeping across sessions goes in the wiki.

## Directory Structure

```
/data/workspace/wiki/
  index.md        — catalog of all pages with one-line summaries
  log.md          — append-only operations log
  sources/        — immutable raw documents (never modify after saving)
  entities/       — pages about people, projects, systems, services
  concepts/       — pages about topics, decisions, patterns, technologies
  syntheses/      — cross-cutting analysis spanning multiple entities/concepts
```

## Bootstrap

On first use, create the directory structure and seed files:

shell: mkdir -p /data/workspace/wiki/sources /data/workspace/wiki/entities /data/workspace/wiki/concepts /data/workspace/wiki/syntheses

file_write index.md:
```
# Wiki Index
Updated: YYYY-MM-DD

## Entities
(none yet)

## Concepts
(none yet)

## Syntheses
(none yet)
```

file_write log.md:
```
# Wiki Operations Log
Newest entries at top.
```

## Operations

### Ingest

When Dan shares a document, URL content, meeting summary, or any knowledge source:

1. Save the raw source to sources/ with a descriptive filename (sources/2026-04-08-speakr-architecture-notes.md). Never modify source files after saving.
2. Read the source and identify entities (people, projects, systems) and concepts (topics, decisions, patterns).
3. For each entity or concept:
   - Check if a wiki page already exists (glob_search wiki/entities/ or wiki/concepts/).
   - If exists: file_edit to add new information with a dated section header. Note the source.
   - If new: file_write a new page using the page template below.
4. If the source connects multiple entities/concepts in a novel way, write a synthesis page.
5. Update index.md with any new pages.
6. Append to log.md: date, operation, source filename, pages created/updated.

### Query

When Dan asks about a topic or you need accumulated knowledge:

1. Search index.md for relevant pages.
2. If not obvious from the index, use content_search across wiki/ for keywords.
3. Read the relevant pages and synthesize an answer.
4. Cite which wiki pages informed your answer.
5. If the query revealed a gap or a new insight, file it back:
   - New insight: update the relevant page or create a synthesis.
   - Gap identified: note it in the page with a "Gap:" marker for lint to find.
   - Append the query and outcome to log.md.

### Lint

Run as a daily cron (or on-demand when Dan asks to check wiki health):

1. glob_search wiki/ for all .md files.
2. Check each page against index.md. Flag pages missing from the index.
3. Check index.md entries against disk. Flag entries where the file is missing.
4. Search for "Gap:" markers across all pages. Report unresolved gaps.
5. Check for stale pages: anything not updated in 30+ days that references active projects.
6. If issues found: report to Dan. If clean: end silently.

Cron setup (if not already present):
name=wiki-lint, schedule=0 9 * * 1-5, timezone=America/Vancouver, job_type=agent, session_target=isolated, delivery=none.

Cron prompt:
```
You are in an isolated cron session. Do not call cron_run.
Use the wiki-management skill to run a lint check.
Scan all wiki pages for: missing index entries, orphaned index references, Gap markers, stale pages.
If all clean: end immediately.
If issues found: send Dan a brief summary via send_user_message listing what needs attention.
```

## Page Template

Every wiki page follows this structure:

```
# [Page Title]
Category: entity | concept | synthesis
Created: YYYY-MM-DD
Last updated: YYYY-MM-DD
Related: [[other-page-name]], [[another-page]]

## Summary
One paragraph overview.

## Details
Main content organized by topic. Use dated section headers when adding
new information over time:

### 2026-04-08 — [Source context]
New information added from [source reference].

## Open Questions
- Any unresolved questions (prefix with "Gap:" for lint to find)
```

## Cross-References

Use [[page-name]] syntax for cross-references (e.g., [[walter]], [[istio-ambient-mesh]]).
These are plain text markers now. When the wiki module is built, they become real links.
When creating or updating a page, add reciprocal references: if page A references page B,
page B should reference page A.

## What Goes Where

Entities (wiki/entities/): things with identity. People (dan.md, walter.md), projects (zeroclaw.md, speakr.md), systems (homelab-cluster.md), services (vikunja.md, gitea.md).

Concepts (wiki/concepts/): ideas and knowledge. Technologies (istio-ambient-mesh.md), decisions (signal-cli-spqr-migration.md), patterns (k8s-deployment-checklist.md).

Syntheses (wiki/syntheses/): analysis that connects multiple entities/concepts. Architecture overviews, project retrospectives, decision records that span systems.

## Ingest Triggers

These events should trigger a wiki ingest:
- Dan shares a document or URL ("check this out", "read this", "save this")
- Meeting summary cron completes (ingest key decisions and action items)
- You complete a multi-step task with lessons learned
- Dan makes a decision worth recording
- You discover something during research

## Tools Used

file_read, file_write, file_edit: page CRUD.
glob_search: find pages by name pattern.
content_search: full-text search across wiki.
memory_store: quick-lookup cache only (key=wiki-ref/page-name, value=one-line summary).
send_user_message: lint notifications.
cron_add: lint scheduling.

When the dedicated wiki module ships, these will be replaced by wiki_ingest, wiki_query, wiki_lint tools. The directory structure and page format will stay the same.
