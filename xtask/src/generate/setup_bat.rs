//! setup.bat renderer. The hand-written imperative glue (toolchain bootstrap,
//! PATH, copy, quickstart, prebuilt download) stays in setup.bat; only the
//! drift-prone data lives in sentinel-delimited regions this renderer owns.
//! setup.bat has three such zones: build-mode routing, per-mode presets, and
//! post-install guidance.

use super::spec::{self, QUICKSTART_COMMAND, Selection, ZEROCODE_APP};
use std::path::Path;

/// A named generated zone, delimited by id-tagged sentinels so multiple regions
/// can coexist in one file and the splicer targets each precisely.
fn begin(id: &str) -> String {
    format!(":: >>> generated:{id} by `cargo generate installers` - do not edit <<<")
}
fn end(id: &str) -> String {
    format!(":: >>> end generated:{id} <<<")
}

const ZONE_MENU: &str = "menu";
const ZONE_PRESETS: &str = "presets";
const ZONE_POST_INSTALL: &str = "post-install";

/// Render the menu/routing zone body: non-interactive MODE routing plus the
/// interactive numbered menu, walked from `Selection::menu()`. Option 1 is the
/// hand-written prebuilt path; generated options start at 2.
fn render_menu(_manifest_dir: &Path) -> String {
    let menu = Selection::menu();
    let mut out = String::new();
    out.push_str(":choose_mode\n");
    out.push_str("if \"%MODE%\"==\"prebuilt\" goto :install_prebuilt\n");
    for sel in &menu {
        out.push_str(&format!(
            "if \"%MODE%\"==\"{id}\" goto :build_{id}\n",
            id = sel.id()
        ));
    }
    out.push('\n');
    out.push_str("echo %BOLD%[2/5] Choose installation method:%RESET%\n");
    out.push_str("echo.\n");
    out.push_str("echo   1) Prebuilt binary - Download pre-compiled release (fastest)\n");
    for (i, sel) in menu.iter().enumerate() {
        out.push_str(&format!(
            "echo   {n}) {id} build - {desc}\n",
            n = i + 2,
            id = sel.id(),
            desc = sel.describe()
        ));
    }
    let last = menu.len() + 1;
    out.push_str("echo.\n");
    out.push_str(&format!(
        "set /p \"CHOICE=  Select [1-{last}] (default: 1): \"\n"
    ));
    out.push_str("if \"%CHOICE%\"==\"\" set \"CHOICE=1\"\n");
    out.push_str("if \"%CHOICE%\"==\"1\" goto :install_prebuilt\n");
    for (i, sel) in menu.iter().enumerate() {
        out.push_str(&format!(
            "if \"%CHOICE%\"==\"{n}\" goto :build_{id}\n",
            n = i + 2,
            id = sel.id()
        ));
    }
    out.push_str(&format!(
        "echo   %RED%Invalid choice. Please enter 1-{last}.%RESET%\n"
    ));
    out.push_str("goto :choose_mode");
    out
}

/// Render the presets zone body: one `:build_<id>` per menu selection, FEATURES
/// from the canonical resolved flags, description from the selection.
fn render_presets(manifest_dir: &Path) -> anyhow::Result<String> {
    let mut out = String::new();
    let menu = Selection::menu();
    for (i, sel) in menu.iter().enumerate() {
        let flags = spec::resolve_flags(manifest_dir, sel)?;
        out.push_str(&format!(":build_{}\n", sel.id()));
        out.push_str(&format!("set \"FEATURES={flags}\"\n"));
        out.push_str(&format!(
            "set \"BUILD_DESC={} ({})\"\n",
            sel.id(),
            sel.describe()
        ));
        out.push_str("goto :do_build");
        if i + 1 < menu.len() {
            out.push_str("\n\n");
        }
    }
    Ok(out)
}

/// setup.bat is its own interactive installer surface. Its PATH and branch
/// behavior intentionally do not inherit the manual PowerShell route contract;
/// only shared command and app identities come from the canonical spec.
fn render_post_install() -> String {
    format!(
        "echo   Next steps:\n\
if /I \"%INSTALL_ROUTE%\"==\"prebuilt\" (\n\
echo     1. PATH is ready in this terminal and future terminals\n\
echo     2. Run: {QUICKSTART_COMMAND}\n\
echo     3. Configure a model provider during Quickstart\n\
echo     4. Launch the TUI when installed: {ZEROCODE_APP}\n\
) else (\n\
echo     1. PATH is ready in this terminal and future terminals\n\
if /I \"%MODE%\"==\"minimal\" (\n\
echo     2. Minimal build excludes quickstart ^({QUICKSTART_COMMAND} is unavailable^)\n\
echo     3. Configure model providers with the supported config surface\n\
echo     4. Use reduced CLI path: zeroclaw agent --message \"Hello\"\n\
) else (\n\
echo     2. Run: {QUICKSTART_COMMAND}\n\
echo     3. Configure a model provider during Quickstart\n\
echo     4. Launch the TUI when installed: {ZEROCODE_APP}\n\
)\n\
)"
    )
}

/// The recommended default selection's id - what the prebuilt-fallback path
/// jumps to. Derived, not hardcoded: `Dist` is the recommended build.
pub fn fallback_build_id() -> &'static str {
    Selection::Dist.id()
}

/// Splice all three generated zones into the current setup.bat, leaving all
/// hand-written glue untouched. Errors if any zone's sentinels are missing.
pub fn render_file(manifest_dir: &Path, current: &str) -> anyhow::Result<String> {
    let with_menu = splice(current, ZONE_MENU, &render_menu(manifest_dir))?;
    let with_presets = splice(&with_menu, ZONE_PRESETS, &render_presets(manifest_dir)?)?;
    splice(&with_presets, ZONE_POST_INSTALL, &render_post_install())
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let b = begin(zone);
    let e = end(zone);
    let begin_at = current.find(&b).ok_or_else(|| {
        anyhow::Error::msg(format!("setup.bat missing generated:{zone} BEGIN sentinel"))
    })?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..].find(&e).ok_or_else(|| {
        anyhow::Error::msg(format!("setup.bat missing generated:{zone} END sentinel"))
    })?;
    let end_at = after_begin + end_rel;

    let mut out = String::new();
    out.push_str(&current[..after_begin]);
    out.push('\n');
    out.push_str(body);
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
    fn presets_have_a_label_per_menu_selection() {
        let p = render_presets(&root()).unwrap();
        for sel in Selection::menu() {
            assert!(
                p.contains(&format!(":build_{}", sel.id())),
                "missing :build_{}",
                sel.id()
            );
        }
    }

    #[test]
    fn dist_preset_ships_lean_release_channels() {
        let presets = render_presets(&root()).unwrap();
        let (_, dist_and_rest) = presets.split_once(":build_dist").unwrap();
        let (dist, _) = dist_and_rest.split_once("goto :do_build").unwrap();
        assert!(dist.contains("channel-matrix"));
        assert!(dist.contains("whatsapp-web"));
        assert!(!dist.contains("channel-slack"));
    }

    #[test]
    fn real_setup_bat_help_describes_dist_as_lean() {
        let setup = std::fs::read_to_string(root().join("setup.bat")).unwrap();
        assert!(setup.contains("--dist        Build lean standard distribution"));
        assert!(!setup.contains("--dist        Build all channels"));
    }

    #[test]
    fn menu_routes_prebuilt_and_every_selection() {
        let m = render_menu(&root());
        assert!(m.contains("if \"%MODE%\"==\"prebuilt\" goto :install_prebuilt"));
        for sel in Selection::menu() {
            assert!(m.contains(&format!("goto :build_{}", sel.id())));
        }
    }

    #[test]
    fn splice_targets_named_zone_only() {
        let cur = format!(
            "A\n{}\nOLD\n{}\nB\n{}\nKEEP\n{}\nC\n",
            begin(ZONE_MENU),
            end(ZONE_MENU),
            begin(ZONE_PRESETS),
            end(ZONE_PRESETS)
        );
        let out = splice(&cur, ZONE_MENU, "NEW").unwrap();
        assert!(out.contains("NEW") && !out.contains("OLD"));
        assert!(out.contains("KEEP"), "other zone untouched");
    }

    #[test]
    fn splice_errors_without_zone() {
        assert!(splice("nothing", ZONE_MENU, "x").is_err());
    }

    #[test]
    fn fallback_is_recommended_dist() {
        assert_eq!(fallback_build_id(), "dist");
    }

    #[test]
    fn windows_post_install_preserves_setup_bat_runtime_contract() {
        let post_install = render_post_install();
        assert!(post_install.contains("PATH is ready in this terminal and future terminals"));
        assert!(!post_install.contains("Restart your terminal"));
        assert!(post_install.contains(&format!("Run: {QUICKSTART_COMMAND}")));
        assert!(post_install.contains(&format!("installed: {ZEROCODE_APP}")));
    }

    #[test]
    fn real_setup_bat_contains_post_install_zone() {
        let current = std::fs::read_to_string(root().join("setup.bat")).unwrap();
        let once = render_file(&root(), &current).unwrap();
        let twice = render_file(&root(), &once).unwrap();
        assert!(once.contains("generated:post-install"));
        assert_eq!(once, twice, "render must be idempotent");
    }
}
