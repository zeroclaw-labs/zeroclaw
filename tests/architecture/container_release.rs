//! Release invariants for published container variants and scheduled scans.

use std::{fs, path::Path};

fn workflow(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(".github/workflows").join(name))
        .unwrap_or_else(|error| panic!("failed to read {name}: {error}"))
}

fn repository_file(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(name))
        .unwrap_or_else(|error| panic!("failed to read {name}: {error}"))
}

fn top_level_job<'a>(workflow: &'a str, name: &str) -> &'a str {
    let marker = format!("\n  {name}:\n");
    let (_, rest) = workflow
        .split_once(&marker)
        .unwrap_or_else(|| panic!("workflow must contain the {name} job"));
    let end = rest
        .match_indices("\n  ")
        .find_map(|(offset, _)| {
            let line = rest[offset + 1..].lines().next()?;
            (line.starts_with("  ")
                && !line.starts_with("    ")
                && !line.trim_start().starts_with('#')
                && line.trim_end().ends_with(':'))
            .then_some(offset)
        })
        .unwrap_or(rest.len());
    &rest[..end]
}

fn mount_option<'a>(mount: &'a str, names: &[&str]) -> Option<&'a str> {
    mount
        .split(',')
        .filter_map(|option| option.split_once('='))
        .find_map(|(name, value)| names.contains(&name).then_some(value))
}

fn cargo_cache_mounts(containerfile: &str) -> Vec<(&str, Option<&str>)> {
    containerfile
        .lines()
        .flat_map(|line| {
            line.split_whitespace().filter_map(move |token| {
                let mount = token.strip_prefix("--mount=")?;
                if mount_option(mount, &["type"]) != Some("cache") {
                    return None;
                }

                let target = mount_option(mount, &["target", "dst", "destination"])?;
                matches!(target, "/root/.cargo/registry" | "/root/.cargo/git")
                    .then(|| (line.trim(), mount_option(mount, &["sharing"])))
            })
        })
        .collect()
}

#[test]
fn manual_stable_release_calls_container_matrix_at_release_tag() {
    let release = workflow("release-stable-manual.yml");
    let matrix_job = top_level_job(&release, "docker-matrix");

    for required in [
        "needs: [validate, publish, docker]",
        "github.event_name == 'workflow_dispatch'",
        "needs.publish.result == 'success'",
        "needs.docker.result == 'success'",
        "uses: ./.github/workflows/docker-publish.yml",
        "release_ref: ${{ needs.validate.outputs.tag }}",
    ] {
        assert!(
            matrix_job.contains(required),
            "Docker matrix call is missing release invariant: {required}"
        );
    }
    assert!(
        !matrix_job.contains("secrets: inherit"),
        "Docker matrix call must not inherit unrelated release secrets"
    );
    let permissions = matrix_job
        .split_once("\n    permissions:\n")
        .expect("Docker matrix call must declare scoped permissions")
        .1
        .trim();
    assert_eq!(
        permissions,
        "contents: read\n      packages: write\n      id-token: write\n      security-events: write",
        "Docker matrix call permissions must remain minimal and complete"
    );

    let publisher = workflow("docker-publish.yml");
    assert!(
        publisher.contains("push:\n    tags:\n      - \"v*\"")
            && publisher.contains("workflow_call:")
            && publisher.contains("release_ref:")
            && publisher.contains("workflow_dispatch:"),
        "Docker Publish must keep tag-push, reusable, and manual entry points"
    );
    assert_eq!(
        publisher
            .matches("ref: ${{ inputs.release_ref || github.ref }}")
            .count(),
        2,
        "matrix resolution and image builds must use the requested immutable ref"
    );
}

#[test]
fn scheduled_trivy_verifies_published_tag_before_scan() {
    let scheduled = workflow("trivy-scheduled.yml");
    let scan_job = top_level_job(&scheduled, "scan");
    let preflight = scan_job
        .find("- name: Verify published image exists")
        .expect("scheduled Trivy must contain an image-existence preflight");
    let scan = scan_job
        .find("- name: Scan ${{ matrix.stem }} with Trivy")
        .expect("scheduled Trivy scan step must exist");

    assert!(
        preflight < scan,
        "scheduled Trivy must verify the published image before scanner setup"
    );
    for required in [
        "IMAGE_REF: ${{ env.REGISTRY }}/${{ env.IMAGE }}:${{ matrix.floating_tag }}",
        "docker manifest inspect \"$IMAGE_REF\"",
        "manifest unknown|no such manifest|not found",
        "Expected published image $IMAGE_REF",
        "Image inspection failed",
        "Docker Publish release job",
        "strategy:\n      fail-fast: false",
        "- stem: dist\n            floating_tag: dist",
        "- stem: default-features\n            floating_tag: default-features",
        "- name: Upload Trivy SARIF to GitHub Security tab",
        "category: trivy-${{ matrix.stem }}",
    ] {
        assert!(
            scan_job.contains(required),
            "scheduled Trivy preflight is missing invariant: {required}"
        );
    }
    assert_eq!(
        scan_job
            .matches("if: always() && hashFiles('trivy-results.sarif') != ''")
            .count(),
        2,
        "artifact and Security tab SARIF uploads must both be guarded per matrix leg"
    );
    assert!(
        !scheduled.contains("\n  upload-sarif:\n"),
        "each scan matrix leg must upload its own SARIF result independently"
    );
}

#[test]
fn root_compose_publishes_on_host_loopback_by_default() {
    let compose = repository_file("docker-compose.yml");
    let required_overrides =
        "- ZEROCLAW_gateway__host=0.0.0.0\n      - ZEROCLAW_gateway__allow_public_bind=true";

    assert!(
        compose.contains(required_overrides),
        "Compose must keep the non-loopback gateway host beside its public-bind acknowledgement"
    );
    // The in-container listener is 0.0.0.0, and `allow_public_bind` only
    // silences a startup warning rather than refusing a public bind, so the
    // `ports:` mapping is the only enforced host-side boundary. Default it to
    // loopback: a persisted `require_pairing = false` config answers
    // unauthenticated requests on /webhook, /api/config, and /api/browse.
    assert!(
        compose.contains("${HOST_PORT:-127.0.0.1:42617}:${ZEROCLAW_GATEWAY_PORT:-42617}"),
        "Compose must publish the gateway port on host loopback by default"
    );
}

#[test]
fn compose_smoke_proves_override_precedence_through_the_published_port() {
    let workflow = workflow("docker-image-pr.yml");
    let smoke = repository_file("scripts/ci/smoke_docker_compose.sh");

    for required in [
        "- docker-compose.yml",
        "- scripts/ci/smoke_docker_compose.sh",
        "matrix: ${{ fromJSON(needs.changes.outputs.source_matrix) }}",
        "\"gateway_smoke\":true",
        "load: ${{ matrix.gateway_smoke || (matrix.dockerfile == 'Dockerfile.alpine' && matrix.platform == 'linux/amd64') }}",
        "if: matrix.gateway_smoke",
        "run: bash scripts/ci/smoke_docker_compose.sh",
    ] {
        assert!(
            workflow.contains(required),
            "Docker image PR workflow is missing Compose smoke invariant: {required}"
        );
    }

    for required in [
        "host = \"127.0.0.1\"",
        "HOST_PORT=\"127.0.0.1:${requested_host_port}\"",
        "port zeroclaw 42617",
        "http://127.0.0.1:${published_port}/health",
        ":/zeroclaw-data/.zeroclaw/config.toml:ro",
    ] {
        assert!(
            smoke.contains(required),
            "Compose smoke test is missing published-port invariant: {required}"
        );
    }

    // The fixture must stay observably different from the image's baked
    // config, and the probe must assert that difference. Otherwise a lost
    // config bind lets the baked `[::]` listener answer the same /health and
    // the smoke passes while proving nothing about override precedence.
    assert!(
        smoke.contains("require_pairing = true"),
        "Compose smoke fixture must differ from the baked require_pairing=false config"
    );
    assert!(
        smoke.contains(r#"grep -q '"require_pairing":[[:space:]]*true'"#),
        "Compose smoke must assert the fixture's require_pairing in the health payload"
    );
    assert!(
        smoke.contains(r#"published_address="${published%:*}""#)
            && smoke.contains(r#"if [[ "$published_address" != "127.0.0.1" ]]"#),
        "Compose smoke must assert the resolved publication address, not only the port"
    );
}

#[test]
fn containerfile_serializes_shared_cargo_caches() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let containerfile =
        fs::read_to_string(root.join("Containerfile")).expect("failed to read Containerfile");
    let cargo_cache_mounts = cargo_cache_mounts(&containerfile);

    assert!(
        !cargo_cache_mounts.is_empty(),
        "Containerfile must contain Cargo cache mounts"
    );
    for (mount, sharing) in cargo_cache_mounts {
        assert_eq!(
            sharing,
            Some("locked"),
            "parallel Containerfile stages must serialize shared Cargo caches: {mount}"
        );
    }
}

#[test]
fn cargo_cache_guard_parses_option_order_and_exact_values() {
    let mounts = cargo_cache_mounts(
        "RUN --mount=type=cache,id=registry,target=/root/.cargo/registry cargo fetch\n\
         RUN --mount=destination=/root/.cargo/git,sharing=lockedx,type=cache cargo fetch\n\
         RUN --mount=dst=/root/.cargo/git,type=cache,sharing=locked cargo fetch",
    );

    assert_eq!(mounts.len(), 3);
    assert_eq!(mounts[0].1, None, "reordered unlocked mount must be found");
    assert_eq!(
        mounts[1].1,
        Some("lockedx"),
        "malformed sharing value must not be normalized"
    );
    assert_eq!(
        mounts[2].1,
        Some("locked"),
        "dst alias and reordered locked mount must be found"
    );
}
