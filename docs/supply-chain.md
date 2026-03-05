# Supply-Chain Policy

ZeroClaw enforces dependency risk controls with two complementary tools:

1. **`cargo-deny`** for vulnerability/license/source policy gates
2. **`cargo-vet`** for review attestations and controlled bootstrap exemptions

## What is enforced

### 1) `cargo-deny` policy (merge-gating)

Policy file: [`deny.toml`](../deny.toml)

- RustSec advisories are checked
- Allowed licenses are explicitly enumerated
- Unknown registries/git sources are denied
- Ignore exceptions require governance metadata and reason/expiry tracking

CI job:
- `.github/workflows/sec-audit.yml` → `License & Supply Chain`
- command: `cargo-deny check advisories licenses sources`

### 2) `cargo-vet` baseline (review attestations)

Store files:
- [`supply-chain/config.toml`](../supply-chain/config.toml)
- [`supply-chain/audits.toml`](../supply-chain/audits.toml)
- [`supply-chain/imports.lock`](../supply-chain/imports.lock)

Initial bootstrap was generated with `cargo vet init` to make current dependency graph auditable without blocking delivery. This creates explicit exemptions that can be gradually replaced by real audits/imported attestations.

CI job:
- `.github/workflows/sec-audit.yml` → `License & Supply Chain`
- command: `cargo vet check --locked`

## Local developer workflow

```bash
# deny policy checks
cargo-deny check advisories licenses sources

# vet policy checks
cargo vet check --locked
```

## Maintenance expectations

- Keep `deny.toml` exceptions minimal and justified.
- Prefer removing exemptions by upgrading dependencies or adding audited attestations.
- Use `cargo vet suggest` / `cargo vet certify` to work down review backlog.
- Treat new dependency introductions as supply-chain review events.
