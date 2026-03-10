use std::fs;
use std::path::PathBuf;

const PLACEHOLDER_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>ZeroClaw Dashboard Placeholder</title>
  </head>
  <body>
    <main>
      <h1>ZeroClaw dashboard assets are not built</h1>
      <p>Run the web build to replace this placeholder with the real dashboard.</p>
    </main>
  </body>
</html>
"#;

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let dist_dir = manifest_dir.join("web").join("dist");
    let index_path = dist_dir.join("index.html");

    println!("cargo:rerun-if-changed=web/dist");

    if index_path.exists() {
        return;
    }

    fs::create_dir_all(&dist_dir).expect("failed to create web/dist placeholder directory");
    fs::write(&index_path, PLACEHOLDER_INDEX_HTML)
        .expect("failed to write placeholder web/dist/index.html");
    println!(
        "cargo:warning=web/dist was missing; generated a placeholder dashboard so the Rust build can continue"
    );
}
