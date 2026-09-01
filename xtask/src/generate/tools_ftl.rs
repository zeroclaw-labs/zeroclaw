//! Mirror the canonical English tool catalogue into `zeroclaw-tools`.
//!
//! `crates/zeroclaw-runtime/locales/en/tools.ftl` stays the source of truth, and
//! `cargo fluent fill` keeps translations beside it. `zeroclaw-tools` embeds the
//! English strings itself rather than depending on `zeroclaw-runtime`, because
//! the dependency runs runtime -> tools and importing back would invert it.
//!
//! That embed used to be `include_str!("../../zeroclaw-runtime/locales/en/tools.ftl")`,
//! which reaches outside the crate directory. `cargo package` copies only the
//! package directory, so the read made `zeroclaw-tools` unpublishable. Mirroring
//! the file into the crate keeps the dependency direction intact and puts the
//! bytes where packaging can see them; CI fails on drift.

use std::path::Path;

pub const SOURCE: &str = "crates/zeroclaw-runtime/locales/en/tools.ftl";

const HEADER: &str = "\
# GENERATED from crates/zeroclaw-runtime/locales/en/tools.ftl by
# `cargo generate installers` - do not edit by hand. Edit the runtime catalogue
# instead, then regenerate. CI fails on drift via `cargo generate installers --check`.
";

/// Render the mirrored catalogue: the canonical file with a provenance header.
pub fn render_file(root: &Path, _current: &str) -> anyhow::Result<String> {
    let path = root.join(SOURCE);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::Error::msg(format!("{}: {e}", path.display())))?;
    Ok(render(&raw))
}

fn render(canonical: &str) -> String {
    format!("{HEADER}\n{canonical}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_canonical_body_survives_verbatim() {
        let canonical = "tool-x-description = Does a thing\ntool-y-description = Does another\n";
        let out = render(canonical);
        assert!(
            out.ends_with(canonical),
            "the mirrored catalogue must carry the source bytes unchanged"
        );
    }

    #[test]
    fn the_mirror_is_marked_generated() {
        let out = render("a = b\n");
        assert!(out.starts_with('#'), "provenance header comes first");
        assert!(out.contains("do not edit by hand"));
        assert!(
            out.contains(SOURCE),
            "the header names the canonical source so an editor knows where to go"
        );
    }
}
