use super::DockerManager;
use anyhow::{bail, Result};
use std::path::Path;

/// Build the tenant ZeroClaw image from a Dockerfile.
/// Tags as zeroclaw-tenant:{version} and zeroclaw-tenant:latest.
pub fn build_tenant_image(
    dockerfile_path: &Path,
    context_path: &Path,
    version: &str,
) -> Result<()> {
    let tag_version = format!("zeroclaw-tenant:{}", version);
    let tag_latest = "zeroclaw-tenant:latest";
    let df = dockerfile_path.to_str().unwrap_or("Dockerfile.tenant");
    let ctx = context_path.to_str().unwrap_or(".");

    let out = DockerManager::exec(&["build", "-f", df, "-t", &tag_version, "-t", tag_latest, ctx])?;

    if !out.success {
        bail!("image build failed: {}", out.stderr.trim());
    }

    tracing::info!("Built tenant image: {} + {}", tag_version, tag_latest);
    Ok(())
}

/// Build the egress proxy image from Dockerfile.egress.
pub fn build_egress_image(dockerfile_path: &Path, context_path: &Path) -> Result<()> {
    let df = dockerfile_path.to_str().unwrap_or("Dockerfile.egress");
    let ctx = context_path.to_str().unwrap_or(".");

    let out = DockerManager::exec(&["build", "-f", df, "-t", "zcplatform-egress:latest", ctx])?;

    if !out.success {
        bail!("egress image build failed: {}", out.stderr.trim());
    }

    tracing::info!("Built egress proxy image");
    Ok(())
}

/// List local zeroclaw-tenant image tags.
pub fn list_tenant_images() -> Result<Vec<String>> {
    let out = DockerManager::exec(&["images", "zeroclaw-tenant", "--format", "{{.Tag}}"])?;
    Ok(out.stdout.lines().map(|s| s.to_string()).collect())
}
