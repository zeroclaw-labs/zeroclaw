# Factory Foreman Policy

## Responsibility

Factory Foreman coordinates factory roles. It does not own cleanup policy, intake policy, or replay policy; those remain in the individual role policies.

## Mode Mapping

| Foreman mode | Testbench | Clerk | Inspector |
|---|---|---|---|
| `preview` | `fixture-test` | `preview` | `preview` |
| `comment-only` | `fixture-test` | `comment-only` | `comment-only` |
| `apply-safe` | `fixture-test` | `apply-safe` | `comment-only` |

## Guardrails

- Testbench must pass before any non-preview Clerk/Inspector run.
- `apply-safe` requires `--allow-apply-safe`.
- Scheduled workflows must use `preview`.
- Foreman must pass `--max-mutations` to mutating roles.
- Foreman must not bypass role-level protected labels, hidden markers, or review-only decisions.

## Failure Handling

If a role exits non-zero, Foreman exits non-zero and writes the partial summary. Do not continue to later mutating roles after a failure.
