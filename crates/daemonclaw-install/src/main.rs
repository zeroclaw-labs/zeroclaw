use clap::Parser;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "daemonclaw-install", about = "Provision a system to run DaemonClaw")]
struct Cli {
    /// Service account username
    #[arg(long, default_value = "daemonclaw")]
    user: String,

    /// Base directory for DaemonClaw state
    #[arg(long, default_value = "/var/lib/daemonclaw")]
    home: PathBuf,

    /// Path to the daemonclaw binary (copied to this location)
    #[arg(long, default_value = "/usr/local/bin/daemonclaw")]
    bin_path: PathBuf,

    /// Gateway listen address
    #[arg(long, default_value = "127.0.0.1")]
    listen_host: String,

    /// Gateway listen port
    #[arg(long, default_value_t = 42617)]
    listen_port: u16,

    /// Default LLM provider
    #[arg(long, default_value = "zai")]
    provider: String,

    /// Default model name
    #[arg(long, default_value = "glm-5-turbo")]
    model: String,

    /// Print what would be done without making changes
    #[arg(long)]
    dry_run: bool,
}

// ── Change log ───────────────────────────────────────────────────────

struct ChangeLog {
    entries: Vec<String>,
    dry_run: bool,
}

impl ChangeLog {
    fn new(dry_run: bool) -> Self {
        Self {
            entries: Vec::new(),
            dry_run,
        }
    }

    fn record(&mut self, category: &str, detail: String) {
        let prefix = if self.dry_run { "[dry-run] " } else { "" };
        let entry = format!("{prefix}[{category}] {detail}");
        eprintln!("  {entry}");
        self.entries.push(entry);
    }

    fn print_summary(&self) {
        eprintln!();
        eprintln!("═══ Installation summary ({} changes) ═══", self.entries.len());
        for entry in &self.entries {
            eprintln!("  {entry}");
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run {cmd}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{cmd} failed: {stderr}"))
    }
}

fn user_exists(name: &str) -> bool {
    Command::new("id")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn group_exists(name: &str) -> bool {
    Command::new("getent")
        .args(["group", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn set_ownership(path: &Path, user: &str, group: &str) -> Result<(), String> {
    run("chown", &[&format!("{user}:{group}"), &path.to_string_lossy()])?;
    Ok(())
}

// ── Groups ───────────────────────────────────────────────────────────
// ACCESS_MATRIX.md § Groups: two groups, one ACL.

const AGENTS_GROUP: &str = "agents";

fn admin_group(user: &str) -> String {
    format!("{user}-admin")
}

fn create_groups(user: &str, log: &mut ChangeLog) -> Result<(), String> {
    let groups = [
        (AGENTS_GROUP.to_string(), "system-wide read tier for agent service accounts"),
        (admin_group(user), "config write escalation (one ACL on config.toml)"),
    ];
    for (name, desc) in &groups {
        if group_exists(name) {
            log.record("group", format!("{name} already exists ({desc})"));
        } else if !log.dry_run {
            run("groupadd", &["--system", name])?;
            log.record("group", format!("created {name} ({desc})"));
        } else {
            log.record("group", format!("would create {name} ({desc})"));
        }
    }
    Ok(())
}

// ── User ─────────────────────────────────────────────────────────────
// ACCESS_MATRIX.md § Group membership: primary group = agents

fn create_user(user: &str, home: &Path, log: &mut ChangeLog) -> Result<(), String> {
    if user_exists(user) {
        log.record("user", format!("{user} already exists"));
        return Ok(());
    }
    if log.dry_run {
        log.record("user", format!("would create system user {user} (gid={AGENTS_GROUP}) with home {}", home.display()));
        return Ok(());
    }
    run(
        "useradd",
        &[
            "--system",
            "--gid", AGENTS_GROUP,
            "--home-dir", &home.to_string_lossy(),
            "--create-home",
            "--shell", "/usr/sbin/nologin",
            user,
        ],
    )?;
    log.record("user", format!("created system user {user} (gid={AGENTS_GROUP}) with home {}", home.display()));
    Ok(())
}

// ── Directories ──────────────────────────────────────────────────────
// ACCESS_MATRIX.md § Workspace directories, State and logs, Scratch space
// All dirs owned daemonclaw:agents. Mode is the access gate.

struct DirDef {
    relative: &'static str,
    mode: u32,
    description: &'static str,
}

const DIRS: &[DirDef] = &[
    DirDef { relative: ".daemonclaw",                    mode: 0o750, description: "main state directory" },
    DirDef { relative: ".daemonclaw/workspace",          mode: 0o750, description: "agent workspace root" },
    DirDef { relative: ".daemonclaw/workspace/github",   mode: 0o750, description: "github project clones" },
    DirDef { relative: ".daemonclaw/workspace/memory",   mode: 0o700, description: "memory store (private)" },
    DirDef { relative: ".daemonclaw/workspace/sessions", mode: 0o700, description: "session persistence" },
    DirDef { relative: ".daemonclaw/workspace/skills",   mode: 0o750, description: "installed skills" },
    DirDef { relative: ".daemonclaw/state",              mode: 0o750, description: "runtime state and traces" },
    DirDef { relative: ".daemonclaw/state/backups",      mode: 0o750, description: "agent-side backups" },
    DirDef { relative: ".daemonclaw/logs",               mode: 0o750, description: "log directory" },
    DirDef { relative: "tmp",                            mode: 0o700, description: "private scratch (not /tmp)" },
];

fn create_directories(user: &str, home: &Path, log: &mut ChangeLog) -> Result<(), String> {
    for d in DIRS {
        let full = home.join(d.relative);
        if full.exists() {
            log.record("dir", format!("{} already exists ({})", full.display(), d.description));
        } else if !log.dry_run {
            fs::create_dir_all(&full)
                .map_err(|e| format!("mkdir {}: {e}", full.display()))?;
            fs::set_permissions(&full, fs::Permissions::from_mode(d.mode))
                .map_err(|e| format!("chmod {}: {e}", full.display()))?;
            set_ownership(&full, user, AGENTS_GROUP)?;
            log.record(
                "dir",
                format!("{} (mode {:04o}, owner {user}:{AGENTS_GROUP}, {})", full.display(), d.mode, d.description),
            );
        } else {
            log.record(
                "dir",
                format!("would create {} (mode {:04o}, owner {user}:{AGENTS_GROUP}, {})", full.display(), d.mode, d.description),
            );
        }
    }
    Ok(())
}

// ── Default ACLs ─────────────────────────────────────────────────────
// ACCESS_MATRIX.md § Default ACLs: guarantee agents can read runtime-created files

fn set_default_acls(home: &Path, log: &mut ChangeLog) -> Result<(), String> {
    let acl_dirs = [
        (".daemonclaw/logs", "new log files readable by agents"),
        (".daemonclaw/state", "new state files readable by agents"),
    ];
    for (rel, desc) in &acl_dirs {
        let full = home.join(rel);
        let acl = format!("default:group:{AGENTS_GROUP}:r--");
        if log.dry_run {
            log.record("acl", format!("would set {acl} on {} ({desc})", full.display()));
        } else {
            run("setfacl", &["-m", &acl, &full.to_string_lossy()])?;
            log.record("acl", format!("set {acl} on {} ({desc})", full.display()));
        }
    }
    Ok(())
}

// ── Config ───────────────────────────────────────────────────────────
// ACCESS_MATRIX.md § Config:
//   /etc/daemonclaw/           root:agents  0750
//   /etc/daemonclaw/config.toml  root:agents  0640  ACL group:daemonclaw-admin:rw-
//   $HOME/.daemonclaw/config.toml  → symlink to /etc/daemonclaw/config.toml

fn install_config(cli: &Cli, log: &mut ChangeLog) -> Result<(), String> {
    let etc_dir = PathBuf::from("/etc/daemonclaw");
    let etc_config = etc_dir.join("config.toml");
    let home_symlink = cli.home.join(".daemonclaw/config.toml");
    let admin_grp = admin_group(&cli.user);

    // /etc/daemonclaw/ directory
    if etc_dir.exists() {
        log.record("config", format!("{} already exists", etc_dir.display()));
    } else if log.dry_run {
        log.record("config", format!("would create {} (root:{AGENTS_GROUP} 0750)", etc_dir.display()));
    } else {
        fs::create_dir_all(&etc_dir)
            .map_err(|e| format!("mkdir {}: {e}", etc_dir.display()))?;
        fs::set_permissions(&etc_dir, fs::Permissions::from_mode(0o750))
            .map_err(|e| format!("chmod {}: {e}", etc_dir.display()))?;
        set_ownership(&etc_dir, "root", AGENTS_GROUP)?;
        log.record("config", format!("created {} (root:{AGENTS_GROUP} 0750)", etc_dir.display()));
    }

    // /etc/daemonclaw/config.toml
    if etc_config.exists() {
        log.record("config", format!("{} already exists — not overwriting", etc_config.display()));
    } else if log.dry_run {
        log.record("config", format!(
            "would write {} (root:{AGENTS_GROUP} 0640, ACL group:{admin_grp}:rw-)",
            etc_config.display()
        ));
    } else {
        let content = generate_config(cli);
        fs::write(&etc_config, &content)
            .map_err(|e| format!("write {}: {e}", etc_config.display()))?;
        fs::set_permissions(&etc_config, fs::Permissions::from_mode(0o640))
            .map_err(|e| format!("chmod {}: {e}", etc_config.display()))?;
        set_ownership(&etc_config, "root", AGENTS_GROUP)?;
        run("setfacl", &["-m", &format!("group:{admin_grp}:rw-"), &etc_config.to_string_lossy()])?;
        log.record("config", format!(
            "wrote {} (root:{AGENTS_GROUP} 0640, ACL group:{admin_grp}:rw-)",
            etc_config.display()
        ));
    }

    // Symlink $HOME/.daemonclaw/config.toml → /etc/daemonclaw/config.toml
    if home_symlink.exists() || home_symlink.is_symlink() {
        log.record("config", format!("{} already exists", home_symlink.display()));
    } else if log.dry_run {
        log.record("config", format!(
            "would symlink {} → {}",
            home_symlink.display(), etc_config.display()
        ));
    } else {
        std::os::unix::fs::symlink(&etc_config, &home_symlink)
            .map_err(|e| format!("symlink {}: {e}", home_symlink.display()))?;
        set_ownership(&home_symlink, &cli.user, AGENTS_GROUP)?;
        log.record("config", format!(
            "symlinked {} → {}",
            home_symlink.display(), etc_config.display()
        ));
    }

    Ok(())
}

// ── Secret key ───────────────────────────────────────────────────────
// ACCESS_MATRIX.md § Secret key: daemonclaw:agents 0600, chattr +i after creation

fn install_secret_key(cli: &Cli, log: &mut ChangeLog) -> Result<(), String> {
    let key_path = cli.home.join(".daemonclaw/.secret_key");

    if key_path.exists() {
        log.record("secret", format!("{} already exists", key_path.display()));
        return Ok(());
    }

    if log.dry_run {
        log.record("secret", format!(
            "would generate {} ({}:{AGENTS_GROUP} 0600, chattr +i)",
            key_path.display(), cli.user
        ));
        return Ok(());
    }

    // Generate 64 bytes of random key material
    let key_hex = run("openssl", &["rand", "-hex", "32"])?;
    fs::write(&key_path, key_hex.trim())
        .map_err(|e| format!("write {}: {e}", key_path.display()))?;
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod {}: {e}", key_path.display()))?;
    set_ownership(&key_path, &cli.user, AGENTS_GROUP)?;
    run("chattr", &["+i", &key_path.to_string_lossy()])?;
    log.record("secret", format!(
        "generated {} ({}:{AGENTS_GROUP} 0600, chattr +i)",
        key_path.display(), cli.user
    ));

    Ok(())
}

// ── Config content ───────────────────────────────────────────────────

fn generate_config(cli: &Cli) -> String {
    format!(
        r#"# DaemonClaw configuration — generated by daemonclaw-install
# See https://github.com/DeliveryBoyTech/daemonclaw for documentation.

default_provider = "{provider}"
default_model = "{model}"
default_temperature = 0.7
provider_timeout_secs = 120
model_routes = []
embedding_routes = []

[model_providers]

[extra_headers]

[observability]
backend = "none"
runtime_trace_mode = "full"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 1000

[autonomy]
level = "supervised"
workspace_only = true
allowed_commands = [
    "git", "ls", "cat", "grep", "find", "echo", "pwd", "cd",
    "wc", "head", "tail", "date",
    "mkdir", "cp", "mv", "rm", "touch", "ln", "readlink", "realpath", "chmod",
    "sed", "awk", "diff", "patch", "sort", "uniq", "tr", "cut", "xargs",
    "tee", "less", "file", "stat", "basename", "dirname",
    "tar", "gzip", "gunzip", "zip", "unzip",
    "curl", "wget",
    "sh", "bash",
    "python", "python3", "pip", "pip3",
    "node", "npm", "npx", "yarn", "pnpm",
    "rustc", "rustup", "cargo",
    "go", "deno", "bun",
    "make", "cmake", "gcc", "cc", "g++", "ld", "pkg-config",
    "apt", "apt-get", "dpkg", "dpkg-query", "snap",
    "gh", "docker", "docker-compose", "podman",
    "lsb_release", "systemctl", "journalctl",
    "test", "true", "false", "which", "type", "env",
    "uname", "uptime", "free", "df", "du", "ps", "id", "whoami",
    "hostname", "nproc", "lscpu", "lsmem", "lsblk", "lsof",
    "ss", "netstat", "ip", "ping", "traceroute", "nslookup", "dig",
    "sudo",
]
forbidden_paths = [
    "/etc/shadow", "/etc/gshadow",
    "/etc/sudoers", "/etc/sudoers.d",
    "/etc/pam.d", "/etc/polkit-1",
    "/etc/ssh",
    "~/.ssh", "~/.gnupg", "~/.aws",
    "/root", "/home",
    "~/.daemonclaw/.secret_key",
    "~/.daemonclaw/workspace/memory",
    "~/.daemonclaw/workspace/sessions",
    "~/.daemonclaw/workspace/devices.db",
]
max_actions_per_hour = 240
max_cost_per_day_cents = 2500
require_approval_for_medium_risk = true
block_high_risk_commands = true
shell_env_passthrough = ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM", "CARGO_HOME", "RUSTUP_HOME"]
auto_approve = [
    "file_read", "file_write", "file_edit",
    "memory_recall",
    "web_search_tool", "web_fetch",
    "calculator",
    "glob_search", "content_search",
    "image_info", "weather",
    "shell",
]
always_ask = []
allowed_roots = [
    "~/.cargo",
    "~/.rustup",
    "~/.npm",
    "~/.local",
    "/tmp",
    "/proc", "/sys",
    "/etc/os-release",
    "/etc/hostname",
    "~/.gitconfig",
    "~/.config/gh",
]
non_cli_excluded_tools = []

[security.sandbox]
backend = "none"
firejail_args = []

[security.resources]
max_memory_mb = 8192
max_cpu_time_seconds = 1800
max_subprocesses = 64
memory_monitoring = true

[security.audit]
enabled = true
log_path = "audit.log"
max_size_mb = 100
sign_events = false

[security.otp]
enabled = false

[security.estop]
enabled = false

[backup]
enabled = true
max_keep = 10
include_dirs = ["config", "memory", "audit", "knowledge", "skills"]
destination_dir = "state/backups"
compress = true
encrypt = false

[data_retention]
enabled = false
retention_days = 90

[runtime]
kind = "native"

[reliability]
provider_retries = 2
provider_backoff_ms = 500
fallback_providers = []

[scheduler]
enabled = true
max_tasks = 64
max_concurrent = 4

[agent]
compact_context = true
max_tool_iterations = 100
max_history_messages = 150
max_context_tokens = 200000
parallel_tools = false
tool_dispatcher = "auto"

[agent.context_compression]
enabled = false

[agent.thinking]
default_level = "medium"

[skills]
open_skills_enabled = true
allow_scripts = true

[channels.telegram]
enabled = true
"#,
        provider = cli.provider,
        model = cli.model,
    )
}

// ── Systemd unit ─────────────────────────────────────────────────────
// ACCESS_MATRIX.md § systemd hardening

fn generate_service_unit(cli: &Cli) -> String {
    format!(
        r#"[Unit]
Description=DaemonClaw AI Agent
Documentation=https://github.com/DeliveryBoyTech/daemonclaw
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
Group={agents}
WorkingDirectory={home}

ExecStart={bin} gateway --host {host} --port {port}

Restart=on-failure
RestartSec=5
WatchdogSec=120

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
LockPersonality=true
MemoryDenyWriteExecute=true
SystemCallArchitectures=native

# Network: outbound only (Telegram polling)
SocketBindDeny=any

# Allow agent access to its own state + read config
ReadWritePaths={home}
ReadOnlyPaths=/proc /sys /etc/os-release /etc/hostname /etc/resolv.conf /etc/daemonclaw

# Resource limits
MemoryMax=2G
CPUQuota=200%
TasksMax=64
LimitFSIZE=1073741824

# Environment
Environment=RUST_LOG=info
Environment=HOME={home}

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=daemonclaw

[Install]
WantedBy=multi-user.target
"#,
        user = cli.user,
        agents = AGENTS_GROUP,
        home = cli.home.display(),
        bin = cli.bin_path.display(),
        host = cli.listen_host,
        port = cli.listen_port,
    )
}

// ── Systemd slice ────────────────────────────────────────────────────

fn generate_slice() -> String {
    r#"[Unit]
Description=DaemonClaw agent group resource slice

[Slice]
MemoryMax=12G
TasksMax=512
CPUQuota=600%
"#.to_string()
}

// ── Backup infrastructure ────────────────────────────────────────────
// ACCESS_MATRIX.md § Data durability → State → external backups

fn generate_backup_timer() -> &'static str {
    r#"[Unit]
Description=DaemonClaw state backup

[Timer]
OnCalendar=hourly
Persistent=true

[Install]
WantedBy=timers.target
"#
}

fn generate_backup_service(home: &Path) -> String {
    format!(
        r#"[Unit]
Description=DaemonClaw state backup

[Service]
Type=oneshot
ExecStart=/bin/bash -c '\
    ts=$(date +%%Y%%m%%d-%%H%%M%%S) && \
    tar czf /var/backups/daemonclaw/state-${{ts}}.tar.gz \
        -C {home}/.daemonclaw state/ \
    '
User=root
Group={agents}
"#,
        home = home.display(),
        agents = AGENTS_GROUP,
    )
}

fn generate_tmpfiles_conf() -> &'static str {
    "d /var/backups/daemonclaw 0750 root agents - -\n\
     e /var/backups/daemonclaw - - - 30d -\n"
}

// ── Systemd installation ─────────────────────────────────────────────

fn install_systemd(cli: &Cli, log: &mut ChangeLog) -> Result<(), String> {
    let units: Vec<(PathBuf, String, &str)> = vec![
        (
            PathBuf::from("/etc/systemd/system/daemonclaw.service"),
            generate_service_unit(cli),
            "service unit",
        ),
        (
            PathBuf::from("/etc/systemd/system/daemonclaw.slice"),
            generate_slice(),
            "resource slice",
        ),
        (
            PathBuf::from("/etc/systemd/system/daemonclaw-backup.timer"),
            generate_backup_timer().to_string(),
            "backup timer",
        ),
        (
            PathBuf::from("/etc/systemd/system/daemonclaw-backup.service"),
            generate_backup_service(&cli.home),
            "backup service",
        ),
    ];

    if log.dry_run {
        for (path, _, desc) in &units {
            log.record("systemd", format!("would write {} ({desc})", path.display()));
        }
        log.record("systemd", "would run systemctl daemon-reload".to_string());
        log.record("systemd", "would enable daemonclaw.service".to_string());
        log.record("systemd", "would enable daemonclaw-backup.timer".to_string());
        return Ok(());
    }

    for (path, content, desc) in &units {
        fs::write(path, content)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        log.record("systemd", format!("wrote {} ({desc})", path.display()));
    }

    run("systemctl", &["daemon-reload"])?;
    log.record("systemd", "daemon-reload".to_string());

    run("systemctl", &["enable", "daemonclaw.service"])?;
    log.record("systemd", "enabled daemonclaw.service".to_string());

    run("systemctl", &["enable", "daemonclaw-backup.timer"])?;
    log.record("systemd", "enabled daemonclaw-backup.timer".to_string());

    Ok(())
}

// ── Backup directory + tmpfiles ──────────────────────────────────────
// ACCESS_MATRIX.md § External backups: /var/backups/daemonclaw/ root:agents 0750

fn install_backup_infra(log: &mut ChangeLog) -> Result<(), String> {
    let backup_dir = PathBuf::from("/var/backups/daemonclaw");
    let tmpfiles_conf = PathBuf::from("/etc/tmpfiles.d/daemonclaw-backups.conf");

    // Backup directory
    if backup_dir.exists() {
        log.record("backup", format!("{} already exists", backup_dir.display()));
    } else if log.dry_run {
        log.record("backup", format!("would create {} (root:{AGENTS_GROUP} 0750)", backup_dir.display()));
    } else {
        fs::create_dir_all(&backup_dir)
            .map_err(|e| format!("mkdir {}: {e}", backup_dir.display()))?;
        fs::set_permissions(&backup_dir, fs::Permissions::from_mode(0o750))
            .map_err(|e| format!("chmod {}: {e}", backup_dir.display()))?;
        set_ownership(&backup_dir, "root", AGENTS_GROUP)?;
        log.record("backup", format!("created {} (root:{AGENTS_GROUP} 0750)", backup_dir.display()));
    }

    // tmpfiles rotation config
    if tmpfiles_conf.exists() {
        log.record("backup", format!("{} already exists", tmpfiles_conf.display()));
    } else if log.dry_run {
        log.record("backup", format!("would write {} (30d rotation)", tmpfiles_conf.display()));
    } else {
        fs::write(&tmpfiles_conf, generate_tmpfiles_conf())
            .map_err(|e| format!("write {}: {e}", tmpfiles_conf.display()))?;
        log.record("backup", format!("wrote {} (30d rotation)", tmpfiles_conf.display()));
    }

    Ok(())
}

// ── Main ─────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    let cli = Cli::parse();

    eprintln!("DaemonClaw System Installer v{}", env!("CARGO_PKG_VERSION"));
    if cli.dry_run {
        eprintln!("  (dry-run mode — no changes will be made)");
    }
    eprintln!();

    // Must run as root
    if !cli.dry_run {
        match Command::new("id").arg("-u").output() {
            Ok(o) if String::from_utf8_lossy(&o.stdout).trim() != "0" => {
                eprintln!("Error: daemonclaw-install must be run as root");
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("Error: cannot determine uid: {e}");
                return ExitCode::FAILURE;
            }
            _ => {}
        }
    }

    let mut log = ChangeLog::new(cli.dry_run);

    let steps: Vec<(&str, Box<dyn FnOnce(&mut ChangeLog) -> Result<(), String>>)> = vec![
        ("Creating groups", Box::new(|log| create_groups(&cli.user, log))),
        ("Creating service account", Box::new(|log| create_user(&cli.user, &cli.home, log))),
        ("Creating directory structure", Box::new(|log| create_directories(&cli.user, &cli.home, log))),
        ("Setting default ACLs", Box::new(|log| set_default_acls(&cli.home, log))),
        ("Installing config", Box::new(|log| install_config(&cli, log))),
        ("Generating secret key", Box::new(|log| install_secret_key(&cli, log))),
        ("Installing systemd units", Box::new(|log| install_systemd(&cli, log))),
        ("Setting up backup infrastructure", Box::new(|log| install_backup_infra(log))),
    ];

    for (label, step) in steps {
        eprintln!("── {label} ──");
        if let Err(e) = step(&mut log) {
            eprintln!("  FAILED: {e}");
            log.print_summary();
            return ExitCode::FAILURE;
        }
    }

    log.print_summary();

    eprintln!();
    if cli.dry_run {
        eprintln!("Dry run complete. Re-run without --dry-run to apply changes.");
    } else {
        eprintln!("Installation complete.");
        eprintln!();
        eprintln!("Next steps:");
        eprintln!("  1. Copy the daemonclaw binary to {}", cli.bin_path.display());
        eprintln!("  2. Edit /etc/daemonclaw/config.toml — add your API key and Telegram bot token");
        eprintln!("  3. Start the service:  systemctl start daemonclaw");
        eprintln!("  4. Check status:       journalctl -u daemonclaw -f");
    }

    ExitCode::SUCCESS
}
