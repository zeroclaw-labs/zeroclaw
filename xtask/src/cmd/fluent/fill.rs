use crate::util::*;
use std::path::Path;
use std::process::Command;

pub fn run(locale: Option<&str>, force: bool) -> anyhow::Result<()> {
    let root = repo_root();
    let locales_dir = fluent_locales_dir(&root);
    let en_dir = locales_dir.join("en");

    if !en_dir.exists() {
        anyhow::bail!("English locale dir not found: {}", en_dir.display());
    }

    let targets: Vec<&str> = match locale {
        Some(l) => vec![l],
        None => locales().iter().copied().filter(|&l| l != "en").collect(),
    };

    let has_key = std::env::var("ANTHROPIC_API_KEY").is_ok_and(|v| !v.is_empty());

    for target_locale in &targets {
        let target_dir = locales_dir.join(target_locale);
        std::fs::create_dir_all(&target_dir)?;

        for en_ftl in ftl_files_in(&en_dir)? {
            let filename = en_ftl.file_name().unwrap();
            let target_ftl = target_dir.join(filename);

            let delta = count_missing(&en_ftl, &target_ftl, force)?;

            if delta == 0 {
                println!("==> {target_locale}/{}: up to date", filename.to_string_lossy());
                continue;
            }

            if !has_key {
                println!(
                    "==> {target_locale}/{}: {delta} entries need translation (set ANTHROPIC_API_KEY to auto-fill)",
                    filename.to_string_lossy()
                );
                continue;
            }

            println!("==> {target_locale}/{}: AI-filling {delta} entries", filename.to_string_lossy());
            spawn_fill(&root, &en_ftl, &target_ftl, target_locale, force)?;
        }
    }

    Ok(())
}

fn count_missing(en_path: &Path, target_path: &Path, force: bool) -> anyhow::Result<usize> {
    let en_keys = parse_ftl_keys(en_path)?;
    if force {
        return Ok(en_keys.len());
    }
    let target_keys: std::collections::HashSet<String> = if target_path.exists() {
        parse_ftl_keys(target_path)?.into_iter().collect()
    } else {
        std::collections::HashSet::new()
    };
    Ok(en_keys.iter().filter(|k| !target_keys.contains(*k)).count())
}

fn parse_ftl_keys(path: &Path) -> anyhow::Result<Vec<String>> {
    let src = std::fs::read_to_string(path)?;
    let keys = src
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with('-') {
                return None;
            }
            t.split_once(" = ").map(|(k, _)| k.trim().to_string())
        })
        .collect();
    Ok(keys)
}

fn spawn_fill(
    root: &Path,
    en_ftl: &Path,
    target_ftl: &Path,
    locale: &str,
    force: bool,
) -> anyhow::Result<()> {
    let manifest = root.join("tools/fill-fluent/Cargo.toml");
    let mut cmd = Command::new("cargo");
    cmd.args(["run", "-q", "--manifest-path"])
        .arg(&manifest)
        .arg("--")
        .args(["--en"])
        .arg(en_ftl)
        .args(["--ftl"])
        .arg(target_ftl)
        .args(["--locale", locale]);
    if force {
        cmd.arg("--force");
    }
    run_cmd(&mut cmd)
}
