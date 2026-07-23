use super::spec::{
    self, AppPolicy, BranchPolicy, FeaturePolicy, InstallRoute, InstallVariant, Interaction,
    Invocation, PathEffect, Platform, QuickstartHandoff, RouteId, RustPolicy,
};
use std::path::Path;

const EXPECTED_ROUTE_IDS: [RouteId; 4] = [
    RouteId::UnixFast,
    RouteId::UnixGuided,
    RouteId::WindowsPrebuilt,
    RouteId::AdvancedSource,
];

fn route_title(id: RouteId) -> &'static str {
    match id {
        RouteId::UnixFast => "Unix fast path",
        RouteId::UnixGuided => "Unix guided path",
        RouteId::WindowsPrebuilt => "Windows prebuilt path",
        RouteId::AdvancedSource => "Advanced source path",
    }
}

fn route_anchor(id: RouteId) -> &'static str {
    match id {
        RouteId::UnixFast => "unix-fast",
        RouteId::UnixGuided => "unix-guided",
        RouteId::WindowsPrebuilt => "windows-prebuilt",
        RouteId::AdvancedSource => "advanced-source",
    }
}

fn route_command(variant: &InstallVariant) -> anyhow::Result<String> {
    match (variant.platform, variant.invocation) {
        (Platform::Unix, Invocation::PipedScript)
        | (Platform::Unix, Invocation::InteractiveScript)
        | (Platform::Unix, Invocation::SourceScript) => Ok(format!(
            "```sh\n{}\n```",
            variant.invocation.command().unwrap_or_default()
        )),
        (Platform::Windows, Invocation::ManualPrebuilt) => Ok(
            "Use the idempotent PowerShell block in the [Windows setup guide](../setup/windows.md)."
                .to_owned(),
        ),
        (Platform::Windows, Invocation::CargoInstall) => Ok(format!(
            "```powershell\n{}\n```",
            variant.invocation.command().unwrap_or_default()
        )),
        _ => anyhow::bail!("unsupported platform and invocation combination"),
    }
}

fn interaction_sentence(interaction: Interaction) -> &'static str {
    match interaction {
        Interaction::NonInteractive => "This route is noninteractive and does not open a picker.",
        Interaction::Guided => "This route is guided and may offer supported choices.",
    }
}

fn branch_sentence(policy: BranchPolicy) -> &'static str {
    match policy {
        BranchPolicy::PreferPrebuiltThenSource => {
            "It prefers a matching prebuilt binary and falls back to a source build when needed."
        }
        BranchPolicy::UserChoice => {
            "On supported targets, it offers prebuilt or source installation."
        }
        BranchPolicy::PrebuiltOnly => "This route installs a prebuilt release archive.",
        BranchPolicy::SourceOnly => "This route always builds from source.",
    }
}

fn app_sentence(policy: AppPolicy) -> String {
    match policy {
        AppPolicy::ArchiveOptional(apps) => format!(
            "The archive may also contain {}; availability depends on the release asset.",
            apps.join(", ")
        ),
        AppPolicy::SourceDefaultsSelectable(apps) => format!(
            "Source installation selects {} by default and lets you change the app selection.",
            apps.join(", ")
        ),
        AppPolicy::ArchiveOptionalOrSourceDefaultsSelectable(apps) => format!(
            "A prebuilt archive may contain {}; a source choice selects it by default and allows app selection.",
            apps.join(", ")
        ),
        AppPolicy::ArchiveOptionalOrSourceDefaults(apps) => format!(
            "A prebuilt archive may contain {}; a noninteractive source fallback installs the default app without opening a picker.",
            apps.join(", ")
        ),
        AppPolicy::CoreOnly => {
            "This command installs the core `zeroclaw` binary; it does not install optional apps."
                .to_owned()
        }
    }
}

fn feature_sentence(policy: FeaturePolicy) -> &'static str {
    match policy {
        FeaturePolicy::Selectable => {
            "The source path also lets you select optional Cargo features."
        }
        FeaturePolicy::Fixed => "The command uses a fixed feature set.",
    }
}

fn rust_sentence(policy: RustPolicy) -> &'static str {
    match policy {
        RustPolicy::NotRequired => "This prebuilt route does not require a Rust toolchain.",
        RustPolicy::BootstrapIfMissing => {
            "If a source build is needed, the installer can bootstrap Rust when it is missing."
        }
        RustPolicy::RequiredAheadOfTime => {
            "A Rust toolchain is required before using this command."
        }
    }
}

fn path_sentence(platform: Platform, effect: PathEffect) -> anyhow::Result<&'static str> {
    match (platform, effect) {
        (Platform::Unix, PathEffect::ProfileUpdateRequiresReload) => Ok(
            "On Unix, the installer updates the shell profile when allowed; reload the parent shell before relying on the new PATH.",
        ),
        (Platform::Windows, PathEffect::CurrentAndFutureShell) => Ok(
            "On Windows, the PowerShell path updates the current process and the persistent user PATH.",
        ),
        (Platform::Windows, PathEffect::NoInstallerChange) => Ok(
            "This Cargo command does not edit PATH; make sure Cargo's bin directory is already available in your shell.",
        ),
        _ => anyhow::bail!("unsupported platform-specific PATH effect"),
    }
}

fn handoff_sentence(variant: &InstallVariant) -> String {
    match variant.handoff {
        QuickstartHandoff::PrintCommand => format!(
            "The installer skips setup and prints `{}` as the next step.",
            variant.quickstart_command
        ),
        QuickstartHandoff::OfferCliOrBrowser => format!(
            "For an unconfigured install, it offers `{}` or browser-based Quickstart.",
            variant.quickstart_command
        ),
        QuickstartHandoff::RunAutomatically => format!(
            "The PowerShell block finishes by running `{}` automatically.",
            variant.quickstart_command
        ),
        QuickstartHandoff::RunExplicitly => {
            format!("After installation, run `{}`.", variant.quickstart_command)
        }
    }
}

fn render_variant(variant: &InstallVariant) -> anyhow::Result<String> {
    let paragraphs = [
        route_command(variant)?,
        interaction_sentence(variant.interaction).to_owned(),
        branch_sentence(variant.branch).to_owned(),
        app_sentence(variant.apps),
        feature_sentence(variant.features).to_owned(),
        rust_sentence(variant.rust).to_owned(),
        path_sentence(variant.platform, variant.path_effect)?.to_owned(),
        handoff_sentence(variant),
    ];
    Ok(paragraphs.join("\n\n"))
}

fn render_route(route: &InstallRoute) -> anyhow::Result<String> {
    let body = if route.variants.len() == 1 {
        render_variant(&route.variants[0])?
    } else {
        let mut variants = Vec::new();
        for variant in route.variants {
            let platform = match variant.platform {
                Platform::Unix => "#### Unix",
                Platform::Windows => "#### Windows",
            };
            variants.push(format!("{platform}\n\n{}", render_variant(variant)?));
        }
        variants.join("\n\n")
    };

    Ok(format!("### {}\n\n{}\n", route_title(route.id), body))
}

fn render_routes(routes: &[InstallRoute]) -> anyhow::Result<String> {
    let actual: Vec<_> = routes.iter().map(|route| route.id).collect();
    anyhow::ensure!(
        actual.as_slice() == EXPECTED_ROUTE_IDS.as_slice(),
        "canonical install routes must contain each supported route exactly once in presentation order"
    );
    anyhow::ensure!(
        routes == spec::install_routes()?.as_slice(),
        "documented install routes must match every canonical platform contract"
    );

    let mut out =
        String::from("<!-- Generated by `cargo generate installers`; do not edit. -->\n\n");
    for (index, route) in routes.iter().enumerate() {
        if route.id == RouteId::UnixFast {
            out.push_str("<!-- ANCHOR: linux -->\n");
        }
        let anchor = route_anchor(route.id);
        out.push_str(&format!("<!-- ANCHOR: {anchor} -->\n"));
        out.push_str(&render_route(route)?);
        out.push_str(&format!("<!-- ANCHOR_END: {anchor} -->\n"));
        if route.id == RouteId::UnixGuided {
            out.push_str("<!-- ANCHOR_END: linux -->\n");
        }
        if index + 1 < routes.len() {
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn render_markdown() -> anyhow::Result<String> {
    render_routes(&spec::install_routes()?)
}

const WINDOWS_PREBUILT_ZONE: &str = "windows-prebuilt-powershell";

fn windows_begin() -> String {
    format!(
        "<!-- >>> generated:{WINDOWS_PREBUILT_ZONE} by `cargo generate installers` - do not edit <<< -->"
    )
}

fn windows_end() -> String {
    format!("<!-- >>> end generated:{WINDOWS_PREBUILT_ZONE} <<< -->")
}

fn render_windows_prebuilt_block(routes: &[InstallRoute]) -> anyhow::Result<String> {
    let route = routes
        .iter()
        .find(|route| route.id == RouteId::WindowsPrebuilt)
        .ok_or_else(|| anyhow::Error::msg("missing canonical Windows prebuilt route"))?;
    let variant = route.variant(Platform::Windows)?;
    anyhow::ensure!(
        matches!(variant.invocation, Invocation::ManualPrebuilt)
            && variant.interaction == Interaction::NonInteractive
            && variant.branch == BranchPolicy::PrebuiltOnly
            && matches!(variant.apps, AppPolicy::ArchiveOptional(_))
            && variant.features == FeaturePolicy::Fixed
            && variant.rust == RustPolicy::NotRequired
            && variant.path_effect == PathEffect::CurrentAndFutureShell
            && variant.handoff == QuickstartHandoff::RunAutomatically,
        "Windows prebuilt route no longer matches the maintained PowerShell boundary"
    );

    let quickstart_subcommand = variant
        .quickstart_command
        .strip_prefix("zeroclaw ")
        .filter(|subcommand| !subcommand.is_empty() && !subcommand.contains(char::is_whitespace))
        .ok_or_else(|| anyhow::Error::msg("Windows Quickstart command must be one subcommand"))?;

    Ok(r#"```powershell
# Idempotent: re-running this block is a no-op when zeroclaw is already
# installed at the latest release and on the user PATH. After a release
# bumps, the version check fails and the install side runs again.
$ver = (Invoke-RestMethod 'https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest').tag_name.TrimStart('v')
$dst = "$env:USERPROFILE\.zeroclaw\bin"
$exe = "$dst\zeroclaw.exe"

$current = if (Test-Path $exe) {
    ((& $exe --version 2>$null) | Select-String -Pattern '\d+\.\d+\.\d+').Matches.Value
} else { '' }

if ($current -ne $ver) {
    $url = "https://github.com/zeroclaw-labs/zeroclaw/releases/download/v$ver/zeroclaw-x86_64-pc-windows-msvc.zip"
    New-Item -ItemType Directory -Force -Path $dst | Out-Null
    Invoke-WebRequest -Uri $url -OutFile "$env:TEMP\zeroclaw.zip" -UseBasicParsing
    Expand-Archive -Force -Path "$env:TEMP\zeroclaw.zip" -DestinationPath $dst
}

$environment = [Environment]
$userPath = $environment::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dst) {
    $environment::SetEnvironmentVariable('Path', "$dst;$userPath", 'User')
}
if (($env:Path -split ';') -notcontains $dst) {
    $env:Path = "$dst;$env:Path"
}

& $exe __QUICKSTART_SUBCOMMAND__
```"#
        .replace("__QUICKSTART_SUBCOMMAND__", quickstart_subcommand))
}

pub fn render_windows_guide(_root: &Path, current: &str) -> anyhow::Result<String> {
    let begin = windows_begin();
    let end = windows_end();
    let begin_count = current.match_indices(&begin).count();
    let end_count = current.match_indices(&end).count();
    anyhow::ensure!(
        begin_count == 1 && end_count == 1,
        "Windows prebuilt guide must contain exactly one generated sentinel pair"
    );
    let begin_at = current
        .find(&begin)
        .ok_or_else(|| anyhow::Error::msg("missing Windows prebuilt begin sentinel"))?;
    let after_begin = begin_at + begin.len();
    let end_at = current
        .find(&end)
        .ok_or_else(|| anyhow::Error::msg("missing Windows prebuilt end sentinel"))?;
    anyhow::ensure!(
        after_begin < end_at,
        "Windows prebuilt sentinels are out of order"
    );

    let mut rendered = String::new();
    rendered.push_str(&current[..after_begin]);
    rendered.push('\n');
    rendered.push_str(&render_windows_prebuilt_block(&spec::install_routes()?)?);
    rendered.push('\n');
    rendered.push_str(&current[end_at..]);
    Ok(rendered)
}

pub fn render_file(_root: &Path, _current: &str) -> anyhow::Result<String> {
    render_markdown()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn markdown_contains_every_user_route_and_stable_anchor() {
        let markdown = render_markdown().unwrap();
        for (heading, anchor) in [
            ("### Unix fast path", "unix-fast"),
            ("### Unix guided path", "unix-guided"),
            ("### Windows prebuilt path", "windows-prebuilt"),
            ("### Advanced source path", "advanced-source"),
        ] {
            assert!(markdown.contains(heading), "missing {heading}");
            assert!(
                markdown.contains(&format!("<!-- ANCHOR: {anchor} -->")),
                "missing {anchor} anchor"
            );
            assert!(
                markdown.contains(&format!("<!-- ANCHOR_END: {anchor} -->")),
                "missing {anchor} anchor end"
            );
        }
    }

    #[test]
    fn markdown_projects_route_behavior() {
        let markdown = render_markdown().unwrap();
        assert!(markdown.contains("noninteractive"));
        assert!(markdown.contains("prints `zeroclaw quickstart`"));
        assert!(markdown.contains("offers `zeroclaw quickstart` or browser-based Quickstart"));
        assert!(markdown.contains("reload the parent shell"));
        assert!(markdown.contains("cargo install --locked --path ."));
        assert!(markdown.contains("does not edit PATH"));
        assert!(markdown.contains("running `zeroclaw quickstart` automatically"));
        assert!(markdown.contains("lets you change the app selection"));
    }

    #[test]
    fn markdown_preserves_balanced_legacy_linux_anchor() {
        let markdown = render_markdown().unwrap();
        assert_eq!(markdown.matches("<!-- ANCHOR: linux -->").count(), 1);
        assert_eq!(markdown.matches("<!-- ANCHOR_END: linux -->").count(), 1);
        assert!(
            markdown.find("<!-- ANCHOR: linux -->") < markdown.find("<!-- ANCHOR_END: linux -->")
        );
    }

    #[test]
    fn invalid_route_sets_are_rejected() {
        let routes = spec::install_routes().unwrap();

        let mut missing = routes.clone();
        missing.pop();
        assert!(render_routes(&missing).is_err());

        let mut duplicate = routes.clone();
        duplicate[3] = duplicate[0];
        assert!(render_routes(&duplicate).is_err());

        let mut reordered = routes;
        reordered.swap(0, 1);
        assert!(render_routes(&reordered).is_err());
    }

    #[test]
    fn invocation_and_interaction_drift_are_rejected() {
        let mut wrong_invocation = spec::install_routes().unwrap();
        mutate_variant(
            &mut wrong_invocation,
            RouteId::AdvancedSource,
            Platform::Windows,
            |variant| variant.invocation = Invocation::SourceScript,
        );
        assert!(render_routes(&wrong_invocation).is_err());

        let mut wrong_interaction = spec::install_routes().unwrap();
        mutate_variant(
            &mut wrong_interaction,
            RouteId::WindowsPrebuilt,
            Platform::Windows,
            |variant| variant.interaction = Interaction::Guided,
        );
        assert!(render_routes(&wrong_interaction).is_err());
    }

    #[test]
    fn windows_prebuilt_route_rejects_handwritten_guide_drift() {
        let guide = std::fs::read_to_string(root().join("docs/book/src/setup/windows.md")).unwrap();
        let mutations = [
            ("install guard", "if ($current -ne $ver) {", "if ($false) {"),
            (
                "archive extraction",
                "    Expand-Archive -Force -Path \"$env:TEMP\\zeroclaw.zip\" -DestinationPath $dst",
                "    # archive extraction removed",
            ),
            (
                "persistent PATH",
                "    $environment::SetEnvironmentVariable('Path', \"$dst;$userPath\", 'User')",
                "    # persistent PATH update removed",
            ),
            (
                "current PATH",
                "    $env:Path = \"$dst;$env:Path\"",
                "    # current PATH update removed",
            ),
            (
                "automatic Quickstart",
                "& $exe quickstart",
                "Write-Host 'Run zeroclaw quickstart later'",
            ),
        ];

        for (name, current, divergent) in mutations {
            let mismatch = guide.replacen(current, divergent, 1);
            assert_ne!(guide, mismatch, "{name} fixture must modify windows.md");
            assert_eq!(
                guide,
                render_windows_guide(&root(), &mismatch).unwrap(),
                "installer generation must restore Windows {name} drift"
            );
        }
    }

    #[test]
    fn windows_prebuilt_guide_is_fresh_and_idempotent() {
        let guide = std::fs::read_to_string(root().join("docs/book/src/setup/windows.md")).unwrap();
        let once = render_windows_guide(&root(), &guide).unwrap();
        let twice = render_windows_guide(&root(), &once).unwrap();
        assert_eq!(
            guide, once,
            "checked-in Windows guide must be freshly rendered"
        );
        assert_eq!(once, twice, "Windows guide render must be idempotent");
    }

    #[test]
    fn windows_prebuilt_guide_rejects_duplicate_sentinels() {
        let guide = std::fs::read_to_string(root().join("docs/book/src/setup/windows.md")).unwrap();
        let duplicate = format!("{guide}\n{}\n{}", windows_begin(), windows_end());
        assert!(render_windows_guide(&root(), &duplicate).is_err());
    }

    fn mutate_variant(
        routes: &mut [InstallRoute],
        id: RouteId,
        platform: Platform,
        mutate: impl FnOnce(&mut InstallVariant),
    ) {
        let route = routes.iter_mut().find(|route| route.id == id).unwrap();
        let mut variants = route.variants.to_vec();
        mutate(
            variants
                .iter_mut()
                .find(|variant| variant.platform == platform)
                .unwrap(),
        );
        route.variants = Box::leak(variants.into_boxed_slice());
    }

    #[test]
    fn rendering_is_deterministic_and_idempotent() {
        assert_eq!(render_markdown().unwrap(), render_markdown().unwrap());

        let current =
            std::fs::read_to_string(root().join("docs/book/src/_snippets/install.md")).unwrap();
        let once = render_file(&root(), &current).unwrap();
        let twice = render_file(&root(), &once).unwrap();
        assert_eq!(once, twice);
    }
}
