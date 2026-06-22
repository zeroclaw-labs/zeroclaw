/// Returns true when the current runtime environment exposes Android's system
/// shell path.
pub fn is_android() -> bool {
    std::path::Path::new("/system/bin/sh").exists()
}
