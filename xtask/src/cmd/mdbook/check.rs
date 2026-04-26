use crate::util::*;
use std::process::Command;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root();
    let po_dir = po_dir(&root);

    let mut entries: Vec<_> = std::fs::read_dir(&po_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "po"))
        .collect();
    entries.sort_by_key(|e| e.path());

    let mut failed = false;
    for entry in entries {
        let path = entry.path();
        let locale = path.file_stem().unwrap_or_default().to_string_lossy();
        let status = Command::new("msgfmt")
            .args(["--check-format", "-o", "/dev/null"])
            .arg(&path)
            .status()?;
        if !status.success() {
            eprintln!("FAIL: {locale}");
            failed = true;
        }
    }

    if failed {
        anyhow::bail!("one or more .po files have format errors");
    }
    println!("All .po files OK.");
    Ok(())
}
