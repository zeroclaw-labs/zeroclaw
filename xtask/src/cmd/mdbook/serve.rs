use crate::cmd::mdbook::refs::{build_api, build_refs};
use crate::util::*;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const PORT: u16 = 3000;

pub fn run(locale: Option<&str>, tag: Option<&str>) -> anyhow::Result<()> {
    let root = repo_root();
    require_tool("cargo", "https://rustup.rs")?;
    ensure_cargo_tool("mdbook", "mdbook")?;
    ensure_cargo_tool("mdbook-xgettext", "mdbook-i18n-helpers")?;
    ensure_cargo_tool("mdbook-gettext", "mdbook-i18n-helpers")?;
    ensure_cargo_tool("mdbook-mermaid", "mdbook-mermaid")?;

    let entries = locale_entries();
    if let Some(code) = locale
        && !entries.iter().any(|e| e.code == code)
    {
        anyhow::bail!(
            "locale '{code}' not in locales.toml (known: {})",
            entries
                .iter()
                .map(|e| e.code.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let ref_dir = ref_dir(&root);
    if !ref_dir.join("cli.md").exists() || !ref_dir.join("config.md").exists() {
        build_refs(&root)?;
    }
    if !root.join("target/doc").exists() {
        build_api(&root)?;
    }

    let book = book_dir(&root);
    let tag_dir = tag.unwrap_or("master");
    let out_dir = book.join("book").join(tag_dir);

    // Lang switcher always advertises every locale from locales.toml — switching
    // to an unbuilt locale will 404 in single-locale mode, which is fine for
    // local iteration.
    crate::cmd::mdbook::build::inject_lang_switcher_locales(&book, &entries)?;
    crate::cmd::mdbook::themes::run(&root)?;
    crate::cmd::mdbook::keymap::run(&root)?;

    // Watched locale: the one passed in, or the first entry in locales.toml.
    let watch_locale = locale
        .map(str::to_string)
        .or_else(|| entries.first().map(|e| e.code.clone()))
        .ok_or_else(|| anyhow::Error::msg("locales.toml has no entries"))?;

    match locale {
        Some(code) => {
            println!("==> Building locale '{code}' for serve...");
            build_one_locale(&book, tag_dir, code)?;
        }
        None => {
            println!("==> Building all locales for serve...");
            crate::cmd::mdbook::build::build_locales(&root, tag)?;
        }
    }
    crate::cmd::mdbook::build::assemble(&root, tag)?;

    // Watch the active locale for live-reload (rebuilds book/{locale}/ on change)
    let mut watch_cmd = Command::new(mdbook_program()?);
    watch_cmd
        .args(["watch", "-d", &format!("book/{}/{}", tag_dir, watch_locale)])
        .env("MDBOOK_BOOK__LANGUAGE", &watch_locale)
        .current_dir(&book)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some((key, value)) = peer_groups_preprocessor_env() {
        watch_cmd.env(key, value);
    }
    let mut watch = watch_cmd.spawn()?;

    // Re-copy api/ whenever mdbook's clean-on-rebuild removes it
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    let root_clone = root.clone();
    let out_dir_clone = out_dir.clone();
    std::thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if out_dir_clone.exists()
                && !out_dir_clone.join("api").exists()
                && root_clone.join("target/doc").exists()
            {
                let _ = copy_dir_all(root_clone.join("target/doc"), out_dir_clone.join("api"));
            }
        }
    });

    let url = format!("http://localhost:{PORT}");
    match locale {
        Some(code) => {
            let label = entries
                .iter()
                .find(|e| e.code == code)
                .map(|e| e.label.as_str())
                .unwrap_or(code);
            println!("==> Serving locale '{code}' at {url}");
            println!("    {label:<16} {url}/{code}/");
            println!(
                "    (other locales in the language switcher will 404 — run without --locale to build them all)"
            );
        }
        None => {
            println!("==> Serving all locales at {url}");
            for entry in &entries {
                println!("    {:<16} {url}/{}/", entry.label, entry.code);
            }
        }
    }
    println!("    API reference:  {url}/api/index.html");
    println!("    Live-reload:    watching locale '{watch_locale}'");
    println!("    Press Ctrl-C to stop.");

    let _ = Command::new("xdg-open")
        .arg(&url)
        .spawn()
        .or_else(|_| Command::new("open").arg(&url).spawn());

    // Serve with axum + tower-http ServeDir — no Python required
    let result = tokio::runtime::Runtime::new()?.block_on(serve_static(out_dir.clone()));

    running.store(false, Ordering::Relaxed);
    let _ = watch.kill();
    let _ = watch.wait();

    result
}

fn build_one_locale(book: &Path, tag_dir: &str, locale: &str) -> anyhow::Result<()> {
    let dest = format!("book/{}/{}", tag_dir, locale);
    let mut cmd = Command::new(mdbook_program()?);
    cmd.args(["build", "-d", &dest])
        .env("MDBOOK_BOOK__LANGUAGE", locale)
        .current_dir(book);
    if let Some((key, value)) = peer_groups_preprocessor_env() {
        cmd.env(key, value);
    }
    run_cmd(&mut cmd)
}

async fn serve_static(dir: std::path::PathBuf) -> anyhow::Result<()> {
    use axum::Router;
    use tower_http::services::ServeDir;

    let shared_dir = dir.parent().unwrap().join("_shared");
    let app = Router::new()
        .nest_service("/_shared", ServeDir::new(&shared_dir))
        .fallback_service(ServeDir::new(&dir).append_index_html_on_directories(true));
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{PORT}")).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
