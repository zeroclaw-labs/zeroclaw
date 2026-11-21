//! Controlled child for the hosted-Windows service smoke.
//!
//! This example is not shipped with the ZeroClaw binary. It deliberately calls
//! the production service functions so Task Scheduler, bounded capture, and Job
//! Object ownership are exercised without adding a production test switch.

#[cfg(not(windows))]
fn main() {
    eprintln!("windows_service_smoke_fixture is only supported on Windows");
    std::process::exit(1);
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    use std::ffi::OsString;
    use std::io::{Write, stdout};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use zeroclaw_config::schema::Config;
    use zeroclaw_runtime::service::{
        InitSystem, install, logs, run_windows_daemon, start, status, stop, uninstall,
    };

    const PAYLOAD_BYTES: usize = 9 * 1024 * 1024;
    const PAYLOAD_CHUNK_BYTES: usize = 64 * 1024;

    fn parse_args() -> anyhow::Result<(PathBuf, Vec<OsString>)> {
        let mut args = std::env::args_os().skip(1);
        let flag = args.next().context("missing --config-dir")?;
        if flag != "--config-dir" {
            bail!("expected --config-dir as the first argument");
        }
        let config_dir = PathBuf::from(args.next().context("missing config directory")?);
        Ok((config_dir, args.collect()))
    }

    fn config_at(config_dir: &Path) -> Config {
        Config {
            data_dir: config_dir.join("data"),
            config_path: config_dir.join("config.toml"),
            ..Config::default()
        }
    }

    fn run_fixture_daemon(config_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        std::fs::write(
            config_dir.join("daemon-started.pid"),
            std::process::id().to_string(),
        )?;

        let descendant = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 600",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn smoke descendant")?;
        std::fs::write(
            config_dir.join("descendant.pid"),
            descendant.id().to_string(),
        )?;

        fn write_stress_stream(
            writer: &mut impl Write,
            fill: u8,
            marker: &str,
        ) -> anyhow::Result<()> {
            let mut chunk = vec![fill; PAYLOAD_CHUNK_BYTES];
            chunk[PAYLOAD_CHUNK_BYTES - 1] = b'\n';
            // One extra chunk keeps each stream strictly above the stress target.
            for _ in 0..(PAYLOAD_BYTES / PAYLOAD_CHUNK_BYTES + 1) {
                writer.write_all(&chunk)?;
            }
            writer.write_all(marker.as_bytes())?;
            writer.flush()?;
            Ok(())
        }

        let mut out = stdout().lock();
        write_stress_stream(&mut out, b'O', "ZEROCLAW_STDOUT_界_MARKER\n")?;
        let mut err = std::io::stderr().lock();
        write_stress_stream(&mut err, b'E', "ZEROCLAW_STDERR_界_MARKER\n")?;

        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    let (config_dir, args) = parse_args()?;
    let command: Vec<String> = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    match command.as_slice() {
        [daemon] if daemon == "daemon" => run_fixture_daemon(&config_dir),
        [service, action] if service == "service" && action == "run-windows-daemon" => {
            let result = run_windows_daemon(&config_dir).await;
            if let Err(error) = &result {
                // Task Scheduler does not retain stderr from the runner. Keep a
                // fixture-only receipt so hosted smoke failures remain diagnosable.
                let _ = std::fs::write(config_dir.join("runner-error.txt"), format!("{error:#}\n"));
            }
            result
        }
        [service, action] if service == "service" => {
            let config = config_at(&config_dir);
            match action.as_str() {
                "install" => install(&config, InitSystem::Auto),
                "start" => start(&config, InitSystem::Auto),
                "status" => status(&config, InitSystem::Auto),
                "stop" => stop(&config, InitSystem::Auto),
                "uninstall" => uninstall(&config, InitSystem::Auto),
                "logs" => logs(&config, InitSystem::Auto, 15, false),
                _ => bail!("unsupported service action: {action}"),
            }
        }
        _ => bail!("unsupported smoke fixture arguments: {command:?}"),
    }
}
