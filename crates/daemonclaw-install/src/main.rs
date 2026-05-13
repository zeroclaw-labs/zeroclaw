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

fn set_ownership_recursive(path: &Path, user: &str, group: &str) -> Result<(), String> {
    run("chown", &["-R", &format!("{user}:{group}"), &path.to_string_lossy()])?;
    Ok(())
}

// ── Groups ───────────────────────────────────────────────────────────

struct GroupDef {
    name: String,
    description: &'static str,
}

fn group_defs(user: &str) -> Vec<GroupDef> {
    vec![
        GroupDef {
            name: user.to_string(),
            description: "service account primary group",
        },
        GroupDef {
            name: format!("{user}-admin-read"),
            description: "read-only access to agent state and logs",
        },
        GroupDef {
            name: format!("{user}-admin-write"),
            description: "read-write access to agent config and state",
        },
        GroupDef {
            name: format!("{user}-sandbox"),
            description: "dev-tool sandbox group for agent subprocesses",
        },
    ]
}

fn create_groups(groups: &[GroupDef], log: &mut ChangeLog) -> Result<(), String> {
    for g in groups {
        if group_exists(&g.name) {
            log.record("group", format!("{} already exists ({})", g.name, g.description));
        } else if !log.dry_run {
            run("groupadd", &["--system", &g.name])?;
            log.record("group", format!("created {} ({})", g.name, g.description));
        } else {
            log.record("group", format!("would create {} ({})", g.name, g.description));
        }
    }
    Ok(())
}

// ── User ─────────────────────────────────────────────────────────────

fn create_user(user: &str, home: &Path, log: &mut ChangeLog) -> Result<(), String> {
    if user_exists(user) {
        log.record("user", format!("{user} already exists"));
        return Ok(());
    }
    if log.dry_run {
        log.record("user", format!("would create system user {user} with home {}", home.display()));
        return Ok(());
    }
    run(
        "useradd",
        &[
            "--system",
            "--gid", user,
            "--home-dir", &home.to_string_lossy(),
            "--create-home",
            "--shell", "/usr/sbin/nologin",
            user,
        ],
    )?;
    log.record("user", format!("created system user {user} with home {}", home.display()));
    Ok(())
}

// ── Directories ──────────────────────────────────────────────────────

struct DirDef {
    relative: &'static str,
    mode: u32,
    owner_group: &'static str, // "service" | "admin-read" | "admin-write" | "sandbox"
    description: &'static str,
}

const DIRS: &[DirDef] = &[
    DirDef { relative: ".daemonclaw",                    mode: 0o750, owner_group: "service",     description: "main state directory" },
    DirDef { relative: ".daemonclaw/workspace",          mode: 0o750, owner_group: "service",     description: "agent workspace root" },
    DirDef { relative: ".daemonclaw/workspace/github",   mode: 0o750, owner_group: "service",     description: "github project clones" },
    DirDef { relative: ".daemonclaw/workspace/memory",   mode: 0o700, owner_group: "service",     description: "memory store (private)" },
    DirDef { relative: ".daemonclaw/workspace/sessions", mode: 0o700, owner_group: "service",     description: "session persistence" },
    DirDef { relative: ".daemonclaw/workspace/skills",   mode: 0o750, owner_group: "service",     description: "installed skills" },
    DirDef { relative: ".daemonclaw/state",              mode: 0o750, owner_group: "admin-read",  description: "runtime state and traces" },
    DirDef { relative: ".daemonclaw/state/backups",      mode: 0o750, owner_group: "admin-read",  description: "automated backups" },
    DirDef { relative: ".daemonclaw/logs",               mode: 0o750, owner_group: "admin-read",  description: "log directory" },
    DirDef { relative: "tmp",                            mode: 0o700, owner_group: "service",     description: "private tmp (not /tmp)" },
];

fn resolve_group(user: &str, tag: &str) -> String {
    match tag {
        "service" => user.to_string(),
        "admin-read" => format!("{user}-admin-read"),
        "admin-write" => format!("{user}-admin-write"),
        "sandbox" => format!("{user}-sandbox"),
        _ => user.to_string(),
    }
}

fn create_directories(user: &str, home: &Path, log: &mut ChangeLog) -> Result<(), String> {
    for d in DIRS {
        let full = home.join(d.relative);
        let group = resolve_group(user, d.owner_group);
        if full.exists() {
            log.record("dir", format!("{} already exists ({})", full.display(), d.description));
        } else if !log.dry_run {
            fs::create_dir_all(&full)
                .map_err(|e| format!("mkdir {}: {e}", full.display()))?;
            fs::set_permissions(&full, fs::Permissions::from_mode(d.mode))
                .map_err(|e| format!("chmod {}: {e}", full.display()))?;
            set_ownership(&full, user, &group)?;
            log.record(
                "dir",
                format!("{} (mode {:04o}, owner {user}:{group}, {})", full.display(), d.mode, d.description),
            );
        } else {
            log.record(
                "dir",
                format!("would create {} (mode {:04o}, owner {user}:{group}, {})", full.display(), d.mode, d.description),
            );
        }
    }
    Ok(())
}

// ── Config ───────────────────────────────────────────────────────────

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
Group={user}
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

# Allow agent access to its own state
ReadWritePaths={home}
ReadOnlyPaths=/proc /sys /etc/os-release /etc/hostname /etc/resolv.conf

# Resource limits
MemoryMax=8G
TasksMax=256
CPUQuota=400%

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
        home = cli.home.display(),
        bin = cli.bin_path.display(),
        host = cli.listen_host,
        port = cli.listen_port,
    )
}

// ── Systemd slice ────────────────────────────────────────────────────

fn generate_slice(_user: &str) -> String {
    format!(
        r#"[Unit]
Description=DaemonClaw agent group resource slice

[Slice]
MemoryMax=12G
TasksMax=512
CPUQuota=600%

# Future: per-agent scopes will be children of this slice,
# inheriting these limits as a group ceiling.
# e.g. daemonclaw-agent@primary.service → Slice=daemonclaw.slice
"#
    )
}

// ── Systemd installation ─────────────────────────────────────────────

fn install_systemd(cli: &Cli, log: &mut ChangeLog) -> Result<(), String> {
    let unit_path = PathBuf::from("/etc/systemd/system/daemonclaw.service");
    let slice_path = PathBuf::from("/etc/systemd/system/daemonclaw.slice");

    let unit_content = generate_service_unit(cli);
    let slice_content = generate_slice(&cli.user);

    if log.dry_run {
        log.record("systemd", format!("would write {}", unit_path.display()));
        log.record("systemd", format!("would write {}", slice_path.display()));
        log.record("systemd", "would run systemctl daemon-reload".to_string());
        log.record("systemd", "would run systemctl enable daemonclaw.service".to_string());
        return Ok(());
    }

    fs::write(&unit_path, &unit_content)
        .map_err(|e| format!("write {}: {e}", unit_path.display()))?;
    log.record("systemd", format!("wrote {}", unit_path.display()));

    fs::write(&slice_path, &slice_content)
        .map_err(|e| format!("write {}: {e}", slice_path.display()))?;
    log.record("systemd", format!("wrote {}", slice_path.display()));

    run("systemctl", &["daemon-reload"])?;
    log.record("systemd", "daemon-reload".to_string());

    run("systemctl", &["enable", "daemonclaw.service"])?;
    log.record("systemd", "enabled daemonclaw.service".to_string());

    Ok(())
}

// ── Config installation ──────────────────────────────────────────────

fn install_config(cli: &Cli, log: &mut ChangeLog) -> Result<(), String> {
    let config_path = cli.home.join(".daemonclaw/config.toml");

    if config_path.exists() {
        log.record("config", format!("{} already exists — not overwriting", config_path.display()));
        return Ok(());
    }

    let content = generate_config(cli);

    if log.dry_run {
        log.record("config", format!("would write {}", config_path.display()));
        return Ok(());
    }

    fs::write(&config_path, &content)
        .map_err(|e| format!("write {}: {e}", config_path.display()))?;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640))
        .map_err(|e| format!("chmod {}: {e}", config_path.display()))?;
    set_ownership(&config_path, &cli.user, &format!("{}-admin-write", cli.user))?;
    log.record("config", format!("wrote {} (mode 0640, owner {}:{}-admin-write)", config_path.display(), cli.user, cli.user));

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
    let groups = group_defs(&cli.user);

    let steps: Vec<(&str, Box<dyn FnOnce(&mut ChangeLog) -> Result<(), String>>)> = vec![
        ("Creating groups", Box::new(|log| create_groups(&groups, log))),
        ("Creating service account", Box::new(|log| create_user(&cli.user, &cli.home, log))),
        ("Creating directory structure", Box::new(|log| create_directories(&cli.user, &cli.home, log))),
        ("Installing systemd units", Box::new(|log| install_systemd(&cli, log))),
        ("Generating config", Box::new(|log| install_config(&cli, log))),
    ];

    for (label, step) in steps {
        eprintln!("── {label} ──");
        if let Err(e) = step(&mut log) {
            eprintln!("  FAILED: {e}");
            log.print_summary();
            return ExitCode::FAILURE;
        }
    }

    // Final ownership pass on the entire home directory
    if !cli.dry_run {
        eprintln!("── Setting ownership ──");
        if let Err(e) = set_ownership_recursive(&cli.home, &cli.user, &cli.user) {
            eprintln!("  WARNING: recursive chown failed: {e}");
        } else {
            log.record("ownership", format!("chown -R {}:{} {}", cli.user, cli.user, cli.home.display()));
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
        eprintln!("  2. Add your API key to {}", cli.home.join(".daemonclaw/config.toml").display());
        eprintln!("  3. Add your Telegram bot token to the config");
        eprintln!("  4. Start the service:  systemctl start daemonclaw");
        eprintln!("  5. Check status:       journalctl -u daemonclaw -f");
    }

    ExitCode::SUCCESS
}
