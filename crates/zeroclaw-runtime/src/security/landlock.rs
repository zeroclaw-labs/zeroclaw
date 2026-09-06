//! Landlock sandbox (Linux kernel 5.13+ LSM)
//! Landlock provides unprivileged sandboxing through the Linux kernel.
//! This module uses the pure-Rust `landlock` crate for filesystem access control.

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
use landlock::{
    AccessFs, BitFlags, Errno, PathBeneath, PathFd, PathFdError, Ruleset, RulesetAttr,
    RulesetCreated, RulesetCreatedAttr,
};
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
use std::os::unix::process::CommandExt;
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
use std::path::{Path, PathBuf};

use crate::security::traits::Sandbox;

/// Landlock sandbox backend for Linux
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
#[derive(Debug)]
pub struct LandlockSandbox {
    workspace_dir: Option<std::path::PathBuf>,
    /// Extra roots the agent may read AND write, mirroring
    /// `SecurityPolicy::allowed_roots`.
    allowed_roots: Vec<std::path::PathBuf>,
    /// Extra roots the agent may read but NOT write, mirroring
    /// `SecurityPolicy::allowed_roots_read_only`.
    allowed_roots_read_only: Vec<std::path::PathBuf>,
    /// Extra roots the agent may write but NOT read, mirroring
    /// `SecurityPolicy::allowed_roots_write_only`.
    allowed_roots_write_only: Vec<std::path::PathBuf>,
}

/// Every access right this backend's ruleset arbitrates.
///
/// Landlock only reasons about *handled* rights: anything outside this set is
/// neither granted nor withheld by any rule below. It is therefore also the
/// universe against which a tier's withheld rights are computed — a tier
/// cannot be undercut on a right Landlock never mediates.
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
fn handled_access() -> BitFlags<AccessFs> {
    AccessFs::Execute
        | AccessFs::WriteFile
        | AccessFs::ReadFile
        | AccessFs::Truncate
        | AccessFs::ReadDir
        | AccessFs::RemoveDir
        | AccessFs::RemoveFile
        | AccessFs::MakeChar
        | AccessFs::MakeDir
        | AccessFs::MakeReg
        | AccessFs::MakeSock
        | AccessFs::MakeFifo
        | AccessFs::MakeBlock
        | AccessFs::MakeSym
}

/// Rights granted to a read-write root: the primary workspace and every entry
/// of `SecurityPolicy::allowed_roots`.
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
fn read_write_access() -> BitFlags<AccessFs> {
    AccessFs::Execute
        | AccessFs::WriteFile
        | AccessFs::ReadFile
        | AccessFs::Truncate
        | AccessFs::ReadDir
        | AccessFs::RemoveDir
        | AccessFs::RemoveFile
        | AccessFs::MakeDir
        | AccessFs::MakeReg
        | AccessFs::MakeSock
        | AccessFs::MakeFifo
        | AccessFs::MakeSym
}

/// Rights granted to `SecurityPolicy::allowed_roots_read_only`.
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
fn read_only_access() -> BitFlags<AccessFs> {
    AccessFs::ReadFile | AccessFs::ReadDir
}

/// Rights granted to `SecurityPolicy::allowed_roots_write_only`.
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
fn write_only_access() -> BitFlags<AccessFs> {
    AccessFs::WriteFile
        | AccessFs::Truncate
        | AccessFs::RemoveDir
        | AccessFs::RemoveFile
        | AccessFs::MakeDir
        | AccessFs::MakeReg
        | AccessFs::MakeSock
        | AccessFs::MakeFifo
        | AccessFs::MakeSym
}

/// The static, workspace-independent rules every sandboxed child receives.
///
/// `required = true`  -> fail closed if the path is missing (baseline devices, system roots).
/// `required = false` -> skip on NotFound (distro-optional loader/layout paths).
///
/// Hoisted out of `build_ruleset` so the overlap check below and the rules
/// actually installed come from one list: a rule added here that the check
/// never saw would silently reintroduce the tier bypass it exists to catch.
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
fn generic_rules() -> [(&'static str, BitFlags<AccessFs>, bool); 23] {
    [
        // /tmp: general temp directory for child processes (pipes, sockets, temp files).
        // Execute is intentionally omitted to prevent running untrusted binaries from /tmp.
        (
            "/tmp",
            AccessFs::Truncate | AccessFs::WriteFile | AccessFs::ReadFile,
            true,
        ),
        // Linux dynamic linker (ld-linux-yourarch.so.version) which designed to run on FHS 3.0
        // system will read the following file/directories to retrieve dynamic linker config.
        // These are optional: minimal systems may not have all of them.
        ("/etc/ld.so.cache", AccessFs::ReadFile.into(), false),
        ("/etc/ld.so.conf", AccessFs::ReadFile.into(), false),
        ("/etc/ld.so.preload", AccessFs::ReadFile.into(), false),
        (
            "/etc/ld.so.conf.d",
            AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
        // In FHS 3.0 systems, system binaries will live in the following directories:
        // /usr/bin, /usr/lib, /usr/lib64, /bin, /lib, /lib64.
        // Execute: needed to run binaries (execve) and for the dynamic linker's
        // access(X_OK) checks on shared libraries.
        //
        // /usr is optional: Non-FHS distros may not have it.
        (
            "/usr",
            AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
        (
            "/bin",
            AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
            true,
        ),
        // /lib and /lib64 are distro-optional: some systems have one, some both.
        (
            "/lib",
            AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
        (
            "/lib64",
            AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
        // some variant of sh requires access to /dev/null
        ("/dev/null", AccessFs::WriteFile | AccessFs::ReadFile, true),
        // DNS resolution: glibc's resolver (used by getaddrinfo, and thus by
        // Python/most language runtimes) reads these to resolve hostnames.
        // All are optional: not every distro/config uses all of them, and a
        // missing rule here must not turn into a startup failure.
        ("/etc/resolv.conf", AccessFs::ReadFile.into(), false),
        ("/etc/nsswitch.conf", AccessFs::ReadFile.into(), false),
        ("/etc/hosts", AccessFs::ReadFile.into(), false),
        ("/etc/gai.conf", AccessFs::ReadFile.into(), false),
        // systemd-resolved: /etc/resolv.conf is commonly a symlink into this
        // directory, and glibc's nss-resolve module connects to the
        // `io.systemd.Resolve` varlink socket here.
        //
        // Read-only on purpose. `PathBeneath` applies recursively, so a
        // write right here would cover the resolver's own state files
        // (`resolv.conf`, `stub-resolv.conf`) and let a sandboxed child
        // rewrite DNS configuration wherever DAC allowed it — far more than
        // reaching a socket. Connecting to the pathname AF_UNIX socket does
        // not need a write right on the supported ABI surface (the locked
        // `landlock` 0.4.5 tops out at ABI v7); this was verified by
        // connecting to `io.systemd.Resolve` from a sandboxed child under a
        // read-only rule, with `getaddrinfo` and HTTPS verification both
        // still succeeding.
        (
            "/run/systemd/resolve",
            AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
        // TLS trust store: OpenSSL/GnuTLS read the CA bundle from here to
        // verify certificates. Without a rule, any HTTPS request from a
        // sandboxed child fails with "unable to get local issuer
        // certificate" even though the socket itself connects fine.
        //
        // These are deliberately the certificate subpaths rather than
        // `/etc/ssl` as a whole: `PathBeneath` is recursive, and `/etc/ssl`
        // also contains `private/`, the conventional home for server private
        // keys. Landlock only ever restricts — it cannot grant access DAC
        // already denies — but there is no reason to hand the sandbox a rule
        // covering key material it never needs.
        //
        // Both the link and its target must be covered: Landlock authorizes
        // the *resolved* path, and on Arch-family systems the entries under
        // `/etc/ssl` are symlinks into `/etc/ca-certificates/extracted`
        // (`/etc/ssl/cert.pem` -> `../ca-certificates/extracted/tls-ca-bundle.pem`),
        // so a rule covering only the link would authorize nothing. Debian's
        // `/usr/share/ca-certificates` already falls under the `/usr` rule
        // above.
        //
        // The RHEL/Fedora layout gets the same subpath treatment for the same
        // reason: `/etc/pki` as a whole would recursively cover
        // `/etc/pki/tls/private`, where server private keys live alongside
        // the public trust material. Only the certificate and trust-anchor
        // subtrees are granted.
        //
        // ReadDir is required alongside ReadFile because OpenSSL's hashed
        // `capath` lookup (`/etc/ssl/certs`) enumerates the directory.
        (
            "/etc/ssl/certs",
            AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
        ("/etc/ssl/cert.pem", AccessFs::ReadFile.into(), false),
        // OpenSSL reads its config at library init; without a rule it falls
        // back to built-in defaults, which can change verification behaviour.
        ("/etc/ssl/openssl.cnf", AccessFs::ReadFile.into(), false),
        (
            "/etc/ca-certificates",
            AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
        // RHEL/Fedora: public certificates and extracted trust anchors only.
        // `/etc/pki/tls/private` is deliberately never granted.
        (
            "/etc/pki/tls/certs",
            AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
        ("/etc/pki/tls/cert.pem", AccessFs::ReadFile.into(), false),
        ("/etc/pki/tls/openssl.cnf", AccessFs::ReadFile.into(), false),
        (
            "/etc/pki/ca-trust",
            AccessFs::ReadFile | AccessFs::ReadDir,
            false,
        ),
    ]
}

/// Rights an overlapping rule leaks into a tier root, i.e. the rights the tier
/// deliberately withholds that the other rule grants anyway.
///
/// Landlock rules are **additive within a layer**: for a given file the kernel
/// takes the union of the rights of every rule whose subtree contains it. A
/// rule can therefore never subtract a right that a broader — or merely
/// overlapping — rule already granted. `/tmp` is granted
/// `ReadFile | WriteFile | Truncate` recursively for every child, so a
/// read-only root beneath `/tmp` would still be writable, and a write-only
/// root beneath it would still be readable, no matter what its own rule says.
///
/// **Only ever applied against [`generic_rules`].** Overlap between
/// policy-derived grants — the workspace and the three tiers — is not a
/// conflict but the composition `SecurityPolicy` itself performs, and the
/// kernel's union reproduces it exactly. Feeding those grants in here would
/// make two nested restrictive tiers cancel each other out and deny both
/// rights. See the call site in `build_ruleset`.
///
/// The test is symmetric on containment because both rules are `PathBeneath`
/// (recursive): whichever of the two is the ancestor, the union applies
/// somewhere inside the tier root.
///
/// Returns an empty set when the tier stays authoritative.
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
fn leaked_rights(
    root: &Path,
    root_perm: BitFlags<AccessFs>,
    grant: &Path,
    grant_perm: BitFlags<AccessFs>,
) -> BitFlags<AccessFs> {
    if !(root.starts_with(grant) || grant.starts_with(root)) {
        return BitFlags::empty();
    }
    (handled_access() & !root_perm) & grant_perm
}

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
impl LandlockSandbox {
    /// Create a new Landlock sandbox with the given workspace directory
    pub fn new() -> std::io::Result<Self> {
        Self::with_workspace(None)
    }

    /// Create a Landlock sandbox with a specific workspace directory
    pub fn with_workspace(workspace_dir: Option<std::path::PathBuf>) -> std::io::Result<Self> {
        Self::with_roots(workspace_dir, Vec::new(), Vec::new(), Vec::new())
    }

    /// Create a Landlock sandbox with a workspace directory plus the extra
    /// allowed-roots tiers from `SecurityPolicy`. Without this, paths that
    /// the application-layer policy permits outside the primary workspace
    /// (e.g. cross-agent shared directories) would still be rejected by the
    /// kernel, since Landlock would never have a rule for them.
    pub fn with_roots(
        workspace_dir: Option<std::path::PathBuf>,
        allowed_roots: Vec<std::path::PathBuf>,
        allowed_roots_read_only: Vec<std::path::PathBuf>,
        allowed_roots_write_only: Vec<std::path::PathBuf>,
    ) -> std::io::Result<Self> {
        let sandbox = Self {
            workspace_dir,
            allowed_roots,
            allowed_roots_read_only,
            allowed_roots_write_only,
        };

        // Validate by building the ruleset the child will actually receive,
        // rather than a minimal kernel probe. `landlock_available` and
        // `sandbox_posture` both reach this constructor, while enforcement
        // builds its ruleset later inside `wrap_command`. When the two disagree,
        // posture reports the backend as active and every subsequent spawn
        // fails — so availability has to be decided by the same ruleset
        // construction that execution depends on.
        match sandbox.build_ruleset() {
            Ok(_) => Ok(sandbox),
            Err(e) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Landlock not available"
                );
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!("Landlock not available: {e}"),
                ))
            }
        }
    }

    /// Probe if Landlock is available (for auto-detection)
    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    /// Build a Landlock ruleset with all configured access rules.
    ///
    /// The ruleset is **not** enforced here. Enforcement happens in the
    /// child process via `pre_exec` (see `wrap_command`), so only the
    /// child is restricted — the daemon (parent) process is never affected.
    fn build_ruleset(&self) -> std::io::Result<RulesetCreated> {
        let mut ruleset = Ruleset::default()
            .handle_access(handled_access())
            .and_then(|ruleset| ruleset.create())
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // Allow workspace directory (read/write/execute).
        // If a workspace was supplied but doesn't exist, fail closed rather than
        // silently applying restrictions without a rule for it.
        if let Some(ref workspace) = self.workspace_dir {
            let workspace_fd =
                PathFd::new(workspace).map_err(|e| std::io::Error::other(e.to_string()))?;
            ruleset = ruleset
                .add_rule(PathBeneath::new(workspace_fd, read_write_access()))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }

        // The extra `SecurityPolicy` root tiers (cross-agent grants,
        // `[autonomy].allowed_roots`, etc.), paired with the rights that give
        // each tier its meaning.
        let tiers: [(&'static str, &Vec<PathBuf>, BitFlags<AccessFs>); 3] = [
            ("allowed_roots", &self.allowed_roots, read_write_access()),
            (
                "allowed_roots_read_only",
                &self.allowed_roots_read_only,
                read_only_access(),
            ),
            (
                "allowed_roots_write_only",
                &self.allowed_roots_write_only,
                write_only_access(),
            ),
        ];

        // The rules the *policy layer* never authorized: the static allow-list
        // this backend installs so a child can find its loader, its libraries,
        // and a temp directory. `SecurityPolicy` knows nothing about them — it
        // lists `/tmp` among the broad `forbidden_paths` — so these are the only
        // rules that can grant a tier root rights the policy withholds.
        //
        // Resolved once here, and only when the path exists: `canonicalize`
        // fails for exactly the paths whose rules are skipped below, so a rule
        // that is never installed cannot manufacture a false conflict. Its
        // symlink resolution also means overlap is decided on the paths the
        // kernel sees rather than on however they happened to be spelled. A path
        // that exists but cannot be canonicalized (no search permission on a
        // parent) is simply left out of the comparison; its rule is still
        // installed, so this errs toward the pre-existing behaviour rather than
        // toward silent skipping.
        let generic_grants: Vec<(PathBuf, BitFlags<AccessFs>)> = generic_rules()
            .into_iter()
            .filter_map(|(path, perm, _)| {
                Path::new(path)
                    .canonicalize()
                    .ok()
                    .map(|resolved| (resolved, perm))
            })
            .collect();

        // Overlap *between* policy-derived grants is not a conflict: it is how
        // the policy composes. `SecurityPolicy` resolves a path by consulting
        // the workspace, then the read-write tier, then — for reads — the
        // read-only tier and — for writes — the write-only tier, taking the
        // first grant that matches. A write-only root nested inside a read-only
        // root is therefore readable *and* writable at the application layer
        // (`is_resolved_path_readable` matches the read-only parent;
        // `is_resolved_path_allowed` matches the write-only child), and the same
        // holds for the reverse nesting and for a path listed in both tiers.
        //
        // Landlock unions the rights of every rule covering a path, so simply
        // installing both rules reproduces that composition exactly. Treating
        // the two as undercutting each other would deny both rights and leave a
        // valid configuration with no kernel access at all.
        for (tier, roots, perm) in tiers {
            for root in roots {
                // A tier whose rights are undercut by a *generic* rule is not
                // enforceable: Landlock rules are additive within a layer, so a
                // rule can never subtract a right that an overlapping rule
                // already granted. Installing it anyway would advertise a
                // boundary the kernel does not implement — the read-only root
                // beneath `/tmp` would still be writable, the write-only root
                // still readable. Skip the rule and say so at WARN.
                //
                // Skipping is not a loss of protection — the path keeps exactly
                // the rights the generic rule already gave it, which is what it
                // had before any tier was propagated — but it is the difference
                // between a boundary that is absent and one that is falsely
                // advertised. The fix belongs in configuration: move the root
                // out from under the generic rule and the tier becomes
                // enforceable again.
                if let Ok(resolved) = root.canonicalize()
                    && let Some((grant_path, leaked)) =
                        generic_grants.iter().find_map(|(grant_path, grant_perm)| {
                            let leaked = leaked_rights(&resolved, perm, grant_path, *grant_perm);
                            (!leaked.is_empty()).then_some((grant_path, leaked))
                        })
                {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "root": root.display().to_string(),
                                "tier": tier,
                                "overlapping_rule": grant_path.display().to_string(),
                                "leaked_rights": format!("{leaked:?}"),
                            })),
                        "Skipping unenforceable allowed root in Landlock ruleset: \
                         a generic allow-list rule already grants rights this tier withholds"
                    );
                    continue;
                }

                // Unlike the workspace above, a root here that cannot be opened
                // is skipped rather than fatal — whatever the reason. These roots
                // are policy-generated, not operator-typed: `SecurityPolicy::for_agent`
                // unconditionally pushes `<install>/shared/skills` into the read-only
                // tier while fresh config initialization creates `<install>/shared`
                // without that child, and a cross-agent grant can name a sibling
                // workspace that its own agent has not materialized yet.
                //
                // Failing closed on any of them is not a safe default but a
                // security downgrade. The error propagates out of `build_ruleset`
                // to `with_roots`, and `create_selected_sandbox` discards that Err
                // (`security/detect.rs`), so the factory hands back `NoopSandbox`:
                // one malformed optional root — `/dev/null/not-a-child` returns
                // `ENOTDIR`, not `ENOENT` — would strip kernel enforcement from the
                // valid workspace and every other valid root along with it. Inside
                // `wrap_command` it is worse still: the first bad root stops every
                // sandboxed command from spawning at all.
                //
                // Skipping can only ever *remove* a grant, so it is always the
                // strictly-tighter direction, and the omission is recorded so it
                // stays diagnosable: DEBUG for an absent path, which is routine
                // during first-run initialization, WARN for anything else, which
                // means a root is configured that this host cannot open.
                //
                // The primary workspace stays fail-closed: without it there is no
                // boundary left to keep tighter.
                match PathFd::new(root) {
                    Ok(root_fd) => {
                        ruleset = ruleset
                            .add_rule(PathBeneath::new(root_fd, perm))
                            .map_err(|e| std::io::Error::other(e.to_string()))?;
                    }
                    Err(e) => {
                        let absent = matches!(
                            &e,
                            PathFdError::OpenCall { source, .. }
                                if source.kind() == std::io::ErrorKind::NotFound
                        );
                        if absent {
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({
                                    "root": root.display().to_string(),
                                    "tier": tier,
                                })),
                                "Skipping absent allowed root in Landlock ruleset"
                            );
                        } else {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({
                                    "root": root.display().to_string(),
                                    "tier": tier,
                                    "error": e.to_string(),
                                })),
                                "Skipping unopenable allowed root in Landlock ruleset: \
                                 the rest of the ruleset is still enforced"
                            );
                        }
                    }
                }
            }
        }

        // Allow paths for general operations.
        for (allow_path, perm, required) in generic_rules() {
            match PathFd::new(Path::new(allow_path)) {
                Ok(path_fd) => {
                    ruleset = ruleset
                        .add_rule(PathBeneath::new(path_fd, perm))
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                }
                Err(PathFdError::OpenCall { source, .. }) => {
                    if source.kind() == std::io::ErrorKind::NotFound {
                        if required {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                format!(
                                    "Required path {allow_path} not found for Landlock sandbox"
                                ),
                            ));
                        }
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            format!(
                                "Failed to create PathFd for a nonexistent path {}.",
                                allow_path,
                            ),
                        );
                    } else {
                        Err(std::io::Error::other(source.to_string()))?;
                    }
                }
                Err(e) => {
                    Err(std::io::Error::other(e.to_string()))?;
                }
            }
        }

        // Return the ruleset WITHOUT enforcing it.
        // Enforcement is deferred to the child process via pre_exec
        // (see wrap_command), which calls restrict_self() after fork()
        // but before exec(). This prevents the daemon from locking itself.
        Ok(ruleset)
    }
}

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
impl Sandbox for LandlockSandbox {
    fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()> {
        // Build the ruleset in the parent process where allocation is safe.
        // `RulesetCreated` is `Send + Sync + 'static`, which is necessary
        // for the value to be moved into the `pre_exec` closure (the closure
        // must be `Send`), but this bound alone does not make the closure
        // fork-safe — see the invariants below.
        let mut ruleset = Some(self.build_ruleset()?);

        // Enforce Landlock **only in the child process** via pre_exec,
        // which runs after fork() but before exec(). The daemon (parent)
        // is never restricted.
        //
        // SAFETY: `pre_exec` runs in a forked child after fork() but before
        // exec(). In a multi-threaded process, only async-signal-safe
        // operations are guaranteed correct in this window. The closure
        // must not allocate heap memory, acquire locks, or call
        // async-signal-unsafe functions on the success path.
        //
        // The closure performs three operations:
        //
        // 1. `ruleset.take()` — `Option::take()`. Moves the `RulesetCreated`
        //    out of the `Option`. Pure memory manipulation: no allocation,
        //    no syscall, no lock.
        //
        // 2. `rs.restrict_self()` — consumes the `RulesetCreated`. Internally
        //    issues `prctl(PR_SET_NO_NEW_PRIVS)` and `landlock_restrict_self()`,
        //    both raw syscalls, but also performs compatibility and status
        //    bookkeeping (e.g. checking Landlock ABI version, updating internal
        //    best-effort restriction state). These bookkeeping operations read
        //    and write stack-local or already-allocated fields; they do not
        //    allocate heap memory or acquire locks on the success path.
        //    On return, `rs` is dropped, which closes the ruleset file
        //    descriptor via another raw syscall.
        //
        //    Errors are translated to `io::Error::from_raw_os_error()` via
        //    `landlock::Errno`, which extracts the raw errno from the
        //    `RulesetError`'s source chain. `from_raw_os_error` stores the
        //    error as `Repr::Os(i32)` — no heap allocation, no formatting.
        //    `Errno::from` walks `error.source()` (a reference) and calls
        //    `raw_os_error()` (reads an `i32`); dropping the consumed error
        //    frees no heap since the underlying `io::Error` is also
        //    `Repr::Os(i32)`. The parent receives a proper `Err` from
        //    `spawn()`. `std` installs `always_abort()` before invoking
        //    `pre_exec` as a safety net, but the closure does not rely on it
        //    for normal operation.
        //
        // 3. Same-child defensive guard — `ruleset.take()` returns `None` only
        //    if `pre_exec` were invoked twice within the *same* forked child.
        //    Repeated `Command::spawn()` calls fork distinct children, each
        //    receiving its own copy of the `Option` (fork copies the parent's
        //    memory), so the parent's captured `Some` is never consumed.
        //    Because `pre_exec` runs at most once per fork, this branch is
        //    unreachable; it returns `EINVAL` via `from_raw_os_error()` as a
        //    defensive guard. No allocation, no panic.
        //
        // Re-audit obligation: any version bump of the `landlock` crate
        // requires re-verifying that `RulesetCreated::restrict_self()` and
        // `Drop for RulesetCreated` remain fork-safe — no heap allocation,
        // no lock acquisition, no async-signal-unsafe calls between fork()
        // and exec().
        //
        // SAFETY: the closure obeys `pre_exec`'s post-fork restrictions for
        // the reasons above: its captured state is child-local, the audited
        // landlock operations are fork-safe, and every error path is
        // allocation-free.
        unsafe {
            cmd.pre_exec(move || {
                if let Some(rs) = ruleset.take() {
                    rs.restrict_self()
                        .map_err(|e| std::io::Error::from_raw_os_error(*Errno::from(e)))?;
                } else {
                    // Unreachable: `pre_exec` is called exactly once per
                    // fork, and each forked child receives its own copy of
                    // `ruleset` (always `Some` on first entry). Kept as a
                    // defensive guard against same-child double-invocation.
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                }
                Ok(())
            });
        }

        Ok(())
    }

    fn is_available(&self) -> bool {
        // Try to create a minimal ruleset to verify availability
        Ruleset::default()
            .handle_access(AccessFs::ReadFile)
            .and_then(|ruleset| ruleset.create())
            .is_ok()
    }

    fn name(&self) -> &str {
        "landlock"
    }

    fn description(&self) -> &str {
        "Linux kernel LSM sandboxing (filesystem access control)"
    }
}

// Stub implementations for non-Linux or when feature is disabled
#[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
#[derive(Debug)]
pub struct LandlockSandbox;

#[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
impl LandlockSandbox {
    pub fn new() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux with the sandbox-landlock feature",
        ))
    }

    pub fn with_workspace(_workspace_dir: Option<std::path::PathBuf>) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux",
        ))
    }

    pub fn with_roots(
        _workspace_dir: Option<std::path::PathBuf>,
        _allowed_roots: Vec<std::path::PathBuf>,
        _allowed_roots_read_only: Vec<std::path::PathBuf>,
        _allowed_roots_write_only: Vec<std::path::PathBuf>,
    ) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux",
        ))
    }

    pub fn probe() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux",
        ))
    }
}

#[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
impl Sandbox for LandlockSandbox {
    fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux",
        ))
    }

    fn is_available(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        "landlock"
    }

    fn description(&self) -> &str {
        "Linux kernel LSM sandboxing (not available on this platform)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlap predicate that decides whether a tier stays authoritative.
    ///
    /// Landlock unions the rights of every rule covering a path, so a tier root
    /// beneath a broader rule silently inherits whatever that rule grants. These
    /// cases pin the three outcomes that matter: a tier undercut by `/tmp`, a
    /// tier that `/tmp` cannot undercut, and a tier far enough away to be
    /// unaffected.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn leaked_rights_reports_tiers_undercut_by_a_broader_rule() {
        let tmp = Path::new("/tmp");
        let tmp_perm = AccessFs::Truncate | AccessFs::WriteFile | AccessFs::ReadFile;

        // A read-only root beneath /tmp keeps /tmp's write rights.
        let leaked = leaked_rights(
            Path::new("/tmp/read-only-root"),
            read_only_access(),
            tmp,
            tmp_perm,
        );
        assert_eq!(
            leaked,
            AccessFs::WriteFile | AccessFs::Truncate,
            "a read-only root beneath /tmp must be reported as writable"
        );

        // A write-only root beneath /tmp keeps /tmp's read right.
        let leaked = leaked_rights(
            Path::new("/tmp/write-only-root"),
            write_only_access(),
            tmp,
            tmp_perm,
        );
        assert_eq!(
            leaked,
            BitFlags::from(AccessFs::ReadFile),
            "a write-only root beneath /tmp must be reported as readable"
        );

        // The read-write tier withholds only device-node creation, which no
        // generic rule grants, so it is never undercut.
        assert!(
            leaked_rights(
                Path::new("/tmp/read-write-root"),
                read_write_access(),
                tmp,
                tmp_perm,
            )
            .is_empty(),
            "a read-write root beneath /tmp must stay enforceable"
        );

        // No containment in either direction: /tmp cannot reach it.
        assert!(
            leaked_rights(
                Path::new("/var/tmp/read-only-root"),
                read_only_access(),
                tmp,
                tmp_perm,
            )
            .is_empty(),
            "a root outside the generic allow-list must stay enforceable"
        );

        // Containment the other way round still unions inside the tier root.
        assert!(
            !leaked_rights(Path::new("/"), read_only_access(), tmp, tmp_perm).is_empty(),
            "a tier root that *contains* a broader rule is undercut inside it"
        );

        // A sibling prefix is not a path prefix: /tmpfoo is not beneath /tmp.
        assert!(
            leaked_rights(
                Path::new("/tmpfoo/read-only-root"),
                read_only_access(),
                tmp,
                tmp_perm,
            )
            .is_empty(),
            "overlap must compare path components, not string prefixes"
        );
    }

    /// Every generic rule must be reachable by the overlap check: a rule that
    /// grants rights the check never sees would reintroduce the bypass.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn generic_rules_grant_only_handled_access() {
        for (path, perm, _) in generic_rules() {
            assert!(
                (perm & !handled_access()).is_empty(),
                "generic rule {path} grants a right the ruleset does not handle, \
                 so no tier could ever withhold it"
            );
        }
    }

    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn landlock_sandbox_name() {
        if let Ok(sandbox) = LandlockSandbox::new() {
            assert_eq!(sandbox.name(), "landlock");
        }
    }

    #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
    #[test]
    fn landlock_not_available_on_non_linux() {
        assert!(!LandlockSandbox.is_available());
        assert_eq!(LandlockSandbox.name(), "landlock");
    }

    #[test]
    fn landlock_with_none_workspace() {
        // Should work even without a workspace directory
        let result = LandlockSandbox::with_workspace(None);
        // On Linux with sandbox-landlock feature, this must succeed.
        // On other platforms or without the feature, failure is acceptable.
        if cfg!(all(feature = "sandbox-landlock", target_os = "linux")) {
            let sandbox = result.expect("landlock should succeed on linux with feature enabled");
            assert!(sandbox.is_available());
        }
    }

    // ── Parent-process protection ──
    //
    // `restrict_self()` must run in the forked child via `pre_exec`,
    // never in the parent.  These tests verify the daemon (parent)
    // process is never restricted.

    /// Regression: `wrap_command` must NOT restrict the parent process.
    ///
    /// Before the fix, `restrict_self()` was called directly inside
    /// `wrap_command`, which locked the daemon itself within the Landlock
    /// ruleset. Now enforcement is deferred to the child via `pre_exec`.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_does_not_restrict_parent_process() {
        let sandbox = match LandlockSandbox::new() {
            Ok(s) => s,
            Err(_) => return, // Landlock not available — skip
        };

        // /etc/passwd is world-readable on every Linux but NOT in the
        // Landlock allow-list (/tmp, /usr, /bin).  After wrap_command
        // the parent must still be able to read it.
        let sentinel = Path::new("/etc/passwd");

        // The sentinel must exist and be readable before the test starts.
        // If it doesn't, the test environment is broken — fail loudly
        // rather than silently passing without verifying anything.
        assert!(
            sentinel.exists(),
            "/etc/passwd must exist as a sentinel — test environment is broken"
        );
        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "/etc/passwd must be readable before sandboxing — test environment is broken"
        );

        let mut cmd = std::process::Command::new("true");
        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command must succeed");

        cmd.spawn()
            .expect("child spawn must succeed")
            .wait()
            .expect("child wait must succeed");

        // THE CORE ASSERTION: after wrap_command the parent must STILL
        // be able to read /etc/passwd.  If this fails, restrict_self()
        // was called in the parent — which is the bug this commit fixes.
        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "parent process must NOT be restricted by wrap_command — \
             restrict_self() must only run inside the forked child via pre_exec"
        );
    }

    /// `build_ruleset` must NOT enforce restrictions on the caller.
    /// It returns a `RulesetCreated` without calling `restrict_self()`.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn build_ruleset_does_not_restrict_parent() {
        let sandbox = match LandlockSandbox::new() {
            Ok(s) => s,
            Err(_) => return,
        };

        let sentinel = Path::new("/etc/passwd");

        // The sentinel must exist and be readable before the test starts.
        assert!(
            sentinel.exists(),
            "/etc/passwd must exist as a sentinel — test environment is broken"
        );
        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "/etc/passwd must be readable before build_ruleset — test environment is broken"
        );

        // build_ruleset is safe to call — it only constructs the ruleset,
        // it does NOT enforce it.
        let _ruleset = sandbox.build_ruleset().expect("build_ruleset must succeed");

        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "build_ruleset must not restrict the parent process"
        );
    }

    /// `wrap_command` must return `Ok(())` on a valid command.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_returns_ok() {
        let sandbox = match LandlockSandbox::new() {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut cmd = std::process::Command::new("true");
        assert!(sandbox.wrap_command(&mut cmd).is_ok());
    }

    /// `wrap_command` must NOT replace the program binary (unlike
    /// bubblewrap/firejail which prepend their own wrapper).  Landlock
    /// uses `pre_exec` only, so the program and args stay unchanged.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_preserves_program_and_args() {
        let sandbox = match LandlockSandbox::new() {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hello");
        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command must succeed");

        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "echo",
            "landlock must not replace the program — it uses pre_exec, not a wrapper binary"
        );

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args,
            vec!["hello".to_string()],
            "landlock must not modify command arguments"
        );
    }

    /// Calling `wrap_command` on multiple distinct commands must not
    /// panic or fail.  Each call builds a fresh ruleset and a separate
    /// `pre_exec` closure, so wrapping multiple commands is safe.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_multiple_distinct_commands() {
        let sandbox = LandlockSandbox::new().expect("Failed to create landlock sandbox");

        for i in 0..3 {
            let mut cmd = std::process::Command::new("true");
            sandbox
                .wrap_command(&mut cmd)
                .unwrap_or_else(|e| panic!("wrap_command call #{i} failed: {e}"));
        }
    }

    /// When a workspace directory is set, `wrap_command` must still
    /// not lock the parent process.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_with_workspace_does_not_restrict_parent() {
        let tmp = tempfile::TempDir::new().expect("must create temp dir");

        let sandbox = LandlockSandbox::with_workspace(Some(tmp.path().to_path_buf()))
            .expect("Failed to create landlock sandbox");

        let sentinel = Path::new("/etc/passwd");

        // The sentinel must exist and be readable before the test starts.
        assert!(
            sentinel.exists(),
            "/etc/passwd must exist as a sentinel — test environment is broken"
        );
        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "/etc/passwd must be readable before wrap_command — test environment is broken"
        );

        let mut cmd = std::process::Command::new("true");
        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command must succeed");

        cmd.spawn()
            .expect("child spawn must succeed")
            .wait()
            .expect("child wait must succeed");

        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "parent must not be restricted even with workspace configured"
        );
    }

    // ── §1.1 Landlock stub tests ──────────────────────────────

    #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
    #[test]
    fn landlock_stub_wrap_command_returns_unsupported() {
        let sandbox = LandlockSandbox;
        let mut cmd = std::process::Command::new("echo");
        let result = sandbox.wrap_command(&mut cmd);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
    #[test]
    fn landlock_stub_new_returns_unsupported() {
        let result = LandlockSandbox::new();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
    #[test]
    fn landlock_stub_probe_returns_unsupported() {
        let result = LandlockSandbox::probe();
        assert!(result.is_err());
    }
}
