//! Desktop invariants: CI runs the desktop app's unit tests, and every
//! desktop release sidecar must contain the dashboard.

use std::{fs, path::Path};

#[test]
fn windows_desktop_manifest_selects_common_controls_v6() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("apps/tauri/windows/app.manifest"))
        .expect("Windows desktop manifest should be readable");

    for required in [
        "name=\"Microsoft.Windows.Common-Controls\"",
        "version=\"6.0.0.0\"",
        "publicKeyToken=\"6595b64144ccf1df\"",
    ] {
        assert!(
            manifest.contains(required),
            "Windows desktop manifest must select Common Controls v6: missing {required}"
        );
    }

    let build_script = fs::read_to_string(root.join("apps/tauri/build.rs"))
        .expect("Tauri build script should be readable");
    assert!(
        build_script.contains("app_manifest(include_str!(\"windows/app.manifest\"))"),
        "Tauri must embed the guarded Windows application manifest"
    );
}

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

#[test]
fn desktop_app_check_runs_the_desktop_unit_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/desktop-check.yml"))
        .expect("desktop app check workflow should be readable");
    let (triggers, jobs) = workflow
        .split_once("\njobs:\n")
        .expect("desktop app check should define jobs");

    assert!(
        triggers.contains("\n  pull_request:\n") && triggers.contains("- \"apps/tauri/**\""),
        "desktop app check must run on pull requests that change the desktop app"
    );
    assert!(
        jobs.contains("os: [macos-14, ubuntu-22.04, windows-latest]"),
        "desktop unit tests must run on macOS, Linux, and Windows"
    );
    assert!(
        !workflow.contains("continue-on-error"),
        "a failing desktop unit test must fail the desktop app check"
    );

    let test_step = jobs
        .split("\n      - ")
        .find(|step| step.starts_with("name: Test (zeroclaw-desktop)"))
        .expect("desktop app check must have a desktop unit-test step");
    assert!(
        test_step
            .lines()
            .any(|line| line.trim() == "run: cargo test --locked -p zeroclaw-desktop")
            && !test_step.contains("if:"),
        "the desktop unit-test step must run every desktop test on every platform"
    );
}

#[test]
fn desktop_dashboard_smoke_launches_like_a_fresh_install() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/desktop/smoke-dashboard.sh"))
        .expect("desktop dashboard smoke script should be readable");

    assert!(
        script.contains("cd \"$smoke_cwd\"")
            && script.contains("--config-dir \"$(native_path \"$config_dir\")\"")
            && script.contains("HOME=\"$smoke_home\"")
            && script.contains("XDG_DATA_HOME=\"$xdg_data_home\"")
            && script.contains("host=\"127.0.0.1\"")
            && script.contains("--host \"$host\" --port \"$port\""),
        "the dashboard smoke must launch from an empty cwd with isolated config"
    );
    assert!(
        script.contains("\"$origin/\"")
            && script.contains("[[ \"$status_code\" == \"200\" ]]")
            && script.contains("grep -Fq 'id=\"root\"'"),
        "the dashboard smoke must require a successful SPA response"
    );
}

#[test]
fn linux_and_windows_desktop_sidecars_embed_the_web_artifact() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/release-stable-manual.yml"))
        .expect("release workflow should be readable");
    let linux_job = workflow
        .split_once("\n  build-desktop-linux:\n")
        .and_then(|(_, rest)| rest.split_once("\n  build-desktop-windows:\n"))
        .map(|(job, _)| job)
        .expect("Linux desktop release job should exist");
    let windows_job = workflow
        .split_once("\n  build-desktop-windows:\n")
        .and_then(|(_, rest)| rest.split_once("\n  sbom:\n"))
        .map(|(job, _)| job)
        .expect("Windows desktop release job should exist");

    for (platform, job, triple, kernel) in [
        (
            "Linux",
            linux_job,
            "x86_64-unknown-linux-gnu",
            "zeroclaw-x86_64-unknown-linux-gnu",
        ),
        (
            "Windows",
            windows_job,
            "x86_64-pc-windows-msvc",
            "zeroclaw-x86_64-pc-windows-msvc.exe",
        ),
    ] {
        assert!(
            job.contains("needs: [validate, web]"),
            "{platform} desktop release must wait for the canonical web-dist artifact"
        );
        let restore = job
            .find("uses: actions/download-artifact@")
            .unwrap_or_else(|| panic!("{platform} desktop release must restore web-dist"));
        assert!(
            job[restore..].contains("name: web-dist") && job[restore..].contains("path: web/dist/"),
            "{platform} desktop release must restore web-dist at the embedded-web source path"
        );
        let stage = job
            .find(&format!(
                "prepare-kernel.sh --target {triple} --features embedded-web"
            ))
            .unwrap_or_else(|| {
                panic!("{platform} desktop kernel must enable the embedded-web Cargo feature")
            });
        let smoke = job
            .find(&format!(
                "scripts/desktop/smoke-dashboard.sh apps/tauri/binaries/{kernel}"
            ))
            .unwrap_or_else(|| {
                panic!("{platform} desktop release must smoke test the staged sidecar")
            });
        let bundle = job
            .find("- name: Build Tauri app (bundled kernel)")
            .unwrap_or_else(|| panic!("{platform} desktop release must bundle the sidecar"));
        assert!(
            restore < stage && stage < smoke && smoke < bundle,
            "{platform} desktop release must restore web-dist, stage, smoke test, then bundle"
        );
    }
}
