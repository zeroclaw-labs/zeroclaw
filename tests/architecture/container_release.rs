//! Release invariants for published container variants and scheduled scans.

use std::{fs, path::Path};

fn workflow(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(".github/workflows").join(name))
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
        "secrets: inherit",
        "contents: read",
        "packages: write",
        "id-token: write",
        "security-events: write",
    ] {
        assert!(
            matrix_job.contains(required),
            "Docker matrix call is missing release invariant: {required}"
        );
    }

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
    let preflight = scheduled
        .find("- name: Verify published image exists")
        .expect("scheduled Trivy must contain an image-existence preflight");
    let scan = scheduled
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
        "- name: Upload Trivy SARIF to GitHub Security tab",
        "category: trivy-${{ matrix.stem }}",
    ] {
        assert!(
            scheduled.contains(required),
            "scheduled Trivy preflight is missing invariant: {required}"
        );
    }
    assert!(
        !scheduled.contains("\n  upload-sarif:\n"),
        "each scan matrix leg must upload its own SARIF result independently"
    );
}
