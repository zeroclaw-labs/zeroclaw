# ZeroClaw Sandboxing Strategies

## Problem
ZeroClaw currently has application-layer security (allowlists, path blocking, command injection protection) but lacks OS-level containment. If an attacker is on the allowlist, they can run any allowed command with zeroclaw's user permissions.

## Proposed Solutions

### Option 1: Firejail Integration (Recommended for Linux)
Firejail provides user-space sandboxing with minimal overhead.

```rust
// src/security/firejail.rs
use std::process::Command;

pub struct FirejailSandbox {
    enabled: bool,
}

impl FirejailSandbox {
    pub fn new() -> Self {
        let enabled = which::which("firejail").is_ok();
        Self { enabled }
    }

    pub fn wrap_command(&self, cmd: &mut Command) -> &mut Command {
        if !self.enabled {
            return cmd;
        }

        // Firejail wraps any command with sandboxing
        let mut jail = Command::new("firejail");
        jail.args([
            "--private=home",           // New home directory
            "--private-dev",            // Minimal /dev
            "--nosound",                // No audio
            "--no3d",                   // No 3D acceleration
            "--novideo",                // No video devices
            "--nowheel",                // No input devices
            "--notv",                   // No TV devices
            "--noprofile",              // Skip profile loading
            "--quiet",                  // Suppress warnings
        ]);

        // Append original command
        if let Some(program) = cmd.get_program().to_str() {
            jail.arg(program);
        }
        for arg in cmd.get_args() {
            if let Some(s) = arg.to_str() {
                jail.arg(s);
            }
        }

        // Replace original command with firejail wrapper
        *cmd = jail;
        cmd
    }
}
```

**Config option:**
```toml
[security]
enable_sandbox = true
sandbox_backend = "firejail"  # or "none", "bubblewrap", "docker"
```

---

### Option 2: Bubblewrap (Portable, no root required)
Bubblewrap uses user namespaces to create containers.

```bash
# Install bubblewrap
sudo apt install bubblewrap

# Wrap command:
bwrap --ro-bind /usr /usr \
      --dev /dev \
      --proc /proc \
      --bind /workspace /workspace \
      --unshare-all \
      --share-net \
      --die-with-parent \
      -- /bin/sh -c "command"
```

---

### Option 3: Docker-in-Docker (Heavyweight but complete isolation)
Run agent tools inside ephemeral containers.

```rust
pub struct DockerSandbox {
    image: String,
}

impl DockerSandbox {
    pub async fn execute(&self, command: &str, workspace: &Path) -> Result<String> {
        let output = Command::new("docker")
            .args([
                "run", "--rm",
                "--memory", "512m",
                "--cpus", "1.0",
                "--network", "none",
                "--volume", &format!("{}:/workspace", workspace.display()),
                &self.image,
                "sh", "-c", command
            ])
            .output()
            .await?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}
```

---

### Option 4: Landlock (Linux Kernel LSM, Rust native)
Landlock provides file system access control without containers.

```rust
use landlock::{Ruleset, AccessFS};

pub fn apply_landlock() -> Result<()> {
    let ruleset = Ruleset::new()
        .set_access_fs(AccessFS::read_file | AccessFS::write_file)
        .add_path(Path::new("/workspace"), AccessFS::read_file | AccessFS::write_file)?
        .add_path(Path::new("/tmp"), AccessFS::read_file | AccessFS::write_file)?
        .restrict_self()?;

    Ok(())
}
```

---

## Priority Implementation Order

| Phase | Solution | Effort | Security Gain |
|-------|----------|--------|---------------|
| **P0** | Landlock (Linux only, native) | Low | High (filesystem) |
| **P1** | Firejail integration | Low | Very High |
| **P2** | Bubblewrap wrapper | Medium | Very High |
| **P3** | Docker sandbox mode | High | Complete |

## Config Schema Extension

```toml
[security.sandbox]
enabled = true
backend = "auto"  # auto | firejail | bubblewrap | landlock | docker | none

# Firejail-specific
[security.sandbox.firejail]
extra_args = ["--seccomp", "--caps.drop=all"]

# Landlock-specific
[security.sandbox.landlock]
readonly_paths = ["/usr", "/bin", "/lib"]
readwrite_paths = ["$HOME/workspace", "/tmp/zeroclaw"]
```

## Testing Strategy

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn sandbox_blocks_path_traversal() {
        // Try to read /etc/passwd through sandbox
        let result = sandboxed_execute("cat /etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn sandbox_allows_workspace_access() {
        let result = sandboxed_execute("ls /workspace");
        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_no_network_isolation() {
        // Ensure network is blocked when configured
        let result = sandboxed_execute("curl http://example.com");
        assert!(result.is_err());
    }
}
```
