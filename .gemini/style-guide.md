# ZeroClaw Code Style Guide

This style guide provides instructions for Gemini Code Assist when reviewing pull requests for the ZeroClaw project.

## Project Overview

ZeroClaw is a Rust-based security-focused project that handles encryption, secrets management, and secure configuration. Code reviews should prioritize security, memory safety, and Rust best practices.

## General Principles

### Priority Levels

- **CRITICAL**: Security vulnerabilities, memory safety issues, data leaks
- **HIGH**: Logic errors, incorrect error handling, API misuse
- **MEDIUM**: Code quality, performance concerns, non-idiomatic Rust
- **LOW**: Style issues, documentation improvements, minor refactoring

## Rust-Specific Guidelines

### Memory Safety

1. **Borrowing and Lifetimes**: Verify proper use of borrowing and lifetime annotations
2. **Unsafe Code**: Flag any `unsafe` blocks for careful review - they should be minimal and well-justified
3. **Clone Usage**: Identify unnecessary `.clone()` calls that could be replaced with borrowing
4. **Memory Leaks**: Watch for potential memory leaks in long-running processes

### Error Handling

1. **Result Types**: All fallible operations should return `Result` types
2. **Error Propagation**: Use `?` operator for clean error propagation
3. **Custom Errors**: Ensure custom error types implement appropriate traits
4. **Panic**: Flag any uses of `panic!`, `unwrap()`, or `expect()` in production code

### Security

1. **Cryptography**: Review all crypto code for:
   - Proper key generation and storage
   - Secure random number generation
   - No hardcoded secrets or keys
   - Use of well-vetted crypto libraries

2. **Secrets Management**:
   - Secrets should never be logged
   - Use secure memory wiping when appropriate
   - Validate encryption/decryption implementations

3. **Input Validation**: All external input must be validated

### Code Quality

1. **Documentation**: Public APIs should have doc comments with examples
2. **Tests**: Critical paths should have comprehensive test coverage
3. **Type Safety**: Prefer type-safe abstractions over primitive types
4. **Idiomatic Rust**: Follow Rust API guidelines and conventions

## Project-Specific Rules

### Configuration Management

- Configuration migrations must be backward compatible
- Validate all configuration before applying
- Test migration paths from legacy to new formats

### Dependencies

- Prefer well-maintained crates with security audit history
- Avoid unnecessary dependencies
- Check for known vulnerabilities in dependencies

## Review Focus Areas

When reviewing PRs, pay special attention to:

1. Changes in `src/security/` - highest security scrutiny
2. Configuration migration code - ensure data integrity
3. Error handling paths - verify all edge cases
4. Public API changes - check for breaking changes
5. Test coverage - ensure critical code is tested

## Common Issues to Flag

- Unhandled errors or generic error messages
- Missing input validation
- Hardcoded credentials or secrets
- Unsafe code without justification
- Missing documentation on public APIs
- Inadequate test coverage on security-critical code
- Performance issues (unnecessary allocations, inefficient algorithms)
- Breaking API changes without deprecation warnings
