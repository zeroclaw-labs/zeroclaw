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
