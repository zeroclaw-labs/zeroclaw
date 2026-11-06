//! Harmless native argv recorder for the Windows registry command regression.
//! Only writes when explicitly invoked by the test in an isolated directory.
fn main() {
    let Some(path) = std::env::var_os("ZEROCLAW_REGISTRY_ARGV_FILE") else {
        eprintln!("registry argv fixture requires an explicit output path");
        std::process::exit(2);
    };
    let args: Vec<_> = std::env::args().skip(1).collect();
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("exclusively create fixture output");
    file.write_all(&serde_json::to_vec(&args).expect("serialize fixture argv"))
        .expect("write fixture argv");
}
