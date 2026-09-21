//! Architecture gate for Quality Gate runner selection.
//!
//! The compile-heavy jobs name their Blacksmith runner label directly instead
//! of reading it from a `fmt` output. `runs-on` resolves before a job is
//! created, so reading it from another job forced every compile job to wait for
//! a GitHub-hosted runner to pick up the formatting check first. Writing the
//! constant once per job is what removes that wait, and this gate is what keeps
//! the copies honest: a stray label sends one job to a different runner class,
//! and a stray cache-provider input sends it to a cache the rest of the fleet
//! never writes.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use regex::Regex;

/// The one runner label the compile-heavy fleet may use. Moving the fleet means
/// changing this constant and every job below in the same reviewed commit.
const RUNNER_LABEL: &str = "blacksmith-8vcpu-ubuntu-2404";

/// The runner label for housekeeping jobs: formatting, change detection,
/// repository guards, docs and policy gates, the Nix checks, the container
/// smoke, the Windows-test selector, and the required-gate aggregator. None of
/// them needs 8 vCPUs, but all of them were stranded on `ubuntu-latest` during
/// the 2026-09-14 hosted-runner assignment outage while the Blacksmith fleet
/// ran untouched, so the whole required gate stalled on jobs whose combined
/// work is minutes. Hosting them on Blacksmith removes GitHub's hosted pool
/// from the required gate's critical path entirely.
const HOUSEKEEPING_LABEL: &str = "blacksmith-4vcpu-ubuntu-2404";

/// Every Quality Gate housekeeping job on the Blacksmith 4-vCPU class. Same contract as
/// `COMPILE_JOBS`: this list is the reviewable inventory the workflow is
/// checked against.
const HOUSEKEEPING_JOBS: [&str; 15] = [
    "fmt",
    "gate",
    "history-guard",
    "repo-structure",
    "docs-style",
    "zerocode-rpc-boundary",
    "parallel-runtime-test-changes",
    "path-changes",
    "relay-container-smoke-changes",
    "windows-clippy-tools-changes",
    "nix-eval",
    "nix-hash-drift",
    "relay-container-smoke",
    "security",
    "web-permission-tests",
];

/// Jobs that stay on GitHub-hosted `ubuntu-latest` because they depend on the
/// hosted image itself. `test-landlock` exercises the Landlock LSM, which the
/// GitHub kernel enables; a different runner class could silently skip that
/// security coverage. Adding a job here requires a reason of that kind, not
/// convenience: during a hosted outage, every entry in this list is a check
/// that cannot run.
const HOSTED_LINUX_JOBS: [&str; 1] = ["test-landlock"];

/// Every job that compiles the workspace on the Blacksmith fleet. A new compile
/// job must be added here, which is the point: the list is the inventory this
/// gate checks the workflow against.
const COMPILE_JOBS: [&str; 12] = [
    "lint",
    "build",
    "check",
    "check-plugin-backends",
    "msrv",
    "check-32bit",
    "bench",
    "test",
    "test-channel-features",
    "memory-postgres-test",
    "parallel-runtime-test",
    "installer-drift",
];

/// `use-blacksmith` inputs the rust-cache composite may receive. The matrix
/// expression belongs to `build`, whose macOS and Windows legs stay on the
/// GitHub-hosted cache; `'false'` belongs to the web job, which does not
/// compile the workspace.
const ALLOWED_CACHE_INPUTS: [&str; 3] = [
    "'true'",
    "'false'",
    "${{ matrix.target == 'x86_64-unknown-linux-gnu' }}",
];

fn ci_workflow() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(".github/workflows/ci.yml"))
        .expect("failed to read .github/workflows/ci.yml")
}

/// Split the `jobs:` mapping into `job id -> job body`. Only scans after the
/// top-level `jobs:` key so that `on:` children such as `pull_request:` are
/// never mistaken for jobs.
fn job_blocks(workflow: &str) -> BTreeMap<String, String> {
    let (_, jobs) = workflow
        .split_once("\njobs:\n")
        .expect("ci.yml must declare a top-level jobs mapping");
    let header = Regex::new(r"(?m)^  ([a-z0-9-]+):$").expect("valid job-header pattern");

    let starts: Vec<(usize, String)> = header
        .captures_iter(jobs)
        .map(|capture| {
            let whole = capture.get(0).expect("match 0 always exists");
            (whole.start(), capture[1].to_string())
        })
        .collect();
    assert!(!starts.is_empty(), "ci.yml must define at least one job");

    let mut blocks = BTreeMap::new();
    for (index, (offset, name)) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(jobs.len(), |(next, _)| *next);
        let previous = blocks.insert(name.clone(), jobs[*offset..end].to_string());
        assert!(previous.is_none(), "duplicate job id {name} in ci.yml");
    }
    blocks
}

/// The `needs:` list of a job, empty when it declares none.
fn needs(block: &str) -> Vec<String> {
    let Some(line) = block.lines().find(|line| line.starts_with("    needs: [")) else {
        return Vec::new();
    };
    line.trim_start()
        .trim_start_matches("needs: [")
        .trim_end_matches(']')
        .split(',')
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect()
}

#[test]
fn compile_jobs_pin_the_runner_label_instead_of_reading_it_from_fmt() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);

    assert!(
        !workflow.contains("needs.fmt.outputs"),
        "no job may read a runner label or cache provider from fmt: that forces \
         every compile job to wait for a GitHub-hosted runner before it starts"
    );

    for name in COMPILE_JOBS {
        let block = blocks
            .get(name)
            .unwrap_or_else(|| panic!("ci.yml must define the {name} job"));
        assert!(
            !needs(block).iter().any(|dependency| dependency == "fmt"),
            "{name} must not declare needs: [fmt]; the gate job is what keeps a \
             formatting error blocking merge"
        );
        let pins_label = if name == "build" {
            // The Linux leg carries the label through the matrix; the macOS and
            // Windows legs name their own GitHub-hosted images.
            block.contains(&format!("- os: {RUNNER_LABEL}\n"))
                && block.contains("runs-on: ${{ matrix.os }}\n")
        } else {
            block.contains(&format!("    runs-on: {RUNNER_LABEL}\n"))
        };
        assert!(pins_label, "{name} must run on {RUNNER_LABEL}");
    }
}

#[test]
fn only_the_declared_compile_jobs_claim_the_blacksmith_fleet() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);

    let claiming: BTreeSet<&str> = blocks
        .iter()
        .filter(|(_, block)| block.contains(RUNNER_LABEL))
        .map(|(name, _)| name.as_str())
        .collect();
    let declared: BTreeSet<&str> = COMPILE_JOBS.into_iter().collect();

    assert_eq!(
        claiming, declared,
        "every job using {RUNNER_LABEL} must be listed in COMPILE_JOBS, so the \
         fleet inventory stays reviewable in one place"
    );
}

#[test]
fn rust_cache_callers_pass_a_reviewed_provider_input() {
    let workflow = ci_workflow();

    for line in workflow.lines() {
        let Some((_, value)) = line.trim_start().split_once("use-blacksmith: ") else {
            continue;
        };
        assert!(
            ALLOWED_CACHE_INPUTS.contains(&value.trim()),
            "unexpected rust-cache provider input {value:?}: a job that writes a \
             cache the rest of the fleet never reads silently loses its cache"
        );
    }
}

#[test]
fn housekeeping_jobs_pin_the_four_vcpu_label() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);

    for name in HOUSEKEEPING_JOBS {
        let block = blocks
            .get(name)
            .unwrap_or_else(|| panic!("ci.yml must define the {name} job"));
        assert!(
            block.contains(&format!("    runs-on: {HOUSEKEEPING_LABEL}\n")),
            "{name} must run on {HOUSEKEEPING_LABEL}"
        );
    }

    // The advisory selector keeps its runner contract after moving workflows.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let advisory = fs::read_to_string(root.join(".github/workflows/windows-tests.yml"))
        .expect("failed to read .github/workflows/windows-tests.yml");
    let advisory_blocks = job_blocks(&advisory);
    let selector = advisory_blocks
        .get("windows-test-scope")
        .expect("windows-tests.yml must define the advisory selector");
    assert!(
        selector.contains(&format!("    runs-on: {HOUSEKEEPING_LABEL}\n")),
        "the advisory selector must run on {HOUSEKEEPING_LABEL}"
    );
}

#[test]
fn only_the_declared_housekeeping_jobs_claim_the_four_vcpu_class() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);

    let claiming: BTreeSet<&str> = blocks
        .iter()
        .filter(|(_, block)| block.contains(HOUSEKEEPING_LABEL))
        .map(|(name, _)| name.as_str())
        .collect();
    let declared: BTreeSet<&str> = HOUSEKEEPING_JOBS.into_iter().collect();

    assert_eq!(
        claiming, declared,
        "every job using {HOUSEKEEPING_LABEL} must be listed in \
         HOUSEKEEPING_JOBS, so this inventory stays reviewable in one place"
    );
}

#[test]
fn hosted_linux_stays_an_explicit_allowlist() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);

    let hosted: BTreeSet<&str> = blocks
        .iter()
        .filter(|(_, block)| block.contains("    runs-on: ubuntu-latest\n"))
        .map(|(name, _)| name.as_str())
        .collect();
    let declared: BTreeSet<&str> = HOSTED_LINUX_JOBS.into_iter().collect();

    assert_eq!(
        hosted, declared,
        "a Linux job may use GitHub-hosted ubuntu-latest only when it depends \
         on the hosted image itself (see HOSTED_LINUX_JOBS): every job here is \
         one the required gate cannot run during a hosted-runner outage"
    );
}

#[test]
fn the_required_gate_still_waits_for_formatting() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);
    let gate = blocks.get("gate").expect("ci.yml must define the gate job");

    assert!(
        needs(gate).iter().any(|dependency| dependency == "fmt"),
        "CI Required Gate must keep needing fmt: it is the only thing that still \
         makes a formatting error block merge"
    );
}
