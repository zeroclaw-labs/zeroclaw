//! Nix flake renderer. The flake is the one packaged surface that rebuilds from
//! source per-user, so it must expose feature selection (overridable), not a
//! fixed set. We generate a sentinel-delimited zone defining the zeroclaw +
//! zerocode packages with explicit per-package defaults. The zeroclaw default
//! is the canonical lean Dist feature list; the zerocode default is resolved
//! from the `zerocode` package's own feature set (currently empty).

use super::spec::{self, Selection};
use std::path::Path;

fn begin(zone: &str) -> String {
    format!("        # >>> generated:{zone} by `cargo generate installers` - do not edit <<<")
}
fn end(zone: &str) -> String {
    format!("        # >>> end generated:{zone} <<<")
}

const ZONE: &str = "flake-packages";

/// Resolve the zerocode package's own selectable features (excluding the
/// `default` entry) from canonical `cargo_metadata`. Source of truth is
/// `apps/zerocode/Cargo.toml [features]`; currently empty.
fn resolve_zerocode_features(root: &Path) -> anyhow::Result<Vec<String>> {
    let meta = cargo_metadata::MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let pkg = meta
        .packages
        .iter()
        .find(|p| p.name == "zerocode")
        .ok_or_else(|| anyhow::Error::msg("workspace has no `zerocode` package"))?;
    let mut features: Vec<String> = pkg
        .features
        .keys()
        .filter(|f| *f != "default")
        .cloned()
        .collect();
    features.sort();
    Ok(features)
}

/// Render the generated package-definition zone body: a Rust package builder
/// taking an explicit per-package feature list, with the Dist feature list
/// (zeroclaw) and the zerocode package's own features as the overridable
/// defaults. Indented to sit inside the per-system `in {` block of the flake.
pub fn render_zone(root: &Path) -> anyhow::Result<String> {
    let version = spec::resolve_version(root)?;
    let dist = spec::resolve_feature_list(root, &Selection::Dist)?;
    let feature_list = dist
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let zerocode_features = resolve_zerocode_features(root)?;
    let zerocode_feature_list = zerocode_features
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(" ");

    // Nix: a function over an explicit feature list, building each binary with
    // --no-default-features --features <list>. Callers pass their package
    // default; users override with `.override { features = [ ... ]; }`.
    let lines = [
        "        # Default feature sets: zeroclaw uses canonical lean Dist,".to_string(),
        "        # zerocode uses its own package features (currently empty).".to_string(),
        "        # Override per-package, e.g. `packages.zeroclaw.override { features = [ ... ]; }`.".to_string(),
        format!("        zeroclawDefaultFeatures = [ {feature_list} ];"),
        format!("        zerocodeDefaultFeatures = [ {zerocode_feature_list} ];"),
        "        buildZeroclaw = { pname, cargoPkg, features }:".to_string(),
        "          (pkgs.makeRustPlatform {".to_string(),
        "            cargo = rustToolchain;".to_string(),
        "            rustc = rustToolchain;".to_string(),
        "          }).buildRustPackage {".to_string(),
        "            inherit pname;".to_string(),
        format!("            version = \"{version}\";"),
        "            src = ./.;".to_string(),
        "            cargoLock = {".to_string(),
        "              lockFile = ./Cargo.lock;".to_string(),
        "              outputHashes = builtins.fromJSON (builtins.readFile ./nix/hashes.json);"
            .to_string(),
        "            };".to_string(),
        "            cargoBuildFlags =".to_string(),
        "              [ \"-p\" cargoPkg \"--no-default-features\" ]".to_string(),
        "              ++ pkgs.lib.optionals (features != [])".to_string(),
        "                [ \"--features\" (pkgs.lib.concatStringsSep \",\" features) ];"
            .to_string(),
        "            doCheck = false;".to_string(),
        "            buildInputs = [ pkgs.stdenv.cc.cc ];".to_string(),
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
    let begin_at = current.find(&b).ok_or_else(|| {
        anyhow::Error::msg(format!("flake.nix missing generated:{ZONE} BEGIN sentinel"))
    })?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..].find(&e).ok_or_else(|| {
        anyhow::Error::msg(format!("flake.nix missing generated:{ZONE} END sentinel"))
    })?;
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
            "zeroclaw default feature list present"
        );
        assert!(
            z.contains("zerocodeDefaultFeatures"),
            "zerocode default feature list present"
        );
        assert!(
            z.contains("buildZeroclaw = { pname, cargoPkg, features }:"),
            "features parameter is explicit per-package"
        );
        assert!(
            z.contains("buildRustPackage"),
            "real package build, not just toolchain"
        );
    }

    #[test]
    fn zone_default_is_lean_dist() {
        let z = render_zone(&root()).unwrap();
        let zeroclaw_line = z
            .lines()
            .find(|l| l.contains("zeroclawDefaultFeatures"))
            .expect("zeroclaw defaults line present");
        for feature in spec::resolve_feature_list(&root(), &Selection::Dist).unwrap() {
            assert!(
                zeroclaw_line.contains(&format!("\"{feature}\"")),
                "dist feature {feature} not rendered"
            );
        }
        for feature in spec::features_outside_dist(&root()).unwrap() {
            assert!(
                !zeroclaw_line.contains(&format!("\"{feature}\"")),
                "{feature} leaked into lean dist"
            );
        }
    }

    #[test]
    fn zerocode_does_not_inherit_zeroclaw_features() {
        let z = render_zone(&root()).unwrap();
        let zerocode_line = z
            .lines()
            .find(|l| l.contains("zerocodeDefaultFeatures"))
            .expect("zerocode defaults line present");
        for feature in spec::resolve_feature_list(&root(), &Selection::Dist).unwrap() {
            assert!(
                !zerocode_line.contains(&format!("\"{feature}\"")),
                "zeroclaw dist feature {feature} leaked into zerocode defaults"
            );
        }
        let expected = resolve_zerocode_features(&root()).unwrap();
        for feature in expected {
            assert!(
                zerocode_line.contains(&format!("\"{feature}\"")),
                "zerocode feature {feature} missing from defaults"
            );
        }
    }

    #[test]
    fn zone_version_from_workspace() {
        let v = spec::resolve_version(&root()).unwrap();
        let z = render_zone(&root()).unwrap();
        assert!(z.contains(&format!("version = \"{v}\"")));
    }

    #[test]
    fn zone_loads_hashes_via_nix_expression() {
        let z = render_zone(&root()).unwrap();
        assert!(
            z.contains("builtins.fromJSON"),
            "hashes loaded at eval time, not baked at generate time"
        );
        assert!(z.contains("outputHashes"), "outputHashes attribute present");
        assert!(z.contains("buildInputs"), "buildInputs attribute present");
    }
}
