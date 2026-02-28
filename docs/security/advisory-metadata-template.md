# Security Advisory Metadata Template

Use this template when preparing advisory metadata before publication.

---

## Advisory Basics

- Advisory title:
- Internal tracking reference (non-public):
- Severity level (S0/S1/S2/S3):
- CVSS score and vector (if available):
- CWE ID(s):
- CVE ID (if assigned):

## Summary

- One-paragraph vulnerability summary:
- Exploit preconditions:
- Impact scope:

## Affected Scope

- Ecosystem:
- Package name:
- Affected version range:
- First affected version (if known):
- Last affected version (if known):
- Fixed version(s):

### Version Range Notes

Use a precise, machine-readable range. Include examples as needed:

- Rust crate example: `>=0.1.0, <0.1.8`
- Single affected release: `=0.1.5`
- Multiple windows: `<0.1.4 || >=0.1.6, <0.1.8`

## Technical Details (Public-safe)

- Root cause summary:
- Vulnerable code path(s):
- Attack vector type (remote/local/authenticated):
- Security boundary crossed:

## Mitigations and Workarounds

- Temporary mitigation steps:
- Configuration hardening guidance:
- Detection/monitoring hints:

## Fix and Validation

- Fix PR(s) or commit SHA(s):
- Backport PR(s) or commit SHA(s):
- Validation evidence summary:
  - `cargo test --workspace --all-targets`
  - security-focused targeted tests

## References

- Patch/release notes link(s):
- External references (if any):
- Researcher credits / acknowledgments:

## Publication Checklist

- [ ] Affected and fixed versions are explicit and accurate.
- [ ] Severity/CVSS/CWE fields are populated or intentionally marked unknown.
- [ ] Mitigations are included when no fixed release exists.
- [ ] Public text excludes sensitive exploit implementation details.
- [ ] Post-disclosure monitoring owner is assigned.
