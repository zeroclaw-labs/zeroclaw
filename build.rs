use std::process::Command;

fn main() {
    // Re-run if web source files change
    println!("cargo:rerun-if-changed=web/src/");
    println!("cargo:rerun-if-changed=web/index.html");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/vite.config.ts");
    println!("cargo:rerun-if-changed=web/tsconfig.json");

    let web_dir = std::path::Path::new("web");

    // Skip if node_modules not installed
    if !web_dir.join("node_modules").exists() {
        let status = Command::new("npm")
            .args(["install"])
            .current_dir(web_dir)
            .status();

        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("warning: `npm install` exited with {s}; web dashboard may be stale");
                return;
            }
            Err(e) => {
                eprintln!("warning: could not run `npm install`: {e}; web dashboard may be stale");
                return;
            }
        }
    }

    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(web_dir)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("warning: `npm run build` exited with {s}; web dashboard may be stale");
        }
        Err(e) => {
            eprintln!("warning: could not run `npm run build`: {e}; web dashboard may be stale");
        }
    }
}
