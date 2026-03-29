<!--
  Thanks for contributing to Hrafn!
  Fill in what's relevant, delete what's not.
  Small PRs don't need every section.
-->

## What

<!-- What changed and why? Link the issue. -->

Closes #

## How to test

<!-- Steps or commands so a reviewer can verify. -->

```bash
cargo test -- relevant_test_name
```

## Breaking changes

<!-- Delete this section if none. -->
<!-- Config key renamed? CLI flag changed? Trait signature updated? -->

- [ ] This PR includes breaking changes

**Migration:** <!-- How do users update? -->

## Security

<!-- Delete this section if not security-relevant. -->

- [ ] New network calls or endpoints
- [ ] Secrets/tokens handling changed
- [ ] File system access scope changed
- [ ] SSRF / input validation implications

**Risk and mitigation:** <!-- Brief description -->

## Checklist

- [ ] `cargo fmt && cargo clippy -D warnings` passes
- [ ] Tests pass, new code has tests
- [ ] I can explain every line in this PR
