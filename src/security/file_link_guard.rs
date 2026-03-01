use std::fs::Metadata;

/// Returns true when a file has multiple hard links.
///
/// Multiple links can allow path-based workspace guards to be bypassed by
/// linking a workspace path to external sensitive content.
pub fn has_multiple_hard_links(metadata: &Metadata) -> bool {
    link_count(metadata) > 1
}

#[cfg(unix)]
fn link_count(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(windows)]
fn link_count(metadata: &Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt;
    u64::from(metadata.number_of_links())
}

#[cfg(not(any(unix, windows)))]
fn link_count(_metadata: &Metadata) -> u64 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_link_file_is_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("single.txt");
        std::fs::write(&file, "hello").unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        assert!(!has_multiple_hard_links(&meta));
    }

    #[test]
    fn hard_link_file_is_flagged_when_supported() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let linked = dir.path().join("linked.txt");
        std::fs::write(&original, "hello").unwrap();

        if std::fs::hard_link(&original, &linked).is_err() {
            // Some filesystems may disable hard links; treat as unsupported.
            return;
        }

        let meta = std::fs::metadata(&original).unwrap();
        assert!(has_multiple_hard_links(&meta));
    }
}
