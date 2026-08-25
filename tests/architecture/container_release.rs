//! Release invariants for published container variants and scheduled scans.

use std::{collections::HashSet, fs, path::Path};

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

fn builder_stage<'a>(containerfile: &'a str, name: &str) -> &'a str {
    let builder = containerfile
        .split_once(" AS builder")
        .and_then(|(_, builder)| {
            builder
                .strip_prefix("\r\n")
                .or_else(|| builder.strip_prefix('\n'))
        })
        .unwrap_or_else(|| panic!("{name} must define a builder stage"));
    builder.split("\nFROM ").next().unwrap_or(builder)
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

fn workflow_step<'a>(job: &'a str, name: &str) -> &'a str {
    let marker = format!("\n      - name: {name}\n");
    let (_, rest) = job
        .split_once(&marker)
        .unwrap_or_else(|| panic!("workflow job must contain the {name} step"));
    let end = rest.find("\n      - ").unwrap_or(rest.len());
    &rest[..end]
}

fn source_matrix_rows(workflow: &str) -> Vec<serde_json::Value> {
    let branch = workflow
        .split_once("          else\n            prebuilt_images=true\n")
        .map(|(_, branch)| branch)
        .expect("Docker workflow must define the full source-image matrix branch");
    let branch = branch
        .split_once("          echo \"prebuilt_images=$prebuilt_images\"")
        .map(|(branch, _)| branch)
        .expect("Docker workflow must publish the generated source-image matrix");

    let matrix_json = branch
        .split_once("source_matrix='")
        .and_then(|(_, value)| value.split_once('\''))
        .map(|(value, _)| value)
        .expect("full source-image branch must initialize source_matrix as JSON");
    let matrix: serde_json::Value =
        serde_json::from_str(matrix_json).expect("source_matrix must contain valid JSON");
    let mut rows = matrix["include"]
        .as_array()
        .expect("source_matrix must contain an include array")
        .clone();

    let mut additions = branch;
    while let Some((_, rest)) = additions.split_once(".include += ") {
        let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<serde_json::Value>();
        let addition = stream
            .next()
            .expect("matrix append must contain JSON")
            .expect("matrix append must contain valid JSON");
        rows.extend(
            addition
                .as_array()
                .expect("matrix append must contain a JSON array")
                .iter()
                .cloned(),
        );
        additions = &rest[stream.byte_offset()..];
    }

    rows
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
        "load: ${{ matrix.gateway_smoke || (matrix.dockerfile == 'Dockerfile.alpine' && matrix.platform == 'linux/amd64' && !matrix.cargo_flags) }}",
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
fn plugin_enabled_container_variants_stage_wit_and_have_source_build_coverage() {
    for name in ["Dockerfile.alpine", "Dockerfile.debian"] {
        let containerfile = repository_file(name);
        let builder = builder_stage(&containerfile, name);
        let wit_copy = builder
            .find("COPY wit/ wit/")
            .unwrap_or_else(|| panic!("{name} must stage the repository WIT contract"));
        let dependency_build = builder
            .find("$ZEROCLAW_CARGO_FLAGS;")
            .unwrap_or_else(|| panic!("{name} must expose its feature-enabled dependency build"));
        let source_copy = builder
            .find("COPY crates/ crates/")
            .unwrap_or_else(|| panic!("{name} must copy real crate sources"));
        let source_build = builder
            .rfind("$ZEROCLAW_CARGO_FLAGS;")
            .unwrap_or_else(|| panic!("{name} must expose its feature-enabled source build"));
        let cleanup_window = &builder[dependency_build..source_copy];
        let cleanup_instructions: Vec<_> = cleanup_window
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("RUN "))
            .collect();

        assert!(
            wit_copy < dependency_build
                && dependency_build < source_copy
                && source_copy < source_build,
            "{name} must retain WIT across the dependency and real source builds"
        );
        assert_eq!(
            cleanup_instructions,
            ["RUN rm -rf src benches crates xtask tools/fill-translations"],
            "{name} must keep the staged WIT contract through its source cleanup"
        );
    }

    let workflow = workflow("docker-image-pr.yml");
    let rows = source_matrix_rows(&workflow);
    let cargo_flags = "--features plugins-wasm-runtime-only";
    for (dockerfile, tag, cache_scope) in [
        (
            "Dockerfile.debian",
            "zeroclaw-pr-source-debian-wasm:build",
            "docker-source-debian-wasm",
        ),
        (
            "Dockerfile.alpine",
            "zeroclaw-pr-source-alpine-wasm:build",
            "docker-source-alpine-wasm-amd64",
        ),
    ] {
        let matches: Vec<_> = rows
            .iter()
            .filter(|row| row["dockerfile"] == dockerfile && row["cargo_flags"] == cargo_flags)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "source-image matrix must contain exactly one {dockerfile} plugin-build lane"
        );
        let row = matches[0];
        assert_eq!(row["platform"], "linux/amd64");
        assert_eq!(row["tag"], tag);
        assert_eq!(row["cache-scope"], cache_scope);
        assert_eq!(row["gateway_smoke"], false);
    }

    let mut cache_scopes = HashSet::new();
    for row in &rows {
        let cache_scope = row["cache-scope"]
            .as_str()
            .expect("every source-image lane must define a cache scope");
        assert!(
            cache_scopes.insert(cache_scope),
            "source-image cache scope must be unique: {cache_scope}"
        );
    }

    let source_images = top_level_job(&workflow, "source-images");
    let build = workflow_step(
        source_images,
        "Build ${{ matrix.dockerfile }} (${{ matrix.platform }}) from source",
    );
    assert!(
        build.contains(
            "load: ${{ matrix.gateway_smoke || (matrix.dockerfile == 'Dockerfile.alpine' && matrix.platform == 'linux/amd64' && !matrix.cargo_flags) }}"
        ) && build.contains(
            "build-args: ${{ matrix.cargo_flags && format('ZEROCLAW_CARGO_FLAGS={0}', matrix.cargo_flags) || '' }}"
        ),
        "source-image build must pass feature flags conditionally and keep feature lanes build-only"
    );
    let alpine_smoke_guard = "if: matrix.dockerfile == 'Dockerfile.alpine' && matrix.platform == 'linux/amd64' && !matrix.cargo_flags";
    for step in ["Smoke Alpine binaries", "Smoke Alpine Compose runtime"] {
        assert!(
            workflow_step(source_images, step).contains(alpine_smoke_guard),
            "{step} must exclude feature-only source-image lanes"
        );
    }
}

#[test]
fn source_containerfiles_stage_nested_workspace_members_during_prefetch() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace: toml::Value = toml::from_str(&repository_file("Cargo.toml"))
        .expect("root Cargo.toml must contain valid TOML");
    let members = workspace["workspace"]["members"]
        .as_array()
        .expect("root Cargo.toml must define workspace members");
    let nested_crate_members: Vec<_> = members
        .iter()
        .filter_map(toml::Value::as_str)
        .filter(|member| member.starts_with("crates/") && member.split('/').count() > 2)
        .collect();

    assert!(
        !nested_crate_members.is_empty(),
        "workspace fixture must exercise nested crate-member staging"
    );

    for name in ["Dockerfile", "Dockerfile.alpine", "Dockerfile.debian"] {
        let containerfile = repository_file(name);
        let builder = builder_stage(&containerfile, name);
        let dependency_build = builder
            .find("$ZEROCLAW_CARGO_FLAGS;")
            .unwrap_or_else(|| panic!("{name} must expose its dependency build"));
        let prefetch = &builder[..dependency_build];
        let generic_fixture_manifests =
            prefetch.contains("COPY --parents crates/*/tests/fixtures/*/Cargo.toml ./");
        let generic_fixture_lib_stubs = prefetch.contains("for d in crates/*/tests/fixtures/*/")
            && prefetch.contains("printf '' > \"${d}src/lib.rs\"");

        for member in &nested_crate_members {
            let member_segments: Vec<_> = member.split('/').collect();
            let generic_fixture_member = matches!(
                member_segments.as_slice(),
                ["crates", _, "tests", "fixtures", _]
            );
            let manifest_copy = format!("COPY --parents {member}/Cargo.toml ./");
            assert!(
                prefetch.contains(&manifest_copy)
                    || (generic_fixture_member && generic_fixture_manifests),
                "{name} must stage nested workspace manifest {member}/Cargo.toml before prefetch"
            );

            let mut found_target = false;
            for target in ["src/lib.rs", "src/main.rs"] {
                if root.join(member).join(target).is_file() {
                    found_target = true;
                    let stub = format!("{member}/{target}");
                    assert!(
                        prefetch.contains(&stub)
                            || (generic_fixture_member
                                && target == "src/lib.rs"
                                && generic_fixture_lib_stubs),
                        "{name} must stub nested workspace target {stub} before prefetch"
                    );
                }
            }
            assert!(
                found_target,
                "nested workspace member {member} must expose a default lib or bin target"
            );
        }
    }
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
