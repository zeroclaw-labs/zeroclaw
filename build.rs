fn main() {
    let dir = std::path::Path::new("web/dist");
    if !dir.exists() {
        std::fs::create_dir_all(dir).expect("failed to create web/dist/");
    }
}
