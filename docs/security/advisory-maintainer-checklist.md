# Security Advisory Maintainer Checklist

Use this checklist for high-impact vulnerabilities from private intake through disclosure.

## A) Intake and Triage (Private)

- [ ] Confirm report is in private advisory workflow (not public issue).
- [ ] Validate reproducibility and impact.
- [ ] Classify severity and affected components.
- [ ] Record severity rationale and SLA clock start time.
- [ ] Decide handling path:
  - [ ] Accept and open as draft advisory
  - [ ] Request additional details
  - [ ] Close as non-security with rationale
- [ ] Add initial timeline/owner in advisory discussion.

## B) Embargoed Fix Development

- [ ] Start or use advisory temporary private fork.
- [ ] Keep exploit details out of public PR/issue threads.
- [ ] Implement minimal-risk patch and tests.
- [ ] Run required validation locally:
  - [ ] `cargo test --workspace --all-targets`
  - [ ] `cargo test -- security`
  - [ ] `cargo test -- tools::shell`
  - [ ] `cargo test -- tools::file_read`
  - [ ] `cargo test -- tools::file_write`
- [ ] Prepare backports if supported versions require them.

## C) Advisory Metadata Quality

- [ ] Use advisory metadata template: `docs/security/advisory-metadata-template.md`.
- [ ] Affected package/ecosystem fields are correct.
- [ ] Affected version range is precise.
- [ ] Fixed version(s) are present, or mitigation is documented.
- [ ] CWE/CVSS fields are filled where possible.
- [ ] References include patch commit(s) and release notes.

## D) Disclosure and Post-Disclosure

- [ ] Publish advisory when fix/mitigation is ready.
- [ ] Request CVE (or attach existing CVE) when appropriate.
- [ ] Verify published advisory references released fix artifacts.
- [ ] Confirm downstream notifications/dependency signals are aligned.
- [ ] Monitor regressions or bypass reports and update advisory metadata if scope changes.

## E) Internal Hygiene

- [ ] No secrets in commits, logs, CI output, or discussion threads.
- [ ] No unnecessary exploit detail in public channels before disclosure.
- [ ] Security response timeline and decision log are captured in advisory comments.
