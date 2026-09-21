//! Integration test: the Landlock sandbox must leave the host's DNS
//! configuration and TLS trust store readable inside the sandboxed child.
//!
//! Landlock is filesystem-only and never confines network egress, so it is easy
//! to assume outbound requests are unaffected by it. They are not: glibc's
//! resolver reads `/etc/resolv.conf` and friends to turn a hostname into an
//! address, and OpenSSL reads the CA bundle to verify a peer. If the allow-list
//! omits those paths, a sandboxed tool gets "Temporary failure in name
//! resolution" and "unable to get local issuer certificate" even though the
//! socket layer is untouched.
//!
//! The other Landlock tests assert ruleset construction, that the parent is
//! never restricted, and the workspace boundary. None of them notice if these
//! paths are dropped from the allow-list, which is what this test covers.
//!
//! nextest runs each test in a separate process, so this test can fork a
//! Landlock-restricted child without affecting other tests.

#![cfg(all(feature = "sandbox-landlock", target_os = "linux"))]

use std::path::Path;
use std::process::Command;
use zeroclaw_runtime::security::landlock::LandlockSandbox;
use zeroclaw_runtime::security::traits::Sandbox;

/// Files glibc's resolver consults during `getaddrinfo`. Which of these exist
/// varies by distro and by resolver stack, so the test probes rather than
/// assumes.
const DNS_CONFIG_PATHS: &[&str] = &[
    "/etc/resolv.conf",
    "/etc/nsswitch.conf",
    "/etc/hosts",
    "/etc/gai.conf",
];

/// CA bundle locations across distro layouts: Arch/BSD-style, Debian/Ubuntu,
/// and RHEL/Fedora respectively.
///
/// Reading these through their conventional path is the point: on Arch-family
/// systems `/etc/ssl/cert.pem` is a symlink into `/etc/ca-certificates/extracted`,
/// so a rule covering only the link and not its target would satisfy a naive
/// existence check while still failing an actual read.
const TLS_TRUST_PATHS: &[&str] = &[
    "/etc/ssl/cert.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/certs/ca-bundle.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
];

/// A path is only worth asserting on if the *unrestricted* parent can read it.
/// Otherwise a child failure would be attributable to DAC or to the file simply
/// not existing, rather than to Landlock.
fn parent_can_read(path: &str) -> bool {
    Path::new(path).is_file() && std::fs::read(path).is_ok()
}

#[test]
fn landlock_grants_dns_and_tls_config_access() {
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp workspace");
    let sandbox = LandlockSandbox::with_workspace(Some(workspace.path().to_path_buf()))
        .expect("landlock should succeed on linux with feature enabled");

    // `/etc/passwd` is world-readable on every Linux but is deliberately NOT in
    // the allow-list. It serves as the proof that the ruleset actually took
    // effect in the child: without it, every positive assertion below would
    // pass vacuously on a host where enforcement silently did not apply.
    const SENTINEL: &str = "/etc/passwd";
    assert!(
        parent_can_read(SENTINEL),
        "{SENTINEL} must be readable by the parent to act as a sentinel — \
         test environment is broken",
    );

    let dns_paths: Vec<&str> = DNS_CONFIG_PATHS
        .iter()
        .copied()
        .filter(|p| parent_can_read(p))
        .collect();
    let tls_paths: Vec<&str> = TLS_TRUST_PATHS
        .iter()
        .copied()
        .filter(|p| parent_can_read(p))
        .collect();

    // Fail loudly rather than degrading into a no-op. If a runner has no
    // resolver config or no trust store at all, this test proves nothing and
    // should say so instead of reporting a green pass.
    assert!(
        !dns_paths.is_empty(),
        "no readable DNS configuration found among {DNS_CONFIG_PATHS:?} — \
         cannot verify the resolver allow-list on this host",
    );
    assert!(
        !tls_paths.is_empty(),
        "no readable CA bundle found among {TLS_TRUST_PATHS:?} — \
         cannot verify the TLS trust store allow-list on this host",
    );

    let mut script = String::from("set -e\n");

    // Negative probes redirect stderr to /dev/null. Prove /dev/null is
    // reachable first so a later redirect failure is attributable to the probed
    // operation rather than to a blocked /dev/null.
    script.push_str(
        "if ! echo test > /dev/null; then \
            echo 'FAIL: child cannot access /dev/null — redirects are unreliable' >&2; \
            exit 1; \
         fi\n",
    );

    // Positive: every resolver and trust-store file the parent could read must
    // also be readable in the child. `cat` follows symlinks, so this exercises
    // the link *and* its target.
    for path in dns_paths.iter().chain(tls_paths.iter()) {
        script.push_str(&format!(
            "if ! cat {path} > /dev/null 2>&1; then \
                echo 'FAIL: sandboxed child cannot read {path}' >&2; \
                exit 1; \
             fi\n",
        ));
    }

    // Positive: OpenSSL's hashed `capath` lookup enumerates this directory
    // rather than opening a known filename, so ReadDir must be granted too and
    // is not implied by the ReadFile assertions above.
    if Path::new("/etc/ssl/certs").is_dir() {
        script.push_str(
            "if ! ls /etc/ssl/certs > /dev/null 2>&1; then \
                echo 'FAIL: sandboxed child cannot enumerate /etc/ssl/certs' >&2; \
                exit 1; \
             fi\n",
        );
    }

    // Negative: the sentinel must stay denied. An explicit `if cmd; then exit 1`
    // is used instead of `! cmd` because bash exempts `!`-negated commands from
    // `set -e`, so an unexpected success would not terminate the script.
    script.push_str(&format!(
        "if cat {SENTINEL} > /dev/null 2>&1; then \
            echo 'FAIL: {SENTINEL} was readable — the ruleset is not being enforced, \
so the positive assertions above prove nothing' >&2; \
            exit 1; \
         fi\n",
    ));

    let mut cmd = Command::new("bash");
    cmd.args(["-c", &script]);

    sandbox
        .wrap_command(&mut cmd)
        .expect("landlock should successfully wrap the command");

    let status = cmd
        .spawn()
        .expect("should spawn bash under landlock restrictions")
        .wait()
        .expect("should wait for bash to complete");

    assert!(
        status.success(),
        "network-config access contract failed: the sandboxed child must be able \
         to read {dns_paths:?} and {tls_paths:?} while {SENTINEL} stays denied; \
         exit status: {status}",
    );
}

/// The resolver and trust-store grants must stay read-only and must not extend
/// to adjacent secret or state material.
///
/// The read rules are deliberately narrow: `/run/systemd/resolve` is granted
/// without a write right, and the CA grants name certificate subpaths rather
/// than `/etc/ssl` or `/etc/pki` wholesale, both of which recursively cover a
/// `private/` directory holding server private keys.
#[test]
fn landlock_network_config_grants_stay_read_only_and_narrow() {
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp workspace");
    let sandbox = LandlockSandbox::with_workspace(Some(workspace.path().to_path_buf()))
        .expect("landlock should succeed on linux with feature enabled");

    let mut script = String::from("set -e\n");
    let mut asserted = 0usize;

    // Resolver state must not be writable. `PathBeneath` is recursive, so a
    // write right on the resolver directory would reach these files and let a
    // sandboxed child rewrite DNS configuration.
    //
    // Note on strength: on a host where DAC already denies the write (these are
    // normally root-owned), this pins the contract rather than isolating
    // Landlock as the cause. It still fails loudly if the rule is widened on a
    // deployment whose process identity can write them.
    for state in [
        "/run/systemd/resolve/resolv.conf",
        "/run/systemd/resolve/stub-resolv.conf",
    ] {
        if Path::new(state).exists() {
            script.push_str(&format!(
                "if : > {state} 2>/dev/null; then \
                    echo 'FAIL: resolver state {state} must not be writable' >&2; \
                    exit 1; \
                 fi\n",
            ));
            asserted += 1;
        }
    }

    // Private-key directories that sit beside the granted trust material must
    // stay unreachable. Only assert where the unrestricted parent can list the
    // directory, so a denial in the child is attributable to Landlock rather
    // than to DAC or the path being absent.
    for private in ["/etc/ssl/private", "/etc/pki/tls/private"] {
        let p = Path::new(private);
        if p.is_dir() && std::fs::read_dir(p).is_ok() {
            script.push_str(&format!(
                "if ls {private} > /dev/null 2>&1; then \
                    echo 'FAIL: {private} must not be reachable from the sandbox' >&2; \
                    exit 1; \
                 fi\n",
            ));
            asserted += 1;
        }
    }

    assert!(
        asserted > 0,
        "no resolver-state or private-key path present to assert against — \
         this host cannot verify the narrowness of the network-config grants",
    );

    let mut cmd = Command::new("bash");
    cmd.args(["-c", &script]);
    sandbox
        .wrap_command(&mut cmd)
        .expect("landlock should successfully wrap the command");
    let status = cmd
        .spawn()
        .expect("should spawn bash under landlock restrictions")
        .wait()
        .expect("should wait for bash to complete");

    assert!(
        status.success(),
        "network-config grants must be read-only and must not cover private-key \
         or resolver-state material; exit status: {status}",
    );
}
