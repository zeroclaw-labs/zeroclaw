# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in ClawSuite, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, email: **security@clawsuite.io**

We will acknowledge your report within 48 hours and aim to provide a fix within 7 days for critical issues.

## Scope

- ClawSuite web application code
- API routes and gateway communication
- Client-side data handling
- Authentication and authorization (when implemented)

## Out of Scope

- OpenClaw gateway itself (report to [OpenClaw](https://github.com/openclaw/openclaw))
- Third-party dependencies (report to the respective maintainer)
- Social engineering attacks

## Security Measures

- API keys and tokens are never bundled in client-side code
- Gateway tokens are server-side only
- Diagnostic output is scrubbed of sensitive data
- Environment files are gitignored

## Supported Versions

| Version | Supported |
| ------- | --------- |
| main    | ✅        |
| < main  | ❌        |
