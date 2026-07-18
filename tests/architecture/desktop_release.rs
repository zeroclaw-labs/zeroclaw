//! Release invariant: the macOS desktop sidecar must contain the dashboard.

use std::{fs, path::Path};

#[test]
fn macos_desktop_sidecar_embeds_the_web_artifact() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/release-stable-manual.yml"))
        .expect("release workflow should be readable");
    let macos_job = workflow
        .split_once("\n  build-desktop:\n")
        .and_then(|(_, rest)| rest.split_once("\n  build-desktop-linux:\n"))
        .map(|(job, _)| job)
        .expect("macOS desktop release job should exist");

    assert!(
        macos_job.contains("needs: [validate, web]"),
        "macOS desktop release must wait for the canonical web-dist artifact"
    );
    assert!(
        macos_job.contains("uses: actions/download-artifact@")
            && macos_job.contains("name: web-dist")
            && macos_job.contains("path: web/dist/"),
        "macOS desktop release must restore web-dist at the embedded-web source path"
    );
    assert!(
        macos_job
            .contains("prepare-kernel.sh --target universal-apple-darwin --features embedded-web"),
        "macOS desktop kernel must enable the existing embedded-web Cargo feature"
    );
    let stage_position = macos_job
        .find("- name: Stage bundled kernel sidecar (universal)")
        .expect("macOS desktop release should stage its sidecar");
    let smoke_position = macos_job
        .find("- name: Smoke test embedded dashboard from an empty directory")
        .expect("macOS desktop release should smoke test the staged sidecar");
    let signing_position = macos_job
        .find("- name: Enable macOS signing")
        .expect("macOS desktop release should configure signing");
    assert!(
        stage_position < smoke_position && smoke_position < signing_position,
        "embedded dashboard smoke test must run immediately after sidecar staging"
    );

    let smoke_step = &macos_job[smoke_position..signing_position];
    assert!(
        smoke_step.contains("cd \"$smoke_cwd\"")
            && smoke_step.contains("--config-dir \"$config_dir\"")
            && smoke_step.contains("HOME=\"$smoke_home\"")
            && smoke_step.contains("XDG_DATA_HOME=\"$xdg_data_home\"")
            && smoke_step.contains("host=\"127.0.0.1\"")
            && smoke_step.contains("port=\"42618\"")
            && smoke_step.contains("origin=\"http://$host:$port\"")
            && smoke_step.contains("--host \"$host\" --port \"$port\""),
        "embedded dashboard smoke test must launch from an empty cwd with isolated config"
    );
    assert!(
        smoke_step.contains("curl --fail --silent --connect-timeout 1 --max-time 2")
            && smoke_step.contains("\"$origin/\"")
            && smoke_step.contains("id=\"root\""),
        "embedded dashboard smoke test must require a successful SPA response"
    );

    let prepare = fs::read_to_string(root.join("scripts/desktop/prepare-kernel.sh"))
        .expect("desktop kernel preparation script should be readable");
    assert!(
        prepare.lines().any(|line| {
            line.contains("cargo build") && line.contains("--features \"$FEATURES\"")
        }),
        "prepare-kernel.sh must forward the requested Cargo features"
    );
}
