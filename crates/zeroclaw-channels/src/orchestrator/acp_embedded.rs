//! ACP re-export of shared embedded-resource materialization.
//! Implementation lives in `zeroclaw_tools::embedded_resource` so MCP and ACP
//! share one helper without `zeroclaw-tools` depending on this crate.

pub use zeroclaw_tools::embedded_resource::{
    EmbeddedResourceError, MAX_EMBEDDED_FILE_BYTES, MaterializedResource, content_hash_name,
    materialize_resource_blob,
};

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use tempfile::tempdir;

    #[test]
    fn reexport_materialize_writes_blob_under_uploads() {
        let dir = tempdir().unwrap();
        let bytes = b"hello docx";
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let out = materialize_resource_blob(
            dir.path(),
            Some("file:///x/report.docx"),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            &b64,
        )
        .unwrap();
        assert!(out.abs_path.exists());
        assert!(out.marker.contains("[Document: report.docx]"));
        assert_eq!(std::fs::read(&out.abs_path).unwrap(), bytes);
    }
}
