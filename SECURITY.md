# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

**Please do NOT open a public GitHub issue for security vulnerabilities.**

Instead, please report them responsibly:

1. **Email**: Send details to the maintainers via GitHub private vulnerability reporting
2. **GitHub**: Use [GitHub Security Advisories](https://github.com/theonlyhennygod/zeroclaw/security/advisories/new)

### What to Include

- Description of the vulnerability
- Steps to reproduce
- Impact assessment
- Suggested fix (if any)

### Response Timeline

- **Acknowledgment**: Within 48 hours
- **Assessment**: Within 1 week
- **Fix**: Within 2 weeks for critical issues

## Security Architecture

ZeroClaw implements defense-in-depth security:

### Autonomy Levels
- **ReadOnly** — Agent can only read, no shell or write access
- **Supervised** — Agent can act within allowlists (default)
- **Full** — Agent has full access within workspace sandbox

### Sandboxing Layers
1. **Workspace isolation** — All file operations confined to workspace directory
2. **Path traversal blocking** — `..` sequences and absolute paths rejected
3. **Command allowlisting** — Only explicitly approved commands can execute
4. **Forbidden path list** — Critical system paths (`/etc`, `/root`, `~/.ssh`) always blocked
5. **Rate limiting** — Max actions per hour and cost per day caps

### What We Protect Against
- Path traversal attacks (`../../../etc/passwd`)
- Command injection (`rm -rf /`, `curl | sh`)
- Workspace escape via symlinks or absolute paths
- Runaway cost from LLM API calls
- Unauthorized shell command execution

## Security Testing

All security mechanisms are covered by automated tests (129 tests):

```bash
cargo test -- security
cargo test -- tools::shell
cargo test -- tools::file_read
cargo test -- tools::file_write
```

## Container Security

ZeroClaw Docker images follow CIS Docker Benchmark best practices:

| Control | Implementation |
|---------|----------------|
| **4.1 Non-root user** | Container runs as UID 65534 (distroless nonroot) |
| **4.2 Minimal base image** | `gcr.io/distroless/cc-debian12:nonroot` — no shell, no package manager |
| **4.6 HEALTHCHECK** | Not applicable (stateless CLI/gateway) |
| **5.25 Read-only filesystem** | Supported via `docker run --read-only` with `/workspace` volume |

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

The `docker` job in `.github/workflows/ci.yml` automatically verifies:
1. Container does not run as root (UID 0)
2. Runtime stage uses `:nonroot` variant
3. Explicit `USER` directive with numeric UID exists
