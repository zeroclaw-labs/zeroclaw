//! Nix flake renderer. The flake is the one packaged surface that rebuilds from
//! source per-user, so it must expose feature selection (overridable), not a
//! fixed set. We generate a sentinel-delimited zone defining the zeroclaw +
//! zerocode packages with the canonical Dist feature list as the default
//! `buildFeatures`, overridable via `.override { features = [...]; }`. The
//! feature list and version come from the spec; nothing is typed.

use super::spec::{self, Selection};
use std::path::Path;

fn begin(zone: &str) -> String {
    format!("        # >>> generated:{zone} by `cargo generate installers` — do not edit <<<")
}
fn end(zone: &str) -> String {
    format!("        # >>> end generated:{zone} <<<")
}

const ZONE: &str = "flake-packages";

/// Render the generated package-definition zone body: a Rust package builder
/// with the Dist feature list as default buildFeatures (overridable), exposing
/// zeroclaw, zerocode, and default. Indented to sit inside the per-system `in {`
/// block of the flake.
pub fn render_zone(root: &Path) -> anyhow::Result<String> {
    let version = spec::resolve_version(root)?;
    let dist = spec::resolve_feature_list(root, &Selection::Dist)?;
    let feature_list = dist
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(" ");

    // Nix: a function over a feature list, defaulting to the canonical Dist set,
    // building each binary with --no-default-features --features <list>. Users
    // override with `.override { features = [ ... ]; }`.
    let lines = [
        "        # Default feature set: canonical Dist (all channels, no heavyweight).".to_string(),
        "        # Override with `packages.zeroclaw.override { features = [ ... ]; }`.".to_string(),
        format!("        zeroclawDefaultFeatures = [ {feature_list} ];"),
        "        buildZeroclaw = { pname, cargoPkg, features ? zeroclawDefaultFeatures }:"
            .to_string(),
        "          (pkgs.makeRustPlatform {".to_string(),
        "            cargo = rustToolchain;".to_string(),
        "            rustc = rustToolchain;".to_string(),
        "          }).buildRustPackage {".to_string(),
        "            inherit pname;".to_string(),
        format!("            version = \"{version}\";"),
        "            src = ./.;".to_string(),
        "            cargoLock.lockFile = ./Cargo.lock;".to_string(),
        "            cargoBuildFlags =".to_string(),
        "              [ \"-p\" cargoPkg \"--no-default-features\" ]".to_string(),
        "              ++ pkgs.lib.optionals (features != [])".to_string(),
        "                [ \"--features\" (pkgs.lib.concatStringsSep \",\" features) ];"
            .to_string(),
        "            doCheck = false;".to_string(),
        "          };".to_string(),
    ];
    let body = lines.join("\n");
    Ok(body)
}

/// Splice the generated package zone into the flake, preserving hand-written
/// outputs (devShell, nixos modules, checks) outside the sentinels.
pub fn render_file(root: &Path, current: &str) -> anyhow::Result<String> {
    let b = begin(ZONE);
    let e = end(ZONE);
    let begin_at = current
        .find(&b)
        .ok_or_else(|| anyhow::anyhow!("flake.nix missing generated:{ZONE} BEGIN sentinel"))?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..]
        .find(&e)
        .ok_or_else(|| anyhow::anyhow!("flake.nix missing generated:{ZONE} END sentinel"))?;
    let end_at = after_begin + end_rel;
    let body = render_zone(root)?;
    let mut out = String::new();
    out.push_str(&current[..after_begin]);
    out.push('\n');
    out.push_str(&body);
    out.push('\n');
    out.push_str(&current[end_at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn zone_exposes_overridable_features() {
        let z = render_zone(&root()).unwrap();
        assert!(
            z.contains("zeroclawDefaultFeatures"),
            "default feature list present"
        );
        assert!(
            z.contains("features ?"),
            "features parameter is overridable"
        );
        assert!(
            z.contains("buildRustPackage"),
            "real package build, not just toolchain"
        );
    }

    #[test]
    fn zone_default_is_dist_channels_no_heavyweight() {
        let z = render_zone(&root()).unwrap();
        assert!(z.contains("\"channel-discord\""), "dist ships all channels");
        assert!(!z.contains("\"hardware\""), "dist excludes heavyweight");
    }

    #[test]
    fn zone_version_from_workspace() {
        let v = spec::resolve_version(&root()).unwrap();
        let z = render_zone(&root()).unwrap();
        assert!(z.contains(&format!("version = \"{v}\"")));
    }
}
