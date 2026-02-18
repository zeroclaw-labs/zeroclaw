use anyhow::{bail, Context as _, Result};
use std::path::{Path, PathBuf};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use zeroclaw::config::zeroclaw_home;
use zeroclaw::db::Registry;
use zeroclaw::lifecycle;
use zeroclaw::migrate;

const USAGE: &str = "\
Usage: zeroclaw-cp [command]

Commands:
  serve                                    Start the control plane server (default)
  start <name>                             Start an instance
  stop <name>                              Stop an instance
  restart <name>                           Restart an instance
  status [<name>]                          Show instance status (all or one)
  logs <name> [-n <lines>] [-f]            Show instance logs
  migrate from-openclaw <path> [--dry-run] Migrate agents from OpenClaw config

Options:
  -h, --help                               Show this help message";

enum CliCommand {
    Serve,
    Start { name: String },
    Stop { name: String },
    Restart { name: String },
    Status { name: Option<String> },
    Logs {
        name: String,
        lines: usize,
        follow: bool,
    },
    Migrate { path: PathBuf, dry_run: bool },
}

fn parse_args() -> Result<CliCommand> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        return Ok(CliCommand::Serve);
    }

    match args[0].as_str() {
        "-h" | "--help" => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        "serve" => {
            if args.len() > 1 {
                bail!("Unexpected arguments after 'serve'\n{USAGE}");
            }
            Ok(CliCommand::Serve)
        }
        "start" => {
            let name = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Missing instance name\n{USAGE}"))?;
            if args.len() > 2 {
                bail!("Unexpected arguments after instance name\n{USAGE}");
            }
            Ok(CliCommand::Start {
                name: name.clone(),
            })
        }
        "stop" => {
            let name = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Missing instance name\n{USAGE}"))?;
            if args.len() > 2 {
                bail!("Unexpected arguments after instance name\n{USAGE}");
            }
            Ok(CliCommand::Stop {
                name: name.clone(),
            })
        }
        "restart" => {
            let name = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Missing instance name\n{USAGE}"))?;
            if args.len() > 2 {
                bail!("Unexpected arguments after instance name\n{USAGE}");
            }
            Ok(CliCommand::Restart {
                name: name.clone(),
            })
        }
        "status" => {
            let name = args.get(1).cloned();
            if args.len() > 2 {
                bail!("Unexpected arguments after instance name\n{USAGE}");
            }
            Ok(CliCommand::Status { name })
        }
        "logs" => parse_logs(&args[1..]),
        "migrate" => parse_migrate(&args[1..]),
        other => bail!("Unknown command: {other}\n{USAGE}"),
    }
}

fn parse_logs(args: &[String]) -> Result<CliCommand> {
    if args.is_empty() {
        bail!("Missing instance name\n{USAGE}");
    }

    let name = args[0].clone();
    let mut lines = lifecycle::DEFAULT_LOG_LINES;
    let mut follow = false;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--follow" => {
                follow = true;
                i += 1;
            }
            "-n" | "--lines" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("Missing value for -n\n{USAGE}"))?;
                lines = val
                    .parse()
                    .with_context(|| format!("Invalid line count: {val}"))?;
                i += 2;
            }
            s if s.starts_with('-') => bail!("Unknown flag: {s}\n{USAGE}"),
            s => bail!("Unexpected argument: {s}\n{USAGE}"),
        }
    }

    Ok(CliCommand::Logs {
        name,
        lines,
        follow,
    })
}

fn parse_migrate(args: &[String]) -> Result<CliCommand> {
    if args.first().map(|s| s.as_str()) != Some("from-openclaw") {
        bail!("Unknown migrate source\n{USAGE}");
    }

    let mut path: Option<PathBuf> = None;
    let mut dry_run = false;

    for arg in &args[1..] {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            s if s.starts_with('-') => bail!("Unknown flag: {s}\n{USAGE}"),
            s => {
                if path.is_some() {
                    bail!("Unexpected argument: {s}\n{USAGE}");
                }
                path = Some(PathBuf::from(s));
            }
        }
    }

    let path = path.ok_or_else(|| anyhow::anyhow!("Missing path\n{USAGE}"))?;
    if !path.is_file() {
        bail!("Not a file: {}", path.display());
    }

    Ok(CliCommand::Migrate { path, dry_run })
}

fn main() -> Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let cmd = parse_args()?;

    match cmd {
        CliCommand::Serve => run_server(),
        CliCommand::Start { name } => run_start(&name),
        CliCommand::Stop { name } => run_stop(&name),
        CliCommand::Restart { name } => run_restart(&name),
        CliCommand::Status { name } => run_status(name.as_deref()),
        CliCommand::Logs {
            name,
            lines,
            follow,
        } => run_logs(&name, lines, follow),
        CliCommand::Migrate { path, dry_run } => run_migration(&path, dry_run),
    }
}

fn cp_dir() -> PathBuf {
    let home = zeroclaw_home();
    home.join("cp")
}

fn instances_dir(cp: &Path) -> PathBuf {
    cp.join("instances")
}

fn registry_path(cp: &Path) -> PathBuf {
    cp.join("registry.db")
}

/// Open the registry (creating CP dirs if needed).
fn open_registry() -> Result<Registry> {
    let cp = cp_dir();
    std::fs::create_dir_all(&cp)?;
    let inst_dir = instances_dir(&cp);
    std::fs::create_dir_all(&inst_dir)?;
    Registry::open(&registry_path(&cp))
}

fn run_start(name: &str) -> Result<()> {
    let registry = open_registry()?;
    lifecycle::start_instance(&registry, name)
}

fn run_stop(name: &str) -> Result<()> {
    let registry = open_registry()?;
    lifecycle::stop_instance(&registry, name)
}

fn run_restart(name: &str) -> Result<()> {
    let registry = open_registry()?;
    lifecycle::restart_instance(&registry, name)
}

fn run_status(name: Option<&str>) -> Result<()> {
    let registry = open_registry()?;
    lifecycle::show_status(&registry, name)
}

fn run_logs(name: &str, lines: usize, follow: bool) -> Result<()> {
    let registry = open_registry()?;
    let inst_dir = lifecycle::resolve_instance_dir(&registry, name)?;
    lifecycle::show_logs(&inst_dir, lines, follow)
}

fn run_server() -> Result<()> {
    let cp = cp_dir();
    std::fs::create_dir_all(&cp)?;
    let inst_dir = instances_dir(&cp);
    std::fs::create_dir_all(&inst_dir)?;

    // Acquire migration lock for entire server lifetime
    let _migration_lock = migrate::acquire_migration_lock(&cp)
        .context("Cannot start server: migration lock held (migration in progress?)")?;

    let registry = Registry::open(&registry_path(&cp))?;

    // Run reconciliation (lock already held)
    let all_resolved = migrate::reconcile_inner(&cp, &registry, &inst_dir)?;
    if !all_resolved {
        tracing::warn!(
            "Some pending migration manifests could not be fully resolved. Server starting anyway."
        );
    }

    // List instances
    let instances = registry.list_instances()?;
    println!("ZeroClaw Control Plane");
    println!("Instances: {}", instances.len());
    for inst in &instances {
        println!(
            "  {} (id: {}, port: {}, status: {})",
            inst.name, inst.id, inst.port, inst.status
        );
    }

    // TODO: actual HTTP server for instance management (Phase 7)
    println!("\nServer ready. Press Ctrl+C to stop.");

    // Block until signal
    let (tx, rx) = std::sync::mpsc::channel();
    ctrlc_channel(tx);
    let _ = rx.recv();
    println!("\nShutting down.");

    Ok(())
}

fn ctrlc_channel(tx: std::sync::mpsc::Sender<()>) {
    let _ = std::thread::spawn(move || {
        // Simple signal handling: wait for SIGINT/SIGTERM
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1];
        // This blocks until stdin closes (Ctrl+C sends SIGINT which kills the process)
        let _ = stdin.read(&mut buf);
        let _ = tx.send(());
    });
}

fn run_migration(config_path: &Path, dry_run: bool) -> Result<()> {
    let cp = cp_dir();
    std::fs::create_dir_all(&cp)?;
    let inst_dir = instances_dir(&cp);
    std::fs::create_dir_all(&inst_dir)?;

    // Acquire migration lock
    let _lock = migrate::acquire_migration_lock(&cp).context(
        "Cannot migrate: lock held. Is the CP server running? Stop it first.",
    )?;

    let registry = Registry::open(&registry_path(&cp))?;

    // TCP probe as UX hint (best-effort, not safety boundary)
    let port = std::env::var("ZEROCLAW_CP_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(18800);
    if std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
    {
        tracing::warn!("Something is listening on port {port} (not necessarily the CP server)");
    }

    // Run reconciliation (lock already held)
    let all_resolved = migrate::reconcile_inner(&cp, &registry, &inst_dir)?;
    if !all_resolved {
        bail!(
            "Cannot migrate: unresolved pending manifests from a prior migration. \
             Inspect and resolve manually, or start the server to trigger reconciliation."
        );
    }

    // Run the migration
    let report = migrate::openclaw::run_openclaw_migration(
        config_path,
        dry_run,
        &cp,
        &registry,
        &inst_dir,
    )?;

    println!("{report}");

    if !report.errors.is_empty() {
        std::process::exit(1);
    }

    Ok(())
}
