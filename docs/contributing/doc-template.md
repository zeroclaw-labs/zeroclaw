# Documentation Template (Operational)

Use this template when adding a new operational or engineering document under `docs/`.

Keep sections that apply; remove non-applicable placeholders before merging.

---

## 1. Summary

- **Purpose:** <one sentence about why this document exists>
- **Audience:** <operators | reviewers | contributors | maintainers>
- **Scope:** <what this doc covers>
- **Non-goals:** <what this doc intentionally does not cover>

## 2. Prerequisites

- <required environment>
- <required permissions>
- <required tools/config>

## 3. Procedure

### 3.1 Baseline Check

1. <step>
2. <step>

### 3.2 Main Workflow

1. <step>
2. <step>
3. <step>

### 3.3 Verification

- <expected output or success signal>
- <validation command/log/checkpoint>

## 4. Safety, Risk, and Rollback

- **Risk surface:** <which components may be impacted>
- **Failure modes:** <what can go wrong>
- **Rollback plan:** <concrete rollback command/steps>

## 5. Troubleshooting

- **Symptom:** <error/signal>
  - **Cause:** <likely cause>
  - **Fix:** <action>

## 6. Related Docs

- [README.md](./README.md) — documentation taxonomy and navigation.
- <related-doc-1.md>
- <related-doc-2.md>

## 7. Maintenance Notes

- **Owner:** <team/persona/area>
- **Update trigger:** <what changes should force this doc update>
- **Last reviewed:** <YYYY-MM-DD>

