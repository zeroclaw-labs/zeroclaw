use crate::cmd::fluent::catalog::message_ids;
use crate::util::*;
use std::path::Path;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root();

    let mut error_count = 0;
    let mut checked_any = false;

    for locales_dir in fluent_catalog_roots(&root) {
        if !locales_dir.exists() {
            continue;
        }
        checked_any = true;
        for locale in fluent_locales_in(&locales_dir)? {
            let locale_dir = locales_dir.join(&locale);
            for ftl_path in ftl_files_in(&locale_dir)? {
                let errors = check_ftl_file(&ftl_path)?;
                if errors.is_empty() {
                    println!("ok  {}", ftl_path.display());
                } else {
                    for (line_no, msg) in &errors {
                        eprintln!("{}:{}: {}", ftl_path.display(), line_no, msg);
                        error_count += 1;
                    }
                }
            }
        }
    }

    if !checked_any {
        anyhow::bail!("no fluent catalogue roots found");
    }
    if error_count > 0 {
        anyhow::bail!("{error_count} FTL syntax error(s) found");
    }
    Ok(())
}

fn check_ftl_file(path: &Path) -> anyhow::Result<Vec<(usize, String)>> {
    let src = std::fs::read_to_string(path)?;
    Ok(match message_ids(&src) {
        Ok(_) => vec![],
        Err(errors) => errors
            .into_iter()
            .map(|error| (error.line, error.message))
            .collect(),
    })
}
