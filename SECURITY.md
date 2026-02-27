# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Report a Vulnerability (Private)

Please do not open public GitHub issues for unpatched security vulnerabilities.

ZeroClaw uses GitHub's private vulnerability reporting and advisory workflow for important security issues.

Preferred reporting paths:

1. If you are a researcher or user:
   - Go to `Security` -> `Report a vulnerability`.
   - Private reporting is enabled for this repository.
   - Use this report template:
     - English: [`docs/security/private-vulnerability-report-template.md`](docs/security/private-vulnerability-report-template.md)
     - 中文: [`docs/security/private-vulnerability-report-template.zh-CN.md`](docs/security/private-vulnerability-report-template.zh-CN.md)
2. If you are a maintainer/admin opening a draft directly:
   - <https://github.com/zeroclaw-labs/zeroclaw/security/advisories/new>

### What to Include in a Report

- Vulnerability summary and security impact
- Affected versions, commits, or deployment scope
- Reproduction steps and prerequisites
- Safe/minimized proof of concept
- Suggested mitigation or patch direction (if known)
- Any known workaround

## Official Channels and Anti-Fraud Notice

Impersonation scams are a real risk in open communities.

Security-critical rule:

- ZeroClaw maintainers will not ask for cryptocurrency, wallet seed phrases, or private financial credentials.
- Treat direct-message payment requests as fraudulent unless independently verified in the repository.
- Verify announcements using repository sources first.

Canonical statement and reporting guidance:

- [docs/security/official-channels-and-fraud-prevention.md](docs/security/official-channels-and-fraud-prevention.md)

## Maintainer Handling Workflow (GitHub-Native)

### 1. Intake and triage (private)

When a report arrives in `Security` -> `Advisories` with `Triage` status:

1. Confirm whether this is a security issue.
2. Choose one path:
   - `Accept and open as draft` for likely/confirmed security issues.
   - `Start a temporary private fork` for embargoed fix collaboration.
   - Request more details in advisory comments.
   - Close only when confirmed non-security, with rationale.

Maintainers should run the lifecycle checklist:

- English: [`docs/security/advisory-maintainer-checklist.md`](docs/security/advisory-maintainer-checklist.md)
- 中文: [`docs/security/advisory-maintainer-checklist.zh-CN.md`](docs/security/advisory-maintainer-checklist.zh-CN.md)
- Advisory metadata template:
  - English: [`docs/security/advisory-metadata-template.md`](docs/security/advisory-metadata-template.md)
  - 中文: [`docs/security/advisory-metadata-template.zh-CN.md`](docs/security/advisory-metadata-template.zh-CN.md)

### 2. Private fix development and verification

Develop embargoed fixes in the advisory temporary private fork.

Important constraints in temporary private forks:

- Status checks do not run there.
- Branch protection rules are not enforced there.
- You cannot merge individual PRs one by one there.

Required verification before disclosure:

- Reproduce the vulnerability and verify the fix.
- Run full local validation:
  - `cargo test --workspace --all-targets`
- Run targeted security regressions:
  - `cargo test -- security`
  - `cargo test -- tools::shell`
  - `cargo test -- tools::file_read`
  - `cargo test -- tools::file_write`
- Ensure no exploit details or secrets leak into public channels.

### 3. Publish advisory with actionable remediation

Before publishing a repository security advisory:

- Fill affected version ranges precisely.
- Provide fixed version(s) whenever possible.
- Include mitigations when no fixed release is available yet.

Then publish the advisory to disclose publicly and enable downstream remediation workflows.

### 4. CVE and post-disclosure maintenance

- Request a CVE from GitHub when appropriate, or attach existing CVE IDs.
- Update affected/fixed version ranges if scope changes.
- Backport fixes where needed and keep advisory metadata aligned.

## Internal Rule for Critical Security Issues

For high-severity security issues (for example sandbox escape, auth bypass, data exfiltration, or RCE):

- Do not use public issues as primary tracking before remediation.
- Do not publish exploit details in public PRs before advisory publication.
- Use GitHub Security Advisory workflow first, then coordinate release/disclosure.

## Response Timeline Targets

- Acknowledgment: within 48 hours
- Initial triage: within 7 days
- Critical fix target: within 14 days (or publish mitigation plan)

## Severity Levels and SLA Matrix

These SLAs are target windows for private security handling and may be adjusted based on complexity and dependency constraints.

| Severity | Typical impact examples | Acknowledgment target | Triage target | Initial mitigation target | Fix release target |
| ------- | ----------------------- | --------------------- | ------------- | ------------------------- | ------------------ |
| S0 Critical | Active exploitation, unauthenticated RCE, broad data exfiltration | 24 hours | 72 hours | 72 hours | 7 days |
| S1 High | Auth bypass, privilege escalation, significant data exposure | 24 hours | 5 days | 7 days | 14 days |
| S2 Medium | Constrained exploit path, partial data/control impact | 48 hours | 7 days | 14 days | 30 days |
| S3 Low | Limited impact, hard-to-exploit, defense-in-depth gaps | 72 hours | 14 days | As needed | Next planned release |

SLA guidance notes:

- Severity is assigned during private triage and can be revised with new evidence.
- If active exploitation is observed, prioritize mitigation and containment over full feature work.
- When a fixed release is delayed, publish mitigations/workarounds in advisory notes first.

## Severity Assignment Guide

Use the S0-S3 matrix as operational severity. CVSS is an input, not the only decision factor.

| Severity | Typical CVSS range | Assignment guidance |
| ------- | ------------------ | ------------------- |
| S0 Critical | 9.0-10.0 | Active exploitation or near-term exploitability with severe impact (for example pre-auth RCE or broad data exfiltration). |
| S1 High | 7.0-8.9 | High-impact security boundary break with practical exploit path. |
| S2 Medium | 4.0-6.9 | Meaningful but constrained impact due to required conditions or lower blast radius. |
| S3 Low | 0.1-3.9 | Limited impact or defense-in-depth gap with hard-to-exploit conditions. |

Severity override rules:

- Escalate one level when reliable evidence of active exploitation exists.
- Escalate one level when affected surface includes default configurations used by most deployments.
- De-escalate one level only with documented exploit constraints and validated compensating controls.

## Public Communication and Commit Hygiene (Pre-Disclosure)

Before advisory publication:

- Keep exploit-specific details in private advisory threads only.
- Avoid explicit vulnerability naming in public branch names and PR titles.
- Keep public commit messages neutral and fix-oriented (avoid step-by-step exploit instructions).
- Do not include secrets or sensitive payloads in logs, snippets, or screenshots.

## Security Architecture

ZeroClaw uses defense-in-depth controls.

### Autonomy Levels

- `ReadOnly`: read access only, no shell/file write
- `Supervised`: policy-constrained actions (default)
- `Full`: broader autonomy within workspace sandbox constraints

### Sandboxing Layers

1. Workspace isolation for file operations
2. Path traversal blocking for unsafe path patterns
3. Command allowlisting for shell execution
4. Forbidden path controls for critical system locations
5. Runtime safeguards for rate/cost/safety limits

### Threats Addressed

- Path traversal (for example `../../../etc/passwd`)
- Command injection (for example `curl | sh`)
- Workspace escape via symlink/absolute path abuse
- Unauthorized shell execution
- Runaway tool/model usage

## Security Testing

Core security mechanisms are validated with automated tests:

```bash
cargo test --workspace --all-targets
cargo test -- security
cargo test -- tools::shell
cargo test -- tools::file_read
cargo test -- tools::file_write
```

## Container Security

ZeroClaw images follow CIS Docker Benchmark-oriented hardening.

| Control | Implementation |
| ------- | -------------- |
| 4.1 Non-root user | Container runs as UID 65534 (distroless nonroot) |
| 4.2 Minimal base image | `gcr.io/distroless/cc-debian12:nonroot` |
| 5.25 Read-only filesystem | Supported via `docker run --read-only` with `/workspace` volume |

### Verifying Container Security

```bash
# Build and verify non-root user
docker build -t zeroclaw .
docker inspect --format='{{.Config.User}}' zeroclaw
# Expected: 65534:65534

# Run with read-only filesystem (production hardening)
docker run --read-only -v /path/to/workspace:/workspace zeroclaw gateway
```

### CI Enforcement

The `docker` job in `.github/workflows/ci.yml` verifies:

1. Container does not run as root (UID 0)
2. Runtime stage uses `:nonroot` base
3. `USER` directive with numeric UID exists

## References

- How-tos for fixing vulnerabilities:
  - <https://docs.github.com/en/enterprise-cloud@latest/code-security/how-tos/report-and-fix-vulnerabilities/fix-reported-vulnerabilities>
- Managing privately reported vulnerabilities:
  - <https://docs.github.com/en/enterprise-cloud@latest/code-security/how-tos/report-and-fix-vulnerabilities/fix-reported-vulnerabilities/managing-privately-reported-security-vulnerabilities>
- Collaborating in temporary private forks:
  - <https://docs.github.com/en/enterprise-cloud@latest/code-security/tutorials/fix-reported-vulnerabilities/collaborate-in-a-fork>
- Publishing repository advisories:
  - <https://docs.github.com/en/enterprise-cloud@latest/code-security/how-tos/report-and-fix-vulnerabilities/fix-reported-vulnerabilities/publishing-a-repository-security-advisory>
