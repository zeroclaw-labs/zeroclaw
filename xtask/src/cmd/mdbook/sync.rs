use crate::util::*;
use std::path::Path;
use std::process::Command;

pub fn run(
    locale: Option<&str>,
    force: bool,
    provider: Option<&str>,
    batch: Option<usize>,
) -> anyhow::Result<()> {
    let root = repo_root();
    require_tool(
        "mdbook-xgettext",
        "cargo install mdbook-i18n-helpers --locked",
    )?;
    require_tool("msgmerge", "apt install gettext / brew install gettext")?;
    require_tool("msginit", "apt install gettext / brew install gettext")?;
    require_tool("msgfmt", "apt install gettext / brew install gettext")?;
    require_tool("msgattrib", "apt install gettext / brew install gettext")?;

    let book = book_dir(&root);
    let po_dir = po_dir(&root);
    let pot = pot_file(&root);

    std::fs::create_dir_all(&po_dir)?;

    // Step 1: extract English msgids
    println!("==> Extracting English msgids → {}", pot.display());
    run_cmd(
        Command::new("mdbook")
            .args(["build", "-d", "po-extract"])
            .env("MDBOOK_OUTPUT__XGETTEXT__POT_FILE", "messages.pot")
            .current_dir(&book),
    )?;

    let extracted = book.join("po-extract/xgettext/messages.pot");
    if extracted.exists() {
        std::fs::rename(&extracted, &pot)?;
    }
    let _ = std::fs::remove_dir_all(book.join("po-extract"));

    if !pot.exists() {
        anyhow::bail!(
            "messages.pot not generated — is mdbook-i18n-helpers installed?\n  \
             cargo install mdbook-i18n-helpers --locked"
        );
    }

    // Step 2+3: per-locale merge + AI fill
    let targets: Vec<String> = match locale {
        Some(l) => vec![l.to_string()],
        None => locales().into_iter().filter(|l| l != "en").collect(),
    };

    for locale in &targets {
        if locale == "en" {
            continue;
        }
        let po_file = po_dir.join(format!("{locale}.po"));

        if !po_file.exists() {
            println!("==> {locale}: bootstrapping new .po from template");
            run_cmd(
                Command::new("msginit")
                    .args(["--no-translator", &format!("--locale={locale}"), "--input"])
                    .arg(&pot)
                    .arg("--output")
                    .arg(&po_file),
            )?;
        } else {
            println!("==> {locale}: msgmerge");
            run_cmd(
                Command::new("msgmerge")
                    .args(["--update", "--backup=none", "--no-fuzzy-matching"])
                    .arg(&po_file)
                    .arg(&pot),
            )?;
            // Strip obsolete (#~) entries that msgmerge leaves behind for removed source strings.
            run_cmd(
                Command::new("msgattrib")
                    .arg("--no-obsolete")
                    .arg("--output-file")
                    .arg(&po_file)
                    .arg(&po_file),
            )?;
        }

        if force {
            if let Some(p) = provider {
                println!("==> {locale}: --force: re-translating all entries");
                fill(&root, &po_file, locale, true, p, batch)?;
            } else {
                println!(
                    "==> {locale}: --force requested but no --provider specified — skipping AI step"
                );
            }
        } else {
            let delta = count_delta(&po_file)?;
            if delta > 0 {
                if let Some(p) = provider {
                    println!("==> {locale}: AI-filling {delta} entries");
                    fill(&root, &po_file, locale, false, p, batch)?;
                } else {
                    println!(
                        "==> {locale}: {delta} entries need translation (use --provider <name> to auto-fill)"
                    );
                }
            } else {
                println!("==> {locale}: up to date, skipping AI step");
            }
        }
    }

    println!("\n==> Translation summary:");
    for locale in &targets {
        if locale == "en" {
            continue;
        }
        let po_file = po_dir.join(format!("{locale}.po"));
        if !po_file.exists() {
            continue;
        }
        print!("    {locale:<8} ");
        let out = Command::new("msgfmt")
            .args(["--statistics", "-o", "/dev/null"])
            .arg(&po_file)
            .env("LANG", "C")
            .output()?;
        print!("{}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

fn fill(
    root: &Path,
    po_file: &Path,
    locale: &str,
    force: bool,
    provider: &str,
    batch: Option<usize>,
) -> anyhow::Result<()> {
    // Build and invoke the binary directly — `cargo run` wraps the child in a way that
    // breaks Ctrl-C propagation, leaving the translator orphaned in the terminal.
    let bin = root.join("target/release/fill-translations");
    run_cmd(
        Command::new("cargo")
            .args(["build", "--release", "-q", "--manifest-path"])
            .arg(root.join("tools/fill-translations/Cargo.toml")),
    )?;
    let mut cmd = Command::new(&bin);
    cmd.args(["--po"])
        .arg(po_file)
        .args(["--locale", locale])
        .args(["--provider", provider]);
    if let Some(b) = batch {
        cmd.args(["--batch", &b.to_string()]);
    }
    if force {
        cmd.arg("--force");
    }
    run_cmd(&mut cmd)
}

pub fn count_delta(po_file: &Path) -> anyhow::Result<u32> {
    let out = Command::new("msgfmt")
        .args(["--statistics", "-o", "/dev/null"])
        .arg(po_file)
        .env("LANG", "C")
        .output()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let mut total = 0u32;
    let words: Vec<&str> = text.split_whitespace().collect();
    for i in 0..words.len().saturating_sub(1) {
        if let Ok(n) = words[i].parse::<u32>() {
            let next = words[i + 1];
            if next.starts_with("fuzzy") || next.starts_with("untranslated") {
                total += n;
            }
        }
    }
    Ok(total)
}
