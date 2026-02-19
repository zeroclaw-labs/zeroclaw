use anyhow::{bail, Result};
use clap::CommandFactory;
use clap_complete::Shell;
use std::io::Write;
use std::path::PathBuf;

const MARKER: &str = "# zeroclaw completion";

pub fn install(shell: Option<Shell>) -> Result<()> {
    let shell = resolve_shell(shell)?;
    match shell {
        Shell::Bash => install_with_rc(shell, rc_file("bash")?, ".bashrc"),
        Shell::Zsh => install_with_rc(shell, rc_file("zsh")?, ".zshrc"),
        Shell::Fish => install_fish(),
        other => bail!(
            "Auto-install not supported for {other}. \
             Generate manually and source it:\n  \
             zeroclaw completion install --shell bash|zsh|fish"
        ),
    }
}

pub fn uninstall(shell: Option<Shell>) -> Result<()> {
    let shell = resolve_shell(shell)?;
    match shell {
        Shell::Bash => uninstall_with_rc(shell, rc_file("bash")?, ".bashrc"),
        Shell::Zsh => uninstall_with_rc(shell, rc_file("zsh")?, ".zshrc"),
        Shell::Fish => uninstall_fish(),
        other => bail!("Auto-uninstall not supported for {other}."),
    }
}

fn resolve_shell(shell: Option<Shell>) -> Result<Shell> {
    if let Some(s) = shell {
        return Ok(s);
    }
    let shell_path = std::env::var("SHELL").unwrap_or_default();
    let name = std::path::Path::new(&shell_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    match name {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        other => bail!(
            "Cannot auto-detect shell (got {:?}). Use --shell bash|zsh|fish",
            other
        ),
    }
}

fn home_dir() -> Result<PathBuf> {
    directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))
}

fn rc_file(shell: &str) -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(match shell {
        "bash" => home.join(".bashrc"),
        "zsh" => home.join(".zshrc"),
        _ => unreachable!(),
    })
}

fn completion_file(shell: Shell) -> Result<PathBuf> {
    let home = home_dir()?;
    let dir = home
        .join(".local")
        .join("share")
        .join("zeroclaw")
        .join("completions");
    let ext = match shell {
        Shell::Bash => "bash",
        Shell::Zsh => "zsh",
        _ => unreachable!(),
    };
    Ok(dir.join(format!("zeroclaw.{ext}")))
}

fn generate_script(shell: Shell) -> String {
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut super::Cli::command(), "zeroclaw", &mut buf);
    String::from_utf8(buf).expect("completion script is valid utf-8")
}

fn install_with_rc(shell: Shell, rc: PathBuf, rc_label: &str) -> Result<()> {
    let script = generate_script(shell);
    let file = completion_file(shell)?;
    std::fs::create_dir_all(file.parent().expect("completion file has parent dir"))?;
    std::fs::write(&file, &script)?;

    let rc_content = std::fs::read_to_string(&rc).unwrap_or_default();
    if rc_content.contains(MARKER) {
        println!("Completion already installed in ~/{rc_label}.");
        return Ok(());
    }

    let source_line = format!("[ -f '{}' ] && source '{}'", file.display(), file.display());
    let mut f = std::fs::OpenOptions::new().append(true).open(&rc)?;
    writeln!(f, "\n{MARKER}")?;
    writeln!(f, "{source_line}")?;
    println!("Completion installed. Reload your shell or run: source ~/{rc_label}");
    Ok(())
}

fn install_fish() -> Result<()> {
    let home = home_dir()?;
    let dir = home.join(".config").join("fish").join("completions");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("zeroclaw.fish");
    std::fs::write(&file, generate_script(Shell::Fish))?;
    println!("Fish completion installed at {}", file.display());
    Ok(())
}

fn uninstall_with_rc(shell: Shell, rc: PathBuf, rc_label: &str) -> Result<()> {
    let file = completion_file(shell)?;
    if file.exists() {
        std::fs::rem
ove_file(&file)?;
    }

    if rc.exists() {
        let content = std::fs::read_to_string(&rc)?;
        if content.contains(MARKER) {
            let cleaned = content
                .lines()
                .filter(|line| !line.contains(MARKER) && !line.contains("zeroclaw.bash") && !line.contains("zeroclaw.zsh"))
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&rc, cleaned + "\n")?;
            println!("Completion removed from ~/{rc_label}.");
        } else {
            println!("Completion not found in ~/{rc_label}.");
        }
    }
    Ok(())
}

fn uninstall_fish() -> Result<()> {
    let home = home_dir()?;
    let file = home
        .join(".config")
        .join("fish")
        .join("completions")
        .join("zeroclaw.fish");
    if file.exists() {
        std::fs::remove_file(&file)?;
        println!("Fish completion removed.");
    } else {
        println!("Fish completion not found.");
    }
    Ok(())
}