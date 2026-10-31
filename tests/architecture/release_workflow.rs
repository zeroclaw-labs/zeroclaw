//! Architecture gates for release-workflow artifact identity.
//!
//! The macOS desktop job must notarize, staple, validate, and upload the same
//! DMG. Discovering the file independently in multiple steps can notarize one
//! image while publishing another.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn bash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Resolve the interpreter that runs the POSIX release scripts below.
///
/// `Command::new("bash")` is not safe on Windows: `CreateProcess` searches the
/// system directory before `PATH`, and Windows ships a WSL launcher stub at
/// `%SystemRoot%\System32\bash.exe`. The GitHub Windows images carry that stub
/// without a WSL distribution, so it exits non-zero without running the script
/// and without writing anything to stderr — every script-driven gate here then
/// fails with an empty diagnostic. Resolve Git for Windows' Bash explicitly
/// (the same interpreter the workflow's own `shell: bash` steps use) and only
/// fall back to bare name resolution when no real Bash can be located.
fn bash_command() -> Command {
    Command::new(bash_program())
}

#[cfg(not(windows))]
fn bash_program() -> PathBuf {
    PathBuf::from("bash")
}

#[cfg(windows)]
fn bash_program() -> PathBuf {
    let git_bash = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("Git").join("bin").join("bash.exe"));

    let on_path = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .filter(|directory| !is_windows_system_directory(directory))
                .map(|directory| directory.join("bash.exe"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    git_bash
        .chain(on_path)
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("bash"))
}

/// The WSL stub lives in the system directory; nothing else named `bash.exe`
/// is expected there, so excluding it is enough to reach a real interpreter.
#[cfg(windows)]
fn is_windows_system_directory(directory: &Path) -> bool {
    directory.file_name().is_some_and(|name| {
        let name = name.to_string_lossy().to_ascii_lowercase();
        name == "system32" || name == "syswow64"
    })
}

fn workflow(name: &str) -> String {
    let workflow_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".github/workflows")
        .join(name);
    fs::read_to_string(&workflow_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", workflow_path.display()))
}

/// Render everything a failed script run can tell us.
///
/// A misresolved or non-functional interpreter fails with an empty stderr, so
/// an assertion that reports stderr alone leaves nothing to diagnose. Include
/// the exit status and stdout as well.
fn command_diagnostics(output: &std::process::Output) -> String {
    format!(
        "status={:?} stdout={:?} stderr={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_command_failure(
    output: &std::process::Output,
    expected_code: i32,
    expected_stderr: &str,
    context: &str,
) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "{context}: {stderr}"
    );
    assert!(
        stderr.contains(expected_stderr),
        "{context} must report {expected_stderr:?}, got: {stderr}"
    );
}

fn yaml_block<'a>(document: &'a str, header: &str) -> &'a str {
    let header_indent = header.len() - header.trim_start().len();
    let start = document
        .match_indices(header)
        .find_map(|(offset, _)| {
            (offset == 0 || document.as_bytes().get(offset - 1) == Some(&b'\n')).then_some(offset)
        })
        .unwrap_or_else(|| panic!("workflow is missing YAML block: {header}"));
    let remainder = &document[start + header.len()..];
    let end = remainder
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let line_start = *offset;
            *offset += line.len();
            Some((line_start, line))
        })
        .find_map(|(offset, line)| {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();
            (!trimmed.is_empty() && !trimmed.starts_with('#') && indent <= header_indent)
                .then_some(offset)
        })
        .unwrap_or(remainder.len());
    &remainder[..end]
}

#[test]
fn workflow_yaml_block_retains_outdented_comments_until_the_next_job() {
    let release = workflow("release-stable-manual.yml");
    assert!(release.contains("\n# >>> end generated:release-distributions <<<\n"));
    let build = yaml_block(&release, "  build:\n");
    assert!(build.contains("uses: ./.github/actions/build-release-payloads"));
    assert!(
        yaml_block(yaml_block(build, "    strategy:\n"), "      matrix:\n").contains("include:")
    );
    assert!(!build.contains("  build-desktop:\n"));
    let annotated = release.replacen(
        "\n  build-desktop:\n",
        "\n# comment before the next real job\n  build-desktop:\n",
        1,
    );
    assert_ne!(annotated, release);
    let retained = yaml_block(&annotated, "  build:\n");
    assert!(retained.contains("# comment before the next real job"));
    assert!(retained.contains("uses: ./.github/actions/build-release-payloads"));
    assert!(
        yaml_block(yaml_block(retained, "    strategy:\n"), "      matrix:\n").contains("include:")
    );
    assert!(!retained.contains("  build-desktop:\n"));
}

// Immutable test-only reference from public 8b04361c24b99d5a700ac97f517308c2ed456650.
// The production workflows must never consume this fixture. Its complete bytes,
// rather than the edited caller or shallow Git history, pin the before recipe.
const OLD_RELEASE_RECIPE_SHA256: &str =
    "40bccde145213f19c264fdef9a64fcfd1959acf124925bb2cc5beaf91efbabd9";
const OLD_RELEASE_RECIPE: &str = r###"if [ -n "${{ matrix.linker_env || '' }}" ] && [ -n "${{ matrix.linker || '' }}" ]; then
  export "${{ matrix.linker_env }}=${{ matrix.linker }}"
fi
# Force ARMv6 codegen for arm-unknown-linux-gnueabihf (#4556)
# Ubuntu 22.04's gcc-arm-linux-gnueabihf defaults to ARMv7+NEON,
# which segfaults on ARMv6 devices (e.g. Raspberry Pi Zero W).
if [ "${{ matrix.target }}" = "arm-unknown-linux-gnueabihf" ]; then
  export CFLAGS_arm_unknown_linux_gnueabihf="-march=armv6 -mfpu=vfp -mfloat-abi=hard"
  export CXXFLAGS_arm_unknown_linux_gnueabihf="-march=armv6 -mfpu=vfp -mfloat-abi=hard"
  export CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="-C target-feature=-neon"
fi
# Resolve the feature set from the canonical registry
# (`cargo generate features`), the single source of truth that
# install.sh and packaging surfaces also consume. `dist` is the lean
# standard distribution set and carries the default leaves, so every
# target builds with --no-default-features against the explicit list,
# with no implicit feature drift.
FEATURES="$(cargo run --quiet -p xtask --bin generate -- features --selection dist --target "${{ matrix.target }}")"
echo "Resolved features: $FEATURES"
# MUSL targets build via cross-rs (cross-compiled in a container);
# all other targets use the host cargo toolchain directly.
if [ "${{ matrix.use_cross || 'false' }}" = "true" ]; then
  BUILD_CMD="cross build"
else
  BUILD_CMD="cargo build"
fi
# Always build --no-default-features against the generator's explicit
# per-target list: FEATURES already carries the default leaves for the
# target, so this applies the canonical per-target exclusions with no
# implicit feature drift. Unioning the root crate's defaults back in
# (by omitting --no-default-features) would silently re-enable a
# feature the generator excludes for this target (e.g. the Prometheus
# exporter on ARM).
$BUILD_CMD --release --locked --no-default-features --features "${FEATURES}" --target ${{ matrix.target }}
# Build the zerocode TUI and the zerorelay nominated relay alongside the
# main binary so they ship in the release archive. Skipped on Android,
# which lacks the terminal deps.
if [ "${{ matrix.target }}" != "aarch64-linux-android" ]; then
  $BUILD_CMD --release --locked -p zerocode --target ${{ matrix.target }}
  $BUILD_CMD --release --locked -p zerorelay --target ${{ matrix.target }}
fi
"###;

#[derive(Clone, Copy)]
struct ReleaseRecipeContext {
    target: &'static str,
    linker_env: &'static str,
    linker: &'static str,
    use_cross: bool,
}

// Test contexts, not a production target registry. Each is bound below to the
// corresponding existing Stable matrix row; no feature list is copied here.
const RELEASE_RECIPE_CONTEXTS: [ReleaseRecipeContext; 10] = [
    ReleaseRecipeContext {
        target: "x86_64-unknown-linux-gnu",
        linker_env: "",
        linker: "",
        use_cross: false,
    },
    ReleaseRecipeContext {
        target: "x86_64-unknown-linux-musl",
        linker_env: "",
        linker: "",
        use_cross: true,
    },
    ReleaseRecipeContext {
        target: "aarch64-unknown-linux-gnu",
        linker_env: "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER",
        linker: "aarch64-linux-gnu-gcc",
        use_cross: false,
    },
    ReleaseRecipeContext {
        target: "aarch64-unknown-linux-musl",
        linker_env: "",
        linker: "",
        use_cross: true,
    },
    ReleaseRecipeContext {
        target: "armv7-unknown-linux-gnueabihf",
        linker_env: "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER",
        linker: "arm-linux-gnueabihf-gcc",
        use_cross: false,
    },
    ReleaseRecipeContext {
        target: "arm-unknown-linux-gnueabihf",
        linker_env: "CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER",
        linker: "arm-linux-gnueabihf-gcc",
        use_cross: false,
    },
    ReleaseRecipeContext {
        target: "aarch64-apple-darwin",
        linker_env: "",
        linker: "",
        use_cross: false,
    },
    ReleaseRecipeContext {
        target: "x86_64-apple-darwin",
        linker_env: "",
        linker: "",
        use_cross: false,
    },
    ReleaseRecipeContext {
        target: "aarch64-linux-android",
        linker_env: "",
        linker: "",
        use_cross: false,
    },
    ReleaseRecipeContext {
        target: "x86_64-pc-windows-msvc",
        linker_env: "",
        linker: "",
        use_cross: false,
    },
];

fn explicit_bash_body(step: &str, indentation: &str) -> String {
    let (_, body) = step
        .split_once("run: |\n")
        .expect("the recipe has one literal Bash body");
    // The literal block's default clip retains one final LF; inter-step blank
    // spacing belongs to the caller, not to its Bash recipe.
    body.trim_end_matches('\n')
        .lines()
        .map(|line| {
            let line = if line.is_empty() {
                ""
            } else {
                line.strip_prefix(indentation)
                    .expect("recipe line must retain its literal indentation")
            };
            format!("{line}\n")
        })
        .collect()
}

fn shared_release_recipe() -> String {
    let action_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".github/actions/build-release-payloads/action.yml");
    let action = fs::read_to_string(action_path).expect("read shared release recipe");
    let step = yaml_block(&action, "    - id: build\n");
    assert!(step.contains("      shell: bash\n"));
    assert!(action.contains("  cargo-jobs:\n"));
    assert!(action.contains("    default: ''\n"));
    assert!(step.contains("        RELEASE_CARGO_JOBS: ${{ inputs.cargo-jobs }}\n"));
    assert!(step.contains("        RELEASE_FEATURES_EVIDENCE: ${{ inputs.emit_features }}\n"));
    for binding in [
        "RELEASE_TARGET: ${{ inputs.target }}",
        "RELEASE_LINKER_ENV: ${{ inputs.linker_env || '' }}",
        "RELEASE_LINKER: ${{ inputs.linker || '' }}",
        "RELEASE_USE_CROSS: ${{ inputs.use_cross || 'false' }}",
    ] {
        assert!(step.contains(binding));
    }
    let body = explicit_bash_body(step, "        ");
    assert!(!body.contains("${{"), "recipe expressions belong in env");
    body
}

/// The Stable build gives both Linux ARM64 targets the bounded memory helper
/// and two build jobs; the GNU-only qualification job has no musl leg.
const STABLE_ARM64_TARGETS: &str = "(matrix.target == 'aarch64-unknown-linux-gnu' || matrix.target == 'aarch64-unknown-linux-musl')";
const GNU_ARM64_TARGET: &str = "matrix.target == 'aarch64-unknown-linux-gnu'";

fn assert_release_caller(step: &str, emit_features: &str, arm64_targets: &str) {
    assert!(step.contains("        uses: ./.github/actions/build-release-payloads\n"));
    assert!(!step.contains("shell:") && !step.contains("run:"));
    assert_eq!(
        yaml_block(step, "        with:\n").trim_end(),
        format!(
            concat!(
                "          target: ${{{{ matrix.target }}}}\n",
                "          linker_env: ${{{{ matrix.linker_env || '' }}}}\n",
                "          linker: ${{{{ matrix.linker || '' }}}}\n",
                "          use_cross: ${{{{ matrix.use_cross || 'false' }}}}\n",
                "          emit_features: '{}'\n",
                "          cargo-jobs: ${{{{ {} && '2' || '' }}}}"
            ),
            emit_features, arm64_targets
        )
    );
}

fn arm64_build_memory_steps(arm64_targets: &str) -> [String; 2] {
    [
        format!(
            concat!(
                "      - name: Prepare bounded Linux ARM64 build memory\n",
                "        id: dist_memory\n",
                "        timeout-minutes: 5\n",
                "        if: runner.os == 'Linux' && {}\n",
                "        shell: bash\n",
                "        run: bash scripts/ci/dist_runner_memory.sh prepare\n\n"
            ),
            arm64_targets
        ),
        format!(
            concat!(
                "      - name: Report and release owned Linux ARM64 build memory\n",
                "        timeout-minutes: 5\n",
                "        if: always() && runner.os == 'Linux' && {}\n",
                "        shell: bash\n",
                "        env:\n",
                "          ZC_DIST_SWAP_FILE: ${{{{ steps.dist_memory.outputs.swap_file }}}}\n",
                "          ZC_DIST_SWAP_IDENTITY: ${{{{ steps.dist_memory.outputs.swap_identity }}}}\n",
                "        run: bash scripts/ci/dist_runner_memory.sh cleanup\n\n"
            ),
            arm64_targets
        ),
    ]
}

fn assert_arm64_build_memory(job: &str, arm64_targets: &str) {
    let [prepare, cleanup] = arm64_build_memory_steps(arm64_targets);
    assert_eq!(job.matches(&prepare).count(), 1);
    assert_eq!(job.matches(&cleanup).count(), 1);
    assert_eq!(job.matches("dist_runner_memory.sh").count(), 2);
    let build = job.find("      - name: Build release\n").unwrap();
    assert!(job.find(&prepare).unwrap() < build && build < job.find(&cleanup).unwrap());
    let emit_features = if job.contains("emit_features: 'true'") {
        "true"
    } else {
        "false"
    };
    assert_release_caller(
        yaml_block(job, "      - name: Build release\n"),
        emit_features,
        arm64_targets,
    );
}

fn without_arm64_build_memory(document: &str, arm64_targets: &str) -> String {
    let mut document = document.to_owned();
    for step in arm64_build_memory_steps(arm64_targets) {
        assert_eq!(document.matches(&step).count(), 1);
        document = document.replacen(&step, "", 1);
    }
    document
}

fn render_release_recipe(body: &str, namespace: &str, context: ReleaseRecipeContext) -> String {
    let mut body = body.to_string();
    for (field, value) in [
        ("target", context.target),
        ("linker_env", context.linker_env),
        ("linker", context.linker),
        (
            "use_cross",
            if context.use_cross { "true" } else { "false" },
        ),
    ] {
        for expression in [
            format!("${{{{ {namespace}.{field} }}}}"),
            format!("${{{{ {namespace}.{field} || '' }}}}"),
            format!("${{{{ {namespace}.{field} || 'false' }}}}"),
        ] {
            body = body.replace(&expression, value);
        }
    }
    assert!(!body.contains("${{"), "unbound recipe expression: {body}");
    body
}

struct RecipeObservation {
    status: Option<i32>,
    trace: Vec<String>,
    evidence: Option<String>,
    diagnostics: String,
}

fn run_release_recipe(
    body: &str,
    context: ReleaseRecipeContext,
    failure: &str,
    emit_features: &str,
    features: &str,
) -> RecipeObservation {
    let temp = tempfile::tempdir().expect("recipe scratch");
    let script = temp.path().join("recipe.sh");
    let trace = temp.path().join("trace.txt");
    let evidence = temp.path().join("output.txt");
    // Only controlled Bash functions run. The actual extracted recipe selects
    // cargo/cross, expands arguments, exports environment and propagates status.
    // No compiler, package manager, container or payload binary is invoked.
    let harness = r#"
record_command() {
  local linker=""
  if [ -n "$RECIPE_LINKER_ENV" ]; then
    linker="${!RECIPE_LINKER_ENV}"
  fi
  printf '%s' "$1" >> "$RECIPE_TRACE"
  shift
  for arg in "$@"; do printf '|%s' "$arg" >> "$RECIPE_TRACE"; done
  printf '|linker=%s|cflags=%s|cxxflags=%s|rustflags=%s|jobs=%s\n' \
    "$linker" "$CFLAGS_arm_unknown_linux_gnueabihf" \
    "$CXXFLAGS_arm_unknown_linux_gnueabihf" \
    "$CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS" \
    "$CARGO_BUILD_JOBS" >> "$RECIPE_TRACE"
}
controlled_command() {
  local program="$1" stage="main" previous=""
  shift
  record_command "$program" "$@"
  if [ "$1" = run ]; then
    stage="generator"
  else
    for arg in "$@"; do
      if [ "$previous" = -p ]; then stage="$arg"; fi
      previous="$arg"
    done
  fi
  if [ "$RECIPE_FAIL" = "$stage" ]; then return 83; fi
  if [ "$stage" = generator ]; then printf '%s\n' "$RECIPE_FEATURES"; fi
}
cargo() { controlled_command cargo "$@"; }
cross() { controlled_command cross "$@"; }
"#;
    fs::write(&script, format!("{harness}\n{body}")).expect("write recipe fixture");
    let mut command = bash_command();
    command
        .args(["--noprofile", "--norc", "-e", "-o", "pipefail"])
        .arg(bash_path(&script))
        .current_dir(temp.path())
        .env_clear()
        .env("RECIPE_TRACE", bash_path(&trace))
        .env("RECIPE_LINKER_ENV", context.linker_env)
        .env("RECIPE_FAIL", failure)
        .env("RECIPE_FEATURES", features)
        .env("RELEASE_FEATURES_EVIDENCE", emit_features)
        .env("RELEASE_TARGET", context.target)
        .env("RELEASE_LINKER_ENV", context.linker_env)
        .env("RELEASE_LINKER", context.linker)
        .env(
            "RELEASE_USE_CROSS",
            if context.use_cross { "true" } else { "false" },
        );
    // Retain interpreter startup inputs, including Windows' system directory,
    // while excluding inherited Cargo/compiler flags from the recipe oracle.
    for key in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    // With evidence disabled, leave GITHUB_OUTPUT absent: a new Stable write
    // would fail or create an unexpected file rather than escape this oracle.
    if emit_features != "false" {
        command.env("GITHUB_OUTPUT", bash_path(&evidence));
    }
    let output = command.output().expect("execute controlled recipe fixture");
    RecipeObservation {
        status: output.status.code(),
        trace: fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect(),
        evidence: fs::read_to_string(&evidence).ok(),
        diagnostics: command_diagnostics(&output),
    }
}

fn expected_release_trace(
    context: ReleaseRecipeContext,
    failure: &str,
    features: &str,
) -> Vec<String> {
    let armv6 = context.target == "arm-unknown-linux-gnueabihf";
    let flags = if armv6 {
        "-march=armv6 -mfpu=vfp -mfloat-abi=hard"
    } else {
        ""
    };
    let rustflags = if armv6 { "-C target-feature=-neon" } else { "" };
    let environment = format!(
        "|linker={}|cflags={flags}|cxxflags={flags}|rustflags={rustflags}|jobs=",
        context.linker
    );
    let program = if context.use_cross { "cross" } else { "cargo" };
    let mut trace = vec![format!(
        "cargo|run|--quiet|-p|xtask|--bin|generate|--|features|--selection|dist|--target|{}{environment}",
        context.target
    )];
    if failure == "generator" {
        return trace;
    }
    trace.push(format!(
        "{program}|build|--release|--locked|--no-default-features|--features|{features}|--target|{}{environment}",
        context.target
    ));
    if failure == "main" || context.target == "aarch64-linux-android" {
        return trace;
    }
    for package in ["zerocode", "zerorelay"] {
        trace.push(format!(
            "{program}|build|--release|--locked|-p|{package}|--target|{}{environment}",
            context.target
        ));
        if failure == package {
            break;
        }
    }
    trace
}

fn assert_release_trace(
    observed: &RecipeObservation,
    context: ReleaseRecipeContext,
    failure: &str,
    features: &str,
) {
    let fails = !failure.is_empty()
        && !(context.target == "aarch64-linux-android"
            && matches!(failure, "zerocode" | "zerorelay"));
    assert_eq!(
        observed.status,
        Some(if fails { 83 } else { 0 }),
        "{} {failure}: {}",
        context.target,
        observed.diagnostics
    );
    assert_eq!(
        observed.trace,
        expected_release_trace(context, failure, features),
        "{} {failure}: {}",
        context.target,
        observed.diagnostics
    );
}

#[test]
fn immutable_stable_release_recipe_has_real_command_and_failure_baselines() {
    use sha2::{Digest, Sha256};

    assert_eq!(
        format!("{:x}", Sha256::digest(OLD_RELEASE_RECIPE.as_bytes())),
        OLD_RELEASE_RECIPE_SHA256
    );
    let original = workflow("release-stable-manual.yml");
    assert_arm64_build_memory(yaml_block(&original, "  build:\n"), STABLE_ARM64_TARGETS);
    let release = without_arm64_build_memory(&original, STABLE_ARM64_TARGETS);
    let build = yaml_block(&release, "  build:\n");
    let caller = yaml_block(build, "      - name: Build release\n");
    assert_release_caller(caller, "false", STABLE_ARM64_TARGETS);
    // Pin every byte outside the one reviewed step wrapper. The independent
    // shared-body gate below inverts the input adapters to the immutable body.
    let start = release.find("      - name: Build release\n").unwrap();
    let end = release[start..]
        .find("      - name: Check binary size\n")
        .unwrap()
        + start;
    let outside = format!("{}{}", &release[..start], &release[end..]);
    assert_eq!(
        format!("{:x}", Sha256::digest(outside.as_bytes())),
        "188c8f6bef4d2eb7bbeb3a8885607a09ed363e7b2cbdb57c1265251d19528899"
    );
    let matrix = yaml_block(build, "      matrix:\n");
    for context in RELEASE_RECIPE_CONTEXTS {
        let target_line = format!("            target: {}\n", context.target);
        let (_, row) = matrix
            .split_once(&target_line)
            .expect("the baseline context must name an existing Stable target");
        let row = row.split("          - os:").next().unwrap();
        assert_eq!(row.contains("linker_env:"), !context.linker_env.is_empty());
        assert_eq!(row.contains("linker:"), !context.linker.is_empty());
        for binding in [
            format!("linker_env: {}", context.linker_env),
            format!("linker: {}", context.linker),
        ] {
            if !context.linker.is_empty() {
                assert!(row.contains(&binding), "{}: {binding}", context.target);
            }
        }
        assert_eq!(row.contains("use_cross: true"), context.use_cross);
        let recipe = render_release_recipe(OLD_RELEASE_RECIPE, "matrix", context);
        for failure in ["", "generator", "main", "zerocode", "zerorelay"] {
            let observed =
                run_release_recipe(&recipe, context, failure, "false", "fixture-a,fixture-b");
            assert_release_trace(&observed, context, failure, "fixture-a,fixture-b");
            assert!(observed.evidence.is_none());
        }
    }
}

#[test]
fn shared_release_recipe_preserves_all_ten_command_failure_contexts() {
    let body = shared_release_recipe();
    let mut inverted = body.clone();
    for (start, end) in [
        (
            "# BEGIN release input/evidence adapter\n",
            "# END release input/evidence adapter\n",
        ),
        (
            "# BEGIN qualification-only FEATURES output adapter\n",
            "# END qualification-only FEATURES output adapter\n",
        ),
        (
            "# BEGIN optional build concurrency adapter\n",
            "# END optional build concurrency adapter\n",
        ),
    ] {
        let from = inverted.find(start).expect("identified adapter starts");
        let through = inverted[from..]
            .find(end)
            .map(|offset| from + offset + end.len())
            .expect("identified adapter ends");
        inverted.replace_range(from..through, "");
    }
    // Invert only the four checked env bindings and target argument quoting;
    // the immutable full-body comparison and all ten argv traces stay intact.
    for (from, to) in [
        (
            r#"if [ -n "$RELEASE_LINKER_ENV" ] && [ -n "$RELEASE_LINKER" ]; then"#,
            r#"if [ -n "${{ matrix.linker_env || '' }}" ] && [ -n "${{ matrix.linker || '' }}" ]; then"#,
        ),
        (
            r#"export "$RELEASE_LINKER_ENV=$RELEASE_LINKER""#,
            r#"export "${{ matrix.linker_env }}=${{ matrix.linker }}""#,
        ),
        ("$RELEASE_TARGET", "${{ matrix.target }}"),
        ("$RELEASE_USE_CROSS", "${{ matrix.use_cross || 'false' }}"),
    ] {
        assert!(
            inverted.contains(from),
            "env binding must reach recipe: {from}"
        );
        inverted = inverted.replace(from, to);
    }
    for command in [
        r#"$BUILD_CMD --release --locked --no-default-features --features "${FEATURES}""#,
        "$BUILD_CMD --release --locked -p zerocode",
        "$BUILD_CMD --release --locked -p zerorelay",
    ] {
        let quoted = format!(r#"{command} --target "${{{{ matrix.target }}}}""#);
        let original = format!("{command} --target ${{{{ matrix.target }}}}");
        assert_eq!(inverted.matches(&quoted).count(), 1);
        inverted = inverted.replacen(&quoted, &original, 1);
    }
    assert_eq!(
        inverted, OLD_RELEASE_RECIPE,
        "only the four env bindings, target quoting and identified adapters may differ"
    );
    for context in RELEASE_RECIPE_CONTEXTS {
        let recipe = render_release_recipe(&body, "inputs", context);
        for failure in ["", "generator", "main", "zerocode", "zerorelay"] {
            let observed =
                run_release_recipe(&recipe, context, failure, "false", "fixture-a,fixture-b");
            assert_release_trace(&observed, context, failure, "fixture-a,fixture-b");
            assert!(
                observed.evidence.is_none(),
                "Stable must not write evidence"
            );
        }
        let observed = run_release_recipe(&recipe, context, "", "true", "fixture-a,fixture-b");
        assert_release_trace(&observed, context, "", "fixture-a,fixture-b");
        assert_eq!(
            observed.evidence.as_deref(),
            Some("features=fixture-a,fixture-b\n")
        );
    }
}

#[test]
fn shared_release_recipe_optional_jobs_is_explicit_and_narrow() {
    let context = RELEASE_RECIPE_CONTEXTS
        .into_iter()
        .find(|context| context.target == "aarch64-unknown-linux-gnu")
        .unwrap();
    let recipe = render_release_recipe(&shared_release_recipe(), "inputs", context);
    let empty = format!("RELEASE_CARGO_JOBS=''\n{recipe}");
    let observed = run_release_recipe(&empty, context, "", "false", "fixture");
    assert_release_trace(&observed, context, "", "fixture");
    for failure in ["", "generator", "main", "zerocode", "zerorelay"] {
        let bounded = format!("RELEASE_CARGO_JOBS=2\n{recipe}");
        let observed = run_release_recipe(&bounded, context, failure, "false", "fixture");
        let expected: Vec<_> = expected_release_trace(context, failure, "fixture")
            .into_iter()
            .map(|line| line.replace("|build|", "|build|--jobs|2|"))
            .collect();
        assert_eq!(
            observed.status,
            Some(if failure.is_empty() { 0 } else { 83 })
        );
        assert_eq!(observed.trace, expected, "{}", observed.diagnostics);
        assert!(observed.evidence.is_none());
    }
    for jobs in ["0", "-1", "two", "2;true"] {
        let invalid = format!("RELEASE_CARGO_JOBS={jobs:?}\n{recipe}");
        let observed = run_release_recipe(&invalid, context, "", "false", "fixture");
        assert_eq!(observed.status, Some(2), "{}", observed.diagnostics);
        assert_eq!(observed.trace.len(), 1, "only feature resolution may run");
    }
}

#[test]
fn shared_release_recipe_evidence_refuses_empty_features_and_invalid_switch() {
    let context = RELEASE_RECIPE_CONTEXTS[0];
    let recipe = render_release_recipe(&shared_release_recipe(), "inputs", context);
    let stable_empty = run_release_recipe(&recipe, context, "", "false", "");
    assert_release_trace(&stable_empty, context, "", "");
    assert!(stable_empty.evidence.is_none());
    for (switch, features) in [("true", ""), ("", "fixture-a"), ("maybe", "fixture-a")] {
        let refused = run_release_recipe(&recipe, context, "", switch, features);
        assert_eq!(refused.status, Some(2), "{}", refused.diagnostics);
        assert!(refused.evidence.is_none());
        assert!(
            refused.trace.len() <= 1,
            "no payload build after adapter refusal"
        );
    }
    let missing = ReleaseRecipeContext {
        target: "",
        ..context
    };
    let recipe = render_release_recipe(&shared_release_recipe(), "inputs", missing);
    let refused = run_release_recipe(&recipe, missing, "", "false", "fixture-a");
    assert_eq!(refused.status, Some(2), "{}", refused.diagnostics);
    assert!(
        refused.trace.is_empty(),
        "missing target must precede resolution"
    );
    assert!(refused.evidence.is_none());
}

#[test]
fn shared_release_recipe_behavior_oracle_rejects_actual_recipe_mutations() {
    let body = shared_release_recipe();
    for (context, from, to) in [
        (
            0,
            "--release --locked --no-default-features",
            "--release --no-default-features",
        ),
        (0, "--no-default-features --features", "--features"),
        (0, "--release --locked", "--profile dev --locked"),
        (
            0,
            "BUILD_CMD=\"cargo build\"",
            "BUILD_CMD=\"cargo build --jobs 2\"",
        ),
        (1, "BUILD_CMD=\"cross build\"", "BUILD_CMD=\"cargo build\""),
        (
            2,
            "$RELEASE_LINKER_ENV=$RELEASE_LINKER",
            "$RELEASE_LINKER_ENV=ignored-linker",
        ),
        (5, "-C target-feature=-neon", "-C target-feature=+neon"),
        (
            2,
            "--target \"$RELEASE_TARGET\"",
            "--target \"x86_64-unknown-linux-gnu\"",
        ),
        (
            8,
            "!= \"aarch64-linux-android\"",
            "= \"aarch64-linux-android\"",
        ),
        (0, "-p zerocode", "-p zerorelay"),
    ] {
        let changed = body.replacen(from, to, 1);
        assert_ne!(
            changed, body,
            "mutation must reach real recipe text: {from}"
        );
        let context = RELEASE_RECIPE_CONTEXTS[context];
        let recipe = render_release_recipe(&changed, "inputs", context);
        let observed = run_release_recipe(&recipe, context, "", "false", "fixture-a,fixture-b");
        assert!(
            observed.status != Some(0)
                || observed.trace != expected_release_trace(context, "", "fixture-a,fixture-b"),
            "behavior oracle missed mutation {from:?} -> {to:?}: {}",
            observed.diagnostics
        );
    }
    // Masking each real failure must violate status/early-termination evidence,
    // not merely a string-presence assertion.
    for (stage, needle) in [
        (
            "generator",
            "FEATURES=\"$(cargo run --quiet -p xtask --bin generate -- features --selection dist --target \"$RELEASE_TARGET\")\"",
        ),
        (
            "main",
            "$BUILD_CMD --release --locked --no-default-features --features \"${FEATURES}\" --target \"$RELEASE_TARGET\"",
        ),
        (
            "zerocode",
            "$BUILD_CMD --release --locked -p zerocode --target \"$RELEASE_TARGET\"",
        ),
        (
            "zerorelay",
            "$BUILD_CMD --release --locked -p zerorelay --target \"$RELEASE_TARGET\"",
        ),
    ] {
        let changed = body.replacen(needle, &format!("{needle} || true"), 1);
        assert_ne!(changed, body, "failure mutation must reach {stage}");
        let context = RELEASE_RECIPE_CONTEXTS[0];
        let recipe = render_release_recipe(&changed, "inputs", context);
        let observed = run_release_recipe(&recipe, context, stage, "false", "fixture-a,fixture-b");
        assert!(
            observed.status != Some(83)
                || observed.trace != expected_release_trace(context, stage, "fixture-a,fixture-b"),
            "behavior oracle missed {stage} failure suppression: {}",
            observed.diagnostics
        );
    }
}

#[test]
fn shared_release_recipe_adapter_guards_have_failure_controls() {
    let body = shared_release_recipe();
    for (from, to, target, switch, features) in [
        (
            "if [ -z \"$RELEASE_TARGET\" ]; then",
            "if false; then",
            "",
            "false",
            "fixture-a",
        ),
        (
            "true|false) ;;",
            "true|false|maybe) ;;",
            "x86_64-unknown-linux-gnu",
            "maybe",
            "fixture-a",
        ),
        (
            "if [ -z \"$FEATURES\" ]; then",
            "if false; then",
            "x86_64-unknown-linux-gnu",
            "true",
            "",
        ),
    ] {
        let changed = body.replacen(from, to, 1);
        assert_ne!(changed, body, "guard mutation must reach {from}");
        let context = ReleaseRecipeContext {
            target,
            ..RELEASE_RECIPE_CONTEXTS[0]
        };
        let recipe = render_release_recipe(&changed, "inputs", context);
        let observed = run_release_recipe(&recipe, context, "", switch, features);
        assert_ne!(
            observed.status,
            Some(2),
            "disabled guard must falsify its refusal: {from}: {}",
            observed.diagnostics
        );
    }
    let changed = body.replacen(
        "if [ \"$RELEASE_FEATURES_EVIDENCE\" = \"true\" ]; then",
        "if true; then",
        1,
    );
    assert_ne!(changed, body, "emission guard mutation reaches the recipe");
    let context = RELEASE_RECIPE_CONTEXTS[0];
    let recipe = render_release_recipe(&changed, "inputs", context);
    let observed = run_release_recipe(&recipe, context, "", "false", "fixture-a");
    assert_ne!(
        observed.status,
        Some(0),
        "Stable's absent output file must expose an unguarded write: {}",
        observed.diagnostics
    );
}

#[test]
fn release_payload_callers_bind_the_actual_recipe_and_reject_binding_mutants() {
    let stable = workflow("release-stable-manual.yml");
    let manual = workflow("cross-platform-build-manual.yml");
    let body = shared_release_recipe();
    for (document, job, emit, arm64_targets) in [
        (&stable, "  build:\n", "false", STABLE_ARM64_TARGETS),
        (&manual, "  stable-gnu-build:\n", "true", GNU_ARM64_TARGET),
    ] {
        let caller = yaml_block(yaml_block(document, job), "      - name: Build release\n");
        assert_release_caller(caller, emit, arm64_targets);
        for (before, after) in [
            ("target: ${{ matrix.target }}", "target: ${{ matrix.os }}"),
            ("linker: ${{ matrix.linker || '' }}", "linker: ''"),
            (
                "use_cross: ${{ matrix.use_cross || 'false' }}",
                "use_cross: 'true'",
            ),
            (
                &format!("emit_features: '{emit}'"),
                &format!(
                    "emit_features: '{}'",
                    if emit == "true" { "false" } else { "true" }
                ),
            ),
        ] {
            assert!(caller.contains(before));
            let mutant = caller.replacen(before, after, 1);
            assert!(
                std::panic::catch_unwind(|| assert_release_caller(&mutant, emit, arm64_targets))
                    .is_err(),
                "the actual caller binding mutant must be refused: {before}"
            );
        }
        for context in RELEASE_RECIPE_CONTEXTS {
            if emit == "true"
                && !matches!(
                    context.target,
                    "x86_64-unknown-linux-gnu"
                        | "aarch64-unknown-linux-gnu"
                        | "armv7-unknown-linux-gnueabihf"
                        | "arm-unknown-linux-gnueabihf"
                )
            {
                continue;
            }
            let recipe = render_release_recipe(&body, "inputs", context);
            let observed = run_release_recipe(&recipe, context, "", emit, "caller-a,caller-b");
            assert_release_trace(&observed, context, "", "caller-a,caller-b");
            assert_eq!(
                observed.evidence.as_deref(),
                (emit == "true").then_some("features=caller-a,caller-b\n")
            );
        }
    }
}

#[test]
fn manual_qualification_admission_refuses_incompatible_and_malformed_inputs() {
    let manual = workflow("cross-platform-build-manual.yml");
    let guard = explicit_bash_body(
        yaml_block(
            yaml_block(&manual, "  release-tool-smoke:\n"),
            "      - name: Validate manual qualification inputs\n",
        ),
        "          ",
    );
    let check = |body: &str, profile: &str, distribution: &str, tools: &str| {
        let script = format!(
            "QUALIFICATION_PROFILE='{profile}'\nDISTRIBUTION='{distribution}'\nRELEASE_TOOLS_ONLY='{tools}'\n{body}"
        );
        run_release_recipe(&script, RELEASE_RECIPE_CONTEXTS[0], "", "false", "")
    };
    for (profile, distribution, tools) in [
        ("normal", "dist", "false"),
        ("normal", "dist-broad", "false"),
        ("normal", "dist", "true"),
        ("normal", "dist-broad", "true"),
        ("stable-gnu-ubuntu22", "dist", "false"),
    ] {
        let observed = check(&guard, profile, distribution, tools);
        assert_eq!(observed.status, Some(0), "{}", observed.diagnostics);
    }
    for (profile, distribution, tools) in [
        ("", "dist", "false"),
        ("unknown", "dist", "false"),
        (" normal", "dist", "false"),
        ("normal", "", "false"),
        ("normal", "dist ", "false"),
        ("normal", "dist", ""),
        ("normal", "dist", "False"),
        ("stable-gnu-ubuntu22", "dist-broad", "false"),
        ("stable-gnu-ubuntu22", "dist", "true"),
    ] {
        let observed = check(&guard, profile, distribution, tools);
        assert_eq!(observed.status, Some(2), "{}", observed.diagnostics);
        assert!(observed.trace.is_empty());
    }
    for (before, after, profile, distribution, tools) in [
        (
            "normal|stable-gnu-ubuntu22)",
            "*)",
            "unknown",
            "dist",
            "false",
        ),
        ("dist|dist-broad)", "*)", "normal", "unknown", "false"),
        ("true|false)", "*)", "normal", "dist", "False"),
        (
            r#"[ "$DISTRIBUTION" != dist ]"#,
            r#"[ "$DISTRIBUTION" = dist ]"#,
            "stable-gnu-ubuntu22",
            "dist-broad",
            "false",
        ),
        (
            r#"[ "$RELEASE_TOOLS_ONLY" != false ]"#,
            r#"[ "$RELEASE_TOOLS_ONLY" = false ]"#,
            "stable-gnu-ubuntu22",
            "dist",
            "true",
        ),
    ] {
        assert!(guard.contains(before));
        let observed = check(
            &guard.replacen(before, after, 1),
            profile,
            distribution,
            tools,
        );
        assert_eq!(
            observed.status,
            Some(0),
            "guard mutant must expose refusal loss"
        );
    }
}

#[test]
fn manual_qualification_completeness_requires_every_actual_role_success() {
    let manual = workflow("cross-platform-build-manual.yml");
    let job = yaml_block(&manual, "  stable-gnu-complete:\n");
    assert!(
        job.contains(
            "if: ${{ always() && inputs.qualification_profile == 'stable-gnu-ubuntu22' }}"
        )
    );
    assert!(job.contains(
        "needs: [release-tool-smoke, stable-gnu-web, stable-gnu-plugin-smoke-packages, stable-gnu-build, stable-gnu-plugin-smoke]"
    ));
    let step = yaml_block(job, "      - name: Require selected role success\n");
    for (env, role) in [
        ("TOOLS_RESULT", "release-tool-smoke"),
        ("WEB_RESULT", "stable-gnu-web"),
        ("FIXTURES_RESULT", "stable-gnu-plugin-smoke-packages"),
        ("BUILD_RESULT", "stable-gnu-build"),
        ("NATIVE_RESULT", "stable-gnu-plugin-smoke"),
    ] {
        assert!(step.contains(&format!("{env}: ${{{{ needs.{role}.result }}}}")));
    }
    let guard = explicit_bash_body(step, "          ");
    for position in 0..5 {
        for result in ["success", "", "skipped", "failure", "cancelled"] {
            let mut results = ["success"; 5];
            results[position] = result;
            let script = format!(
                "TOOLS_RESULT='{}'\nWEB_RESULT='{}'\nFIXTURES_RESULT='{}'\nBUILD_RESULT='{}'\nNATIVE_RESULT='{}'\n{}",
                results[0], results[1], results[2], results[3], results[4], guard
            );
            let observed = run_release_recipe(&script, RELEASE_RECIPE_CONTEXTS[0], "", "false", "");
            assert_eq!(
                observed.status,
                Some(if result == "success" { 0 } else { 2 })
            );
        }
    }
    let mutant = guard.replacen("success/success/success/success/success)", "*)", 1);
    assert_ne!(guard, mutant);
    let script = format!(
        "TOOLS_RESULT=success\nWEB_RESULT=success\nFIXTURES_RESULT=success\nBUILD_RESULT=skipped\nNATIVE_RESULT=success\n{mutant}"
    );
    let observed = run_release_recipe(&script, RELEASE_RECIPE_CONTEXTS[0], "", "false", "");
    assert_eq!(
        observed.status,
        Some(0),
        "mutant must expose skipped-build acceptance"
    );
}

fn assert_manual_qualification_wiring(manual: &str) {
    for (job_id, role) in [
        ("stable-gnu-web", "web"),
        ("stable-gnu-plugin-smoke-packages", "plugin-smoke-packages"),
        ("stable-gnu-build", "build"),
    ] {
        let job = yaml_block(manual, &format!("  {job_id}:\n"));
        assert!(job.contains("needs: [release-tool-smoke"));
        assert!(job.contains("if: ${{ inputs.qualification_profile == 'stable-gnu-ubuntu22' }}"));
        assert!(job.contains("dtolnay/rust-toolchain@67ef31d5b988238dd797d409d6f9574278e20537"));
        assert!(job.contains("toolchain: 1.98.0"));
        assert!(!job.contains("CARGO_INCREMENTAL:"));
        let receipt_header = match role {
            "web" => "      - name: Record same-run web inputs\n",
            "plugin-smoke-packages" => "      - name: Record same-run fixture inputs\n",
            _ => "      - name: Bind archive qualification receipts\n",
        };
        let receipt = explicit_bash_body(yaml_block(job, receipt_header), "          ");
        assert!(receipt.contains(r#"assert os.environ["CARGO_INCREMENTAL"] == "0""#));
        assert!(receipt.contains(r#""cargo_incremental": os.environ["CARGO_INCREMENTAL"]"#));
        assert!(!receipt.contains(".removeprefix("));
        let cache = yaml_block(
            job,
            "      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2\n",
        );
        let prefix = match role {
            "build" => "          prefix-key: ${{ matrix.os }}-${{ matrix.target }}\n",
            "plugin-smoke-packages" => "          prefix-key: plugin-smoke-packages\n",
            _ => "",
        };
        assert_eq!(
            yaml_block(cache, "        with:\n").trim_end(),
            format!("          shared-key: {role}\n{prefix}          save-if: false")
        );
    }
    let build = yaml_block(manual, "  stable-gnu-build:\n");
    assert!(build.contains("needs: [release-tool-smoke, stable-gnu-web]"));
    assert!(build.contains("timeout-minutes: 90"));
    assert_eq!(build.matches("- os: ubuntu-22.04").count(), 4);
    // GNU ARM64: run 38836146884 and the 10/10 helper versus 0/5 baseline require the existing bounded helper.
    assert_arm64_build_memory(build, GNU_ARM64_TARGET);
    assert!(!build.contains("--jobs"));
    assert!(!build.contains("cache-targets:"));
    assert_release_caller(
        yaml_block(build, "      - name: Build release\n"),
        "true",
        GNU_ARM64_TARGET,
    );
    let inputs = yaml_block(
        build,
        "      - name: Record input manifest and ABI observations\n",
    );
    assert!(inputs.contains("FEATURES: ${{ steps.payloads.outputs.features }}"));
    let body = explicit_bash_body(inputs, "          ");
    assert!(body.contains("test -n \"$FEATURES\"\n"));
    assert!(
        body.contains("--features \"$FEATURES\" --output archive-evidence/input-manifest.json")
    );
    assert_eq!(body.matches("--companion ").count(), 2);
    assert!(body.contains("for name in zeroclaw zerocode zerorelay; do"));
    assert!(body.contains("readelf --file-header --program-headers --dynamic --version-info"));
    assert!(body.contains("\"$compiler\" -print-sysroot"));
    assert!(build.contains("BINARY_SIZE_HARD_LIMIT: \"83886080\""));
    let size = build
        .find("- name: Check standard main binary size")
        .unwrap();
    let manifest = build
        .find("- name: Record input manifest and ABI observations")
        .unwrap();
    let package = build
        .find("- name: Package and verify standard release archive")
        .unwrap();
    let receipt = build
        .find("- name: Bind archive qualification receipts")
        .unwrap();
    assert!(size < manifest && manifest < package && package < receipt);
    let native = yaml_block(manual, "  stable-gnu-plugin-smoke:\n");
    assert!(native.contains(
        "needs: [release-tool-smoke, stable-gnu-build, stable-gnu-plugin-smoke-packages]"
    ));
    assert!(native.contains("timeout-minutes: 30"));
    assert!(native.contains("- os: ubuntu-22.04\n            target: x86_64-unknown-linux-gnu"));
    assert!(
        native.contains("- os: ubuntu-24.04-arm\n            target: aarch64-unknown-linux-gnu")
    );
    assert_eq!(matrix_targets(native).len(), 2);
    let provider = yaml_block(native, "      - name: Run scripted provider tests\n");
    assert!(provider.contains("timeout-minutes: 5"));
    assert!(provider.contains(
        "scripts/ci/plugin_smoke_provider_test.py -v 2>&1 | tee native/provider-tests.log"
    ));
    assert!(provider.contains("set -euo pipefail"));
    let smoke = yaml_block(
        native,
        "      - name: Run the plugin smoke against the built binary\n",
    );
    assert!(!smoke.contains("if:") && !smoke.contains("continue-on-error"));
    assert!(smoke.contains("--archive \"${{ steps.archive.outputs.archive }}\""));
    assert!(!native.contains("inputs.distribution") && !native.contains("path: binary"));
    assert!(
        native.find("- name: Run scripted provider tests").unwrap()
            < native
                .find("- name: Run the plugin smoke against the built binary")
                .unwrap()
    );
    let complete = yaml_block(manual, "  stable-gnu-complete:\n");
    assert!(complete.contains("needs: [release-tool-smoke, stable-gnu-web, stable-gnu-plugin-smoke-packages, stable-gnu-build, stable-gnu-plugin-smoke]"));
    assert!(
        complete.contains(
            "if: ${{ always() && inputs.qualification_profile == 'stable-gnu-ubuntu22' }}"
        )
    );
    assert!(complete.contains("assert inputs == 12"));
    assert!(complete.contains(r#"assert receipt["cache"]["cargo_incremental"] == "0""#));
    assert!(!complete.contains(".removeprefix("));
    assert!(complete.contains("scripts/ci/verify_release_archive.py"));
    assert!(!manual.contains("contents: write") && !manual.contains("secrets."));
}

#[test]
fn stable_and_qualification_builds_share_the_actual_cache_prefix() {
    let release = workflow("release-stable-manual.yml");
    let manual = workflow("cross-platform-build-manual.yml");
    let cache_header =
        "      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2\n";
    let stable_cache = yaml_block(yaml_block(&release, "  build:\n"), cache_header);
    let qualifier_cache = yaml_block(yaml_block(&manual, "  stable-gnu-build:\n"), cache_header);
    let stable_prefix = yaml_block(stable_cache, "        with:\n")
        .lines()
        .find_map(|line| line.trim().strip_prefix("prefix-key: "))
        .expect("the actual Stable build must declare its cache prefix");
    let qualifier_prefix = yaml_block(qualifier_cache, "        with:\n")
        .lines()
        .find_map(|line| line.trim().strip_prefix("prefix-key: "))
        .expect("the actual qualifier build must declare its cache prefix");
    assert_eq!(
        stable_prefix, qualifier_prefix,
        "qualification must restore from the actual Stable cache namespace"
    );
    assert_eq!(stable_prefix, "${{ matrix.os }}-${{ matrix.target }}");
}

#[test]
fn manual_qualification_wiring_rejects_actual_cache_output_and_dependency_mutants() {
    let manual = workflow("cross-platform-build-manual.yml");
    assert_manual_qualification_wiring(&manual);
    for (before, after) in [
        ("shared-key: build", "shared-key: other"),
        ("shared-key: web", "shared-key: other"),
        ("shared-key: plugin-smoke-packages", "shared-key: other"),
        (
            "prefix-key: ${{ matrix.os }}-${{ matrix.target }}\n          save-if: false",
            "prefix-key: wrong\n          save-if: false",
        ),
        (
            "shared-key: build\n          prefix-key:",
            "shared-key: build\n          cache-targets: false\n          prefix-key:",
        ),
        (
            "FEATURES: ${{ steps.payloads.outputs.features }}",
            "FEATURES: ${{ steps.payloads.outputs.other }}",
        ),
        ("test -n \"$FEATURES\"\n", "true\n"),
        (
            "needs: [release-tool-smoke, stable-gnu-web, stable-gnu-plugin-smoke-packages, stable-gnu-build, stable-gnu-plugin-smoke]",
            "needs: [release-tool-smoke, stable-gnu-web, stable-gnu-plugin-smoke-packages, stable-gnu-plugin-smoke]",
        ),
    ] {
        assert!(manual.contains(before));
        let mutant = manual.replacen(before, after, 1);
        assert!(
            std::panic::catch_unwind(|| assert_manual_qualification_wiring(&mutant)).is_err(),
            "actual qualifier wiring mutant must fail: {before}"
        );
    }
    let inputs = explicit_bash_body(
        yaml_block(
            yaml_block(&manual, "  stable-gnu-build:\n"),
            "      - name: Record input manifest and ABI observations\n",
        ),
        "          ",
    )
    .replace("${{ matrix.target }}", RELEASE_RECIPE_CONTEXTS[0].target)
    .replace("${{ matrix.linker || 'gcc' }}", "gcc");
    assert!(!inputs.contains("${{"));
    let observe = |body: &str| {
        let script = format!(
            "FEATURES=''\nGITHUB_SHA=fixture-source\nGITHUB_RUN_ID=17\nGITHUB_RUN_ATTEMPT=2\npython3() {{ echo 'controlled manifest boundary'; return 91; }}\n{body}"
        );
        run_release_recipe(&script, RELEASE_RECIPE_CONTEXTS[0], "", "false", "")
    };
    let refused = observe(&inputs);
    assert_eq!(refused.status, Some(1), "{}", refused.diagnostics);
    assert!(!refused.diagnostics.contains("controlled manifest boundary"));
    let mutant = inputs.replacen("test -n \"$FEATURES\"\n", "true\n", 1);
    assert_ne!(mutant, inputs);
    let exposed = observe(&mutant);
    assert_eq!(exposed.status, Some(91), "{}", exposed.diagnostics);
    assert!(exposed.diagnostics.contains("controlled manifest boundary"));
    for (job, step) in [
        ("stable-gnu-web", "Record same-run web inputs"),
        (
            "stable-gnu-plugin-smoke-packages",
            "Record same-run fixture inputs",
        ),
        ("stable-gnu-build", "Bind archive qualification receipts"),
    ] {
        let body = explicit_bash_body(
            yaml_block(
                yaml_block(&manual, &format!("  {job}:\n")),
                &format!("      - name: {step}\n"),
            ),
            "          ",
        );
        let producer = body
            .strip_prefix("python3 - <<'PY'\n")
            .and_then(|body| body.strip_suffix("PY\n"))
            .expect("one finite producer receipt body");
        let observe = |body: &str, incremental: Option<&str>| {
            let state = match incremental {
                Some(value) => format!("os.environ['CARGO_INCREMENTAL'] = {value:?}"),
                None => "os.environ.pop('CARGO_INCREMENTAL', None)".to_owned(),
            };
            let script = format!(
                "python_program=''\nfor candidate in python3 python; do\n  if command -v \"$candidate\" >/dev/null 2>&1 && \"$candidate\" -c 'import sys; sys.exit(sys.version_info < (3, 8))' 2>/dev/null; then python_program=\"$candidate\"; break; fi\ndone\ntest -n \"$python_program\"\n\"$python_program\" - <<'PY'\nimport os, subprocess\n{state}\ndef revision_boundary(*args, **kwargs):\n    raise SystemExit(91)\nsubprocess.check_output = revision_boundary\n{body}\nPY\n"
            );
            run_release_recipe(&script, RELEASE_RECIPE_CONTEXTS[0], "", "false", "")
        };
        let accepted = observe(producer, Some("0"));
        assert_eq!(accepted.status, Some(91), "{job}: {}", accepted.diagnostics);
        for state in [None, Some(""), Some("1")] {
            let refused = observe(producer, state);
            assert_eq!(refused.status, Some(1), "{job}: {}", refused.diagnostics);
        }
        let mutant = producer.replacen(
            r#"assert os.environ["CARGO_INCREMENTAL"] == "0""#,
            "pass",
            1,
        );
        assert_ne!(mutant, producer);
        let exposed = observe(&mutant, Some("1"));
        assert_eq!(exposed.status, Some(91), "{job}: {}", exposed.diagnostics);
    }
}

#[test]
fn manual_qualification_inventory_refuses_missing_or_rebound_actual_receipts() {
    let manual = workflow("cross-platform-build-manual.yml");
    let body = explicit_bash_body(
        yaml_block(
            yaml_block(&manual, "  stable-gnu-complete:\n"),
            "      - name: Verify finite same-run qualification inventory\n",
        ),
        "          ",
    );
    let inventory = body
        .strip_prefix("python3 - <<'PY'\n")
        .and_then(|body| body.strip_suffix("PY\n"))
        .expect("the finite inventory has one inline Python body");
    let verifier = bash_path(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ci/verify_release_archive.py"),
    );
    // Opaque synthetic payloads exercise receipt and archive verification only.
    // No provider, payload, compiler, or resource helper runs in this fixture.
    let preparation = r###"
import hashlib, io, json, os, pathlib, shutil, subprocess, sys, tarfile
os.environ.update(GITHUB_SHA="fixture-source", GITHUB_RUN_ID="17", GITHUB_RUN_ATTEMPT="2")
binding = {"source": "fixture-source", "tree": "fixture-tree", "run": 17, "attempt": 2, "profile": "stable-gnu-ubuntu22"}
targets = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "armv7-unknown-linux-gnueabihf", "arm-unknown-linux-gnueabihf")
def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True, indent=2) + "\n")
def hashed(data):
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
def hashed_file(path):
    return hashed(path.read_bytes())
def replace_record(path, change):
    value = json.loads(path.read_text())
    change(value)
    write(path, value)
pathlib.Path("scripts/ci").mkdir(parents=True)
shutil.copyfile(VERIFIER_PATH, "scripts/ci/verify_release_archive.py")
web = pathlib.Path("producers/web/qualification-web.json")
fixtures = pathlib.Path("producers/fixtures/qualification-fixtures.json")
web_files = {"index.html": hashed(b"fixture web")}
for path, role, files in (
    (web, "web", web_files),
    (fixtures, "plugin-smoke-packages", {"fixture.wasm": hashed(b"fixture package")}),
):
    write(path, {"binding": binding, "role": role, "files": files, "cache": {"shared_key": role, "save_if": False, "cargo_incremental": "0"}})
for target in targets:
    root = pathlib.Path("archives") / ("stable-gnu-ubuntu22-archive-" + target)
    evidence = root / "archive-evidence"
    payloads = {name: ("fixture " + name).encode() for name in ("zeroclaw", "zerocode", "zerorelay")}
    payloads["web/dist/index.html"] = b"fixture web"
    files = {name: hashed(data) for name, data in payloads.items()}
    manifest = evidence / "input-manifest.json"
    write(manifest, {"format": "zeroclaw-release-archive-evidence-v1", "source": binding["source"], "target": target, "run": binding["run"], "attempt": binding["attempt"], "features": "fixture-a", "main": "zeroclaw", "files": files, "directories": ["web", "web/dist"]})
    archive = root / ("zeroclaw-" + target + ".tar.gz")
    with tarfile.open(archive, "w:gz") as output:
        for name, data in payloads.items():
            member = tarfile.TarInfo(name)
            member.size, member.mode = len(data), 0o755
            output.addfile(member, io.BytesIO(data))
    validation = evidence / "validation.json"
    write(validation, {"fixture": "opaque validation carrier"})
    observations = {}
    for name in ("zeroclaw", "zerocode", "zerorelay"):
        path = evidence / "abi" / (name + ".txt")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("fixture observation")
        observations[name] = {"input": files[name], "output": hashed_file(path)}
    write(evidence / "qualification.json", {"binding": binding, "role": "build", "target": target, "features": "fixture-a", "manifest": hashed_file(manifest), "archive": hashed_file(archive), "validation": hashed_file(validation), "web_receipt": hashed_file(web), "observations": observations, "cache": {"shared_key": "build", "prefix_key": "ubuntu-22.04-" + target, "save_if": False, "cargo_incremental": "0"}})
for target, arch in ((targets[0], "X64"), (targets[1], "ARM64")):
    root = pathlib.Path("natives") / ("stable-gnu-ubuntu22-native-" + target)
    root.mkdir(parents=True)
    archive_root = pathlib.Path("archives") / ("stable-gnu-ubuntu22-archive-" + target)
    for name in ("archive-validation.json", "provider-tests.log", "plugin-smoke.md"):
        (root / name).write_text("fixture carrier")
    write(root / "qualification.json", {"binding": binding, "role": "native", "target": target, "runner_os": "Linux", "runner_arch": arch, "manifest": hashed_file(archive_root / "archive-evidence/input-manifest.json"), "archive": hashed_file(archive_root / ("zeroclaw-" + target + ".tar.gz")), "fixtures_receipt": hashed_file(fixtures), "validation": hashed_file(root / "archive-validation.json"), "provider_log": hashed_file(root / "provider-tests.log"), "smoke_evidence": hashed_file(root / "plugin-smoke.md")})
build_root = pathlib.Path("archives") / ("stable-gnu-ubuntu22-archive-" + targets[0])
build_receipt = build_root / "archive-evidence/qualification.json"
native_root = pathlib.Path("natives") / ("stable-gnu-ubuntu22-native-" + targets[0])
if MUTATION == "missing-archive":
    shutil.rmtree(build_root)
elif MUTATION == "missing-native":
    shutil.rmtree(native_root)
elif MUTATION == "bad-build-source":
    replace_record(build_receipt, lambda value: value["binding"].update(source="other-source"))
elif MUTATION == "bad-cache":
    replace_record(build_receipt, lambda value: value["cache"].update(shared_key="other-role"))
elif MUTATION == "bad-incremental":
    replace_record(build_receipt, lambda value: value["cache"].update(cargo_incremental="1"))
elif MUTATION == "missing-incremental":
    replace_record(build_receipt, lambda value: value["cache"].pop("cargo_incremental"))
elif MUTATION == "integer-incremental":
    replace_record(build_receipt, lambda value: value["cache"].update(cargo_incremental=0))
elif MUTATION == "bad-input":
    replace_record(build_receipt, lambda value: value["observations"]["zeroclaw"]["input"].update(sha256="0" * 64))
elif MUTATION == "bad-output-digest":
    replace_record(build_receipt, lambda value: value["observations"]["zeroclaw"]["output"].update(sha256="0" * 64))
elif MUTATION == "bad-archive-digest":
    replace_record(build_receipt, lambda value: value["archive"].update(sha256="0" * 64))
elif MUTATION == "bad-provider-log-digest":
    replace_record(native_root / "qualification.json", lambda value: value["provider_log"].update(sha256="0" * 64))
actual_output = subprocess.check_output
def controlled_revision(args, **kwargs):
    if args == ["git", "rev-parse", "HEAD"]:
        return "fixture-source\n"
    if args == ["git", "rev-parse", "HEAD^{tree}"]:
        return "fixture-tree\n"
    return actual_output(args, **kwargs)
subprocess.check_output = controlled_revision
actual_run = subprocess.run
def existing_verifier(args, **kwargs):
    assert args[:3] == ["python3", "scripts/ci/verify_release_archive.py", "verify"]
    return actual_run([sys.executable] + args[1:], **kwargs)
subprocess.run = existing_verifier
"###;
    let check = |inventory: &str, mutation: &str| {
        let preparation = preparation.replace("VERIFIER_PATH", &format!("{verifier:?}"));
        let python_selection = r#"python_program=''
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1 \
    && "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 8))' 2>/dev/null; then
    python_program="$candidate"
    break
  fi
done
test -n "$python_program"
"#;
        let script = format!(
            "{python_selection}\"$python_program\" - <<'PY'\nMUTATION={mutation:?}\n{preparation}\n{inventory}\nPY\n"
        );
        run_release_recipe(&script, RELEASE_RECIPE_CONTEXTS[0], "", "false", "")
    };
    let accepted = check(inventory, "");
    assert_eq!(accepted.status, Some(0), "{}", accepted.diagnostics);
    for mutation in [
        "missing-archive",
        "missing-native",
        "bad-build-source",
        "bad-cache",
        "bad-incremental",
        "missing-incremental",
        "integer-incremental",
        "bad-input",
        "bad-output-digest",
        "bad-archive-digest",
        "bad-provider-log-digest",
    ] {
        let refused = check(inventory, mutation);
        assert_eq!(
            refused.status,
            Some(1),
            "{mutation}: {}",
            refused.diagnostics
        );
    }
    for (before, after, mutation) in [
        (
            r#"assert receipt["binding"] == binding and receipt["role"] == "build" and receipt["target"] == target"#,
            r#"assert receipt["role"] == "build" and receipt["target"] == target"#,
            "bad-build-source",
        ),
        (
            r#"assert receipt["cache"]["shared_key"] == "build" and receipt["cache"]["prefix_key"] == "ubuntu-22.04-" + target"#,
            r#"assert receipt["cache"]["prefix_key"] == "ubuntu-22.04-" + target"#,
            "bad-cache",
        ),
        (
            r#"assert receipt["cache"]["save_if"] is False and receipt["cache"]["cargo_incremental"] == "0""#,
            r#"assert receipt["cache"]["save_if"] is False"#,
            "bad-incremental",
        ),
        (
            r#"assert observation["input"] == manifest["files"][name]"#,
            "pass",
            "bad-input",
        ),
        (
            r#"assert observation["output"] == record(evidence / "abi" / (name + ".txt"))"#,
            "pass",
            "bad-output-digest",
        ),
        (
            r#"assert receipt["manifest"] == record(manifest_path) and receipt["archive"] == record(archive)"#,
            r#"assert receipt["manifest"] == record(manifest_path)"#,
            "bad-archive-digest",
        ),
        (
            r#"assert receipt["provider_log"] == record(root / "provider-tests.log") and receipt["smoke_evidence"] == record(root / "plugin-smoke.md")"#,
            r#"assert receipt["smoke_evidence"] == record(root / "plugin-smoke.md")"#,
            "bad-provider-log-digest",
        ),
    ] {
        assert!(inventory.contains(before));
        let mutant = inventory.replacen(before, after, 1);
        let exposed = check(&mutant, mutation);
        assert_eq!(
            exposed.status,
            Some(0),
            "guard mutant {mutation}: {}",
            exposed.diagnostics
        );
    }
}

fn arm64_baseline_python(manual: &str) -> String {
    let build = yaml_block(manual, "  stable-gnu-build:\n");
    let step = yaml_block(build, "      - name: Capture GNU ARM64 runner baseline\n");
    explicit_bash_body(step, "          ")
        .strip_prefix("python3 - <<'PY'\n")
        .and_then(|body| body.strip_suffix("PY\n"))
        .expect("baseline uses one fixed inline Python body")
        .to_string()
}

fn assert_arm64_baseline_wiring(manual: &str) {
    let build = yaml_block(manual, "  stable-gnu-build:\n");
    let capture = yaml_block(build, "      - name: Capture GNU ARM64 runner baseline\n");
    let upload = yaml_block(
        build,
        "      - name: Upload GNU ARM64 runner baseline before build\n",
    );
    assert_eq!(
        build
            .matches("- name: Capture GNU ARM64 runner baseline\n")
            .count(),
        1
    );
    assert_eq!(
        build
            .matches("- name: Upload GNU ARM64 runner baseline before build\n")
            .count(),
        1
    );
    assert_eq!(
        capture
            .split_once("        run: |\n")
            .expect("literal capture body")
            .0,
        r##"        if: matrix.target == 'aarch64-unknown-linux-gnu'
        timeout-minutes: 1
        shell: bash
        env:
          BASELINE_SOURCE: ${{ github.sha }}
          BASELINE_RUN_ID: ${{ github.run_id }}
          BASELINE_RUN_ATTEMPT: ${{ github.run_attempt }}
          BASELINE_TARGET: ${{ matrix.target }}
          BASELINE_DISTRIBUTION: ${{ inputs.distribution }}
          BASELINE_PROFILE: ${{ inputs.qualification_profile }}
          BASELINE_RELEASE_TOOLS_ONLY: ${{ inputs.release_tools_only }}
"##
    );
    assert_eq!(
        upload.trim_end(),
        r##"        if: matrix.target == 'aarch64-unknown-linux-gnu'
        timeout-minutes: 2
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: runner-baseline-gnu-arm64-${{ github.run_id }}-${{ github.run_attempt }}
          path: |
            ${{ runner.temp }}/gnu-arm64-baseline-${{ github.run_id }}-${{ github.run_attempt }}/baseline.json
            ${{ runner.temp }}/gnu-arm64-baseline-${{ github.run_id }}-${{ github.run_attempt }}/baseline.sha256
          retention-days: 1
          if-no-files-found: error
          compression-level: 0"##
    );
    let compiler = build.find("- name: Install cross compiler\n").unwrap();
    let capture_start = build
        .find("- name: Capture GNU ARM64 runner baseline\n")
        .unwrap();
    let upload_start = build
        .find("- name: Upload GNU ARM64 runner baseline before build\n")
        .unwrap();
    let payloads = build.find("- name: Build release\n").unwrap();
    assert!(compiler < capture_start && capture_start < upload_start && upload_start < payloads);
    assert!(!build.contains("CARGO_BUILD_JOBS") && !build.contains("RUSTFLAGS:"));
    assert!(!capture.contains("GITHUB_ENV") && !capture.contains("continue-on-error"));
    let body = arm64_baseline_python(manual);
    for required in [
        "READ_LIMIT = 4096",
        "JSON_LIMIT = 16384",
        "deadline = time.monotonic() + 4\n",
        "child.wait(timeout=1)",
        "stderr=subprocess.DEVNULL",
        "os.O_EXCL | os.O_NOFOLLOW",
        "directory.mkdir(mode=0o700)",
        "os.umask(0o077)",
        "0o600",
        "raise SystemExit(\"ARM64 runner baseline capture refused\") from None",
    ] {
        assert!(body.contains(required), "baseline must retain {required}");
    }
}

#[test]
fn gnu_arm64_runner_baseline_wiring_refuses_scope_upload_and_budget_mutants() {
    let manual = workflow("cross-platform-build-manual.yml");
    assert_arm64_baseline_wiring(&manual);
    for (before, after) in [
        (
            "        if: matrix.target == 'aarch64-unknown-linux-gnu'\n",
            "        if: true\n",
        ),
        (
            "        timeout-minutes: 1\n",
            "        timeout-minutes: 2\n",
        ),
        (
            "        timeout-minutes: 2\n",
            "        timeout-minutes: 3\n",
        ),
        (
            "BASELINE_RUN_ATTEMPT: ${{ github.run_attempt }}",
            "BASELINE_RUN_ATTEMPT: '1'",
        ),
        (
            "BASELINE_SOURCE: ${{ github.sha }}",
            "BASELINE_SOURCE: other",
        ),
        (
            "BASELINE_PROFILE: ${{ inputs.qualification_profile }}",
            "BASELINE_PROFILE: normal",
        ),
        (
            "runner-baseline-gnu-arm64-${{ github.run_id }}-${{ github.run_attempt }}",
            "stable-gnu-ubuntu22-archive-aarch64-unknown-linux-gnu",
        ),
        ("/baseline.json\n", "/*\n"),
        (
            "          retention-days: 1\n",
            "          retention-days: 7\n",
        ),
        (
            "          if-no-files-found: error\n",
            "          if-no-files-found: warn\n",
        ),
        (
            "        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n",
            "        uses: actions/upload-artifact@main\n",
        ),
        ("READ_LIMIT = 4096", "READ_LIMIT = 8192"),
        ("JSON_LIMIT = 16384", "JSON_LIMIT = 32768"),
        (
            "deadline = time.monotonic() + 4",
            "deadline = time.monotonic() + 40",
        ),
        ("stderr=subprocess.DEVNULL", "stderr=subprocess.PIPE"),
        ("directory.mkdir(mode=0o700)", "directory.mkdir(mode=0o755)"),
        (
            "        shell: bash\n        env:\n          BASELINE_SOURCE:",
            "        continue-on-error: true\n        shell: bash\n        env:\n          BASELINE_SOURCE:",
        ),
        (
            "          BASELINE_SOURCE:",
            "          CARGO_BUILD_JOBS: 2\n          BASELINE_SOURCE:",
        ),
    ] {
        // Change only this GNU job: other existing uploads/retentions are unrelated.
        let build = yaml_block(&manual, "  stable-gnu-build:\n");
        assert!(
            build.contains(before),
            "mutation must reach actual source: {before}"
        );
        let mutant_build = build.replacen(before, after, 1);
        let mutant = manual.replacen(build, &mutant_build, 1);
        assert!(std::panic::catch_unwind(|| assert_arm64_baseline_wiring(&mutant)).is_err());
    }
    let build = yaml_block(&manual, "  stable-gnu-build:\n");
    let upload = yaml_block(
        build,
        "      - name: Upload GNU ARM64 runner baseline before build\n",
    );
    let unguarded = upload.replacen(
        "        if: matrix.target == 'aarch64-unknown-linux-gnu'\n",
        "        if: true\n",
        1,
    );
    let mutant = manual.replacen(upload, &unguarded, 1);
    assert!(std::panic::catch_unwind(|| assert_arm64_baseline_wiring(&mutant)).is_err());
    let upload_step =
        format!("      - name: Upload GNU ARM64 runner baseline before build\n{upload}");
    let moved = build.replacen(&upload_step, "", 1);
    let payloads = yaml_block(&moved, "      - name: Build release\n");
    let moved = moved.replacen(payloads, &format!("{payloads}{upload_step}"), 1);
    let mutant = manual.replacen(build, &moved, 1);
    assert!(std::panic::catch_unwind(|| assert_arm64_baseline_wiring(&mutant)).is_err());
}

#[cfg(unix)]
fn observe_arm64_baseline_fixture(source: &str) -> RecipeObservation {
    // Execute only the actual collector's Python with controlled metadata boundaries.
    // No real git/dpkg/resource/kernel/compiler/provider command runs in this fixture.
    let source = serde_json::to_string(source).expect("encode actual inline source");
    let fixture = r###"import contextlib, io, json, os, pathlib, re, stat, subprocess, tempfile, types
from unittest import mock

PRIVATE = "fixture-private-value"
PACKAGE_NAMES = (
    "gcc-aarch64-linux-gnu", "gcc-11-aarch64-linux-gnu",
    "g++-aarch64-linux-gnu", "g++-11-aarch64-linux-gnu",
    "binutils-aarch64-linux-gnu", "libc6-arm64-cross", "libc6-dev-arm64-cross",
)
FILES = {
    "/etc/os-release": b'ID=ubuntu\nVERSION_ID="22.04"\nPRIVATE=fixture-private-value\n',
    "/proc/meminfo": b"MemTotal: 16000 kB\nMemAvailable: 8000 kB\nSwapTotal: 3000 kB\nSwapFree: 2000 kB\nPrivate: fixture-private-value\n",
    "/sys/fs/cgroup/memory.max": b"max\n",
    "/sys/fs/cgroup/memory.current": b"1234\n",
    "/sys/fs/cgroup/memory.events": b"low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\noom_group_kill 0\nprivate fixture-private-value\n",
    "/sys/fs/cgroup/cpu.max": b"max 100000\n",
}

def namespace():
    result = {"__name__": "controlled_baseline"}
    exec(compile(SOURCE, "actual-workflow-baseline", "exec"), result)
    assert result["PACKAGES"] == PACKAGE_NAMES
    return result

def exercise(changes=None, malformed=False, unavailable=False, tiny=False, refuse=False, bad_tree=False):
    code = namespace()
    reads, commands = [], []
    with tempfile.TemporaryDirectory() as scratch:
        root = pathlib.Path(scratch)
        env = {
            "GITHUB_ACTIONS": "true", "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64",
            "GITHUB_SHA": "a" * 40, "BASELINE_SOURCE": "a" * 40,
            "GITHUB_RUN_ID": "17", "BASELINE_RUN_ID": "17",
            "GITHUB_RUN_ATTEMPT": "2", "BASELINE_RUN_ATTEMPT": "2",
            "GITHUB_JOB": "stable-gnu-build", "BASELINE_TARGET": "aarch64-unknown-linux-gnu",
            "BASELINE_DISTRIBUTION": "dist", "BASELINE_PROFILE": "stable-gnu-ubuntu22",
            "BASELINE_RELEASE_TOOLS_ONLY": "false", "RUNNER_TEMP": scratch,
            "ImageOS": "ubuntu22", "ImageVersion": "20261012.1", "PRIVATE": PRIVATE,
        }
        env.update(changes or {})
        def read(path, mode, *, buffering=-1):
            assert path in FILES and mode == "rb"
            assert buffering == 0, "metadata must use the unbuffered raw boundary"
            reads.append(path)
            if unavailable:
                raise FileNotFoundError("controlled missing metadata")
            data = FILES[path]
            if malformed:
                data = b"MemTotal: fixture-private-value\n" if path == "/proc/meminfo" else b"x" * 4096
            class BoundedRawFile(io.RawIOBase):
                def read(self, size=-1):
                    assert 0 < size <= 4096, "unbounded metadata read"
                    return data[:size]
            return BoundedRawFile()
        def command(args):
            commands.append(args)
            if args == ["git", "--no-optional-locks", "rev-parse", "HEAD"]:
                return "a" * 40 + "\n"
            if args == ["git", "--no-optional-locks", "rev-parse", "HEAD^{tree}"]:
                return PRIVATE if bad_tree else "b" * 40 + "\n"
            assert args == ["dpkg-query", "-W", "-f=${Package}\t${Version}\t${db:Status-Status}\n", *PACKAGE_NAMES]
            if unavailable:
                return code["unknown"]("unavailable")
            if malformed:
                return PRIVATE
            return "".join(name + "\t1.2.3\tinstalled\n" for name in PACKAGE_NAMES)
        code["open"], code["metadata_command"] = read, command
        if tiny:
            code["JSON_LIMIT"] = 1
        output = io.StringIO()
        saved_mask = os.umask(0)
        try:
            with mock.patch.dict(os.environ, env, clear=True), \
                 mock.patch.object(os, "cpu_count", return_value=4), \
                 mock.patch.object(os, "sched_getaffinity", return_value={0, 1, 2, 3}, create=True), \
                 mock.patch.object(os, "statvfs", return_value=types.SimpleNamespace(f_bavail=1024, f_frsize=4096)), \
                 contextlib.redirect_stdout(output):
                try:
                    code["main"]()
                except (ValueError, OSError):
                    assert refuse, "valid capture unexpectedly refused"
                    assert not list(root.rglob("baseline.json"))
                    return
            assert not refuse, "invalid binding/size was accepted"
            directory = root / "gnu-arm64-baseline-17-2"
            assert directory.is_dir(), "run/attempt directory must bind the actual identity"
            assert stat.S_IMODE(directory.stat().st_mode) == 0o700
            assert {path.name for path in directory.iterdir()} == {"baseline.json", "baseline.sha256"}
            payload = (directory / "baseline.json").read_bytes()
            assert len(payload) <= code["JSON_LIMIT"] <= 16384
            for path in directory.iterdir():
                info = path.stat()
                assert info.st_uid == os.geteuid() and info.st_nlink == 1
                assert stat.S_IMODE(info.st_mode) == 0o600
            report = json.loads(payload)
            assert report["binding"] == {"source": "a" * 40, "tree": "b" * 40,
                "run": 17, "attempt": 2, "job_key": "stable-gnu-build", "target": "aarch64-unknown-linux-gnu"}
            assert report["inputs"] == {"distribution": "dist",
                "qualification_profile": "stable-gnu-ubuntu22", "release_tools_only": False}
            assert set(report) == {"schema_version", "binding", "inputs", "observed_at_utc", "runner", "snapshot", "scope"}
            assert PRIVATE.encode() not in payload and PRIVATE not in output.getvalue()
            assert re.fullmatch(r"ARM64 runner baseline recorded: bytes=[0-9]+ sha256=[0-9a-f]{64}\n", output.getvalue())
            import hashlib
            digest = hashlib.sha256(payload).hexdigest()
            assert (directory / "baseline.sha256").read_text() == digest + "  baseline.json\n"
            assert digest in output.getvalue()
            assert set(reads) == set(FILES)
            assert len(commands) == 3
            if malformed or unavailable:
                for key in ("os_release", "memory_kib", "packages"):
                    assert report["snapshot"][key]["status"] == "UNKNOWN"
                for value in report["snapshot"]["visible_cgroup"].values():
                    assert value["status"] == "UNKNOWN"
            else:
                assert report["snapshot"]["memory_kib"]["MemTotal"] == 16000
                assert report["snapshot"]["visible_cgroup"]["memory_max"] == "max"
                assert report["snapshot"]["os_release"] == {"ID": "ubuntu", "VERSION_ID": "22.04"}
        finally:
            os.umask(saved_mask)

def reader(raw, timeout=False, status=0, missing=False):
    code = namespace()
    class Pipe:
        closed = False
        def fileno(self):
            return 91
        def close(self):
            self.closed = True
    class Child:
        stdout = Pipe()
        finished = False
        killed = False
        def poll(self):
            return status if self.finished else None
        def kill(self):
            self.killed, self.finished = True, True
        def wait(self, timeout):
            assert 0 <= timeout <= 4
            self.finished = True
            return status
    child = Child()
    remaining = bytearray(raw)
    def popen(args, **kwargs):
        assert args == ["controlled-metadata"]
        assert kwargs == {"stdin": subprocess.DEVNULL, "stdout": subprocess.PIPE, "stderr": subprocess.DEVNULL}
        if missing:
            raise FileNotFoundError("controlled missing command")
        return child
    def select(readers, writers, errors, duration):
        assert readers == [child.stdout] and not writers and not errors
        assert 0 < duration <= 4, "subprocess capture timeout widened"
        return ([], [], []) if timeout else (readers, [], [])
    def read(fd, size):
        assert fd == 91 and 0 < size <= 1024
        value = bytes(remaining[:size])
        del remaining[:size]
        return value
    with mock.patch.object(subprocess, "Popen", side_effect=popen), \
         mock.patch.object(code["select"], "select", side_effect=select), \
         mock.patch.object(os, "read", side_effect=read), \
         mock.patch.object(code["time"], "monotonic", return_value=0):
        value = code["metadata_command"](["controlled-metadata"])
    assert value == "ok\n" if raw == b"ok\n" and not timeout and not status and not missing else value["status"] == "UNKNOWN"
    if not missing:
        assert child.stdout.closed
        assert child.killed if timeout or len(raw) >= 4096 else not child.killed

try:
    exercise()
    exercise(malformed=True)
    exercise(unavailable=True)
    exercise(tiny=True, refuse=True)
    exercise({"GITHUB_SHA": "c" * 40, "BASELINE_SOURCE": "c" * 40}, refuse=True)
    exercise(bad_tree=True, refuse=True)
    for key, value in (
        ("BASELINE_SOURCE", "c" * 40), ("GITHUB_SHA", "c" * 40),
        ("BASELINE_RUN_ID", "18"), ("BASELINE_RUN_ATTEMPT", "1"),
        ("GITHUB_RUN_ATTEMPT", "0"), ("BASELINE_TARGET", "x86_64-unknown-linux-gnu"),
        ("BASELINE_DISTRIBUTION", "dist-broad"), ("BASELINE_PROFILE", "normal"),
        ("BASELINE_RELEASE_TOOLS_ONLY", "true"), ("GITHUB_JOB", "other-job"),
        ("GITHUB_ACTIONS", "false"), ("RUNNER_OS", "Windows"), ("RUNNER_ARCH", "ARM64"),
    ):
        exercise({key: value}, refuse=True)
    reader(b"ok\n")
    reader(b"x" * 8192)
    reader(b"ok\n", timeout=True)
    reader(b"ok\n", status=7)
    reader(b"ok\n", missing=True)
except AssertionError:
    raise SystemExit(91) from None
"###;
    run_release_recipe(
        &format!("python3 - <<'PY'\nSOURCE = {source}\n{fixture}\nPY"),
        RELEASE_RECIPE_CONTEXTS[0],
        "",
        "false",
        "",
    )
}

#[cfg(unix)]
#[test]
fn gnu_arm64_runner_baseline_fixture_preserves_bindings_privacy_and_unknowns() {
    let source = arm64_baseline_python(&workflow("cross-platform-build-manual.yml"));
    let observed = observe_arm64_baseline_fixture(&source);
    assert_eq!(observed.status, Some(0), "{}", observed.diagnostics);
}

#[cfg(unix)]
#[test]
fn gnu_arm64_runner_baseline_fixture_rejects_identity_bounds_and_privacy_mutants() {
    let source = arm64_baseline_python(&workflow("cross-platform-build-manual.yml"));
    for (before, after) in [
        ("source != os.environ.get(\"BASELINE_SOURCE\")", "False"),
        ("run != os.environ.get(\"BASELINE_RUN_ID\")", "False"),
        (
            "attempt != os.environ.get(\"BASELINE_RUN_ATTEMPT\")",
            "False",
        ),
        (
            "if any(os.environ.get(key) != value for key, value in expected.items()):",
            "if False:",
        ),
        ("checkout.strip() != source", "False"),
        ("not re.fullmatch(r\"[0-9a-f]{40}\\n?\", tree)", "False"),
        ("if len(data) > JSON_LIMIT:", "if False:"),
        ("handle.read(READ_LIMIT)", "handle.read()"),
        (
            "open(path, \"rb\", buffering=0)",
            "open(path, \"rb\", buffering=8192)",
        ),
        ("if len(data) >= READ_LIMIT:", "if False:"),
        (
            "deadline = time.monotonic() + 4",
            "deadline = time.monotonic() + 40",
        ),
        ("stderr=subprocess.DEVNULL", "stderr=subprocess.PIPE"),
        (
            "    data = (json.dumps(report,",
            "    report[\"environment\"] = dict(os.environ)\n    data = (json.dumps(report,",
        ),
        (
            "    print(\"ARM64 runner baseline recorded:",
            "    print(report)\n    print(\"ARM64 runner baseline recorded:",
        ),
    ] {
        assert!(
            source.contains(before),
            "mutation must reach actual body: {before}"
        );
        let observed = observe_arm64_baseline_fixture(&source.replacen(before, after, 1));
        assert_eq!(
            observed.status,
            Some(91),
            "{before}: {}",
            observed.diagnostics
        );
    }
}

#[test]
fn gnu_arm64_runner_baseline_cli_propagates_refusal_without_traceback() {
    let manual = workflow("cross-platform-build-manual.yml");
    let body = explicit_bash_body(
        yaml_block(
            yaml_block(&manual, "  stable-gnu-build:\n"),
            "      - name: Capture GNU ARM64 runner baseline\n",
        ),
        "          ",
    );
    // Empty controlled environment refuses before any metadata or resource access.
    let observed = run_release_recipe(&body, RELEASE_RECIPE_CONTEXTS[0], "", "false", "");
    assert_eq!(observed.status, Some(1), "{}", observed.diagnostics);
    assert!(
        observed
            .diagnostics
            .contains("ARM64 runner baseline capture refused")
    );
    assert!(!observed.diagnostics.contains("Traceback"));
    let before = "raise SystemExit(\"ARM64 runner baseline capture refused\") from None";
    assert!(body.contains(before));
    let mutant = body.replacen(before, "pass", 1);
    let exposed = run_release_recipe(&mutant, RELEASE_RECIPE_CONTEXTS[0], "", "false", "");
    assert_eq!(exposed.status, Some(0), "{}", exposed.diagnostics);
}

#[test]
fn macos_desktop_release_notarizes_published_dmg() {
    let workflow_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release-stable-manual.yml");
    let workflow = fs::read_to_string(&workflow_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", workflow_path.display()));
    let macos_job = workflow
        .split_once("  build-desktop:\n")
        .and_then(|(_, remainder)| remainder.split_once("  # New desktop platforms."))
        .map(|(job, _)| job)
        .expect("release workflow must contain the macOS desktop build job");

    assert_eq!(
        macos_job.matches("MACOS_DMG_PATH:").count(),
        1,
        "the published DMG path must have exactly one source of truth"
    );
    for required in [
        "MACOS_DMG_PATH: desktop-assets/ZeroClaw.dmg",
        "dmg_dir=\"target/universal-apple-darwin/release/bundle/dmg\"",
        "dmg_candidates=(\"$dmg_dir\"/*.dmg)",
        "\"${#dmg_candidates[@]}\" -ne 1",
        "mv \"${dmg_candidates[0]}\" \"$MACOS_DMG_PATH\"",
        "notarytool submit \"$MACOS_DMG_PATH\"",
        "stapler staple \"$MACOS_DMG_PATH\"",
        "stapler validate \"$MACOS_DMG_PATH\"",
        "${{ env.MACOS_DMG_PATH }}",
    ] {
        assert!(
            macos_job.contains(required),
            "macOS desktop job is missing release invariant: {required}"
        );
    }

    assert!(
        !macos_job.contains("find target -name '*.dmg'"),
        "the macOS desktop job must not rediscover DMGs from the whole target tree"
    );

    let positions = [
        "mv \"${dmg_candidates[0]}\" \"$MACOS_DMG_PATH\"",
        "notarytool submit \"$MACOS_DMG_PATH\"",
        "stapler staple \"$MACOS_DMG_PATH\"",
        "stapler validate \"$MACOS_DMG_PATH\"",
        "uses: actions/upload-artifact@",
    ]
    .map(|needle| {
        macos_job
            .find(needle)
            .unwrap_or_else(|| panic!("macOS desktop job is missing ordered step: {needle}"))
    });
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "the final DMG must be prepared, notarized, stapled, validated, then uploaded"
    );
}

#[test]
fn package_publishers_use_canonical_sources_and_scoped_credentials() {
    let release = workflow("release-stable-manual.yml");
    assert!(
        !release.contains("pub-homebrew-core.yml"),
        "Homebrew Core is updated by its official autobump service, not a duplicate publisher"
    );
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".github/workflows/pub-homebrew-core.yml")
            .exists(),
        "the redundant project-owned Homebrew publisher must stay retired"
    );

    let scoop = workflow("pub-scoop.yml");
    for required in [
        "SCOOP_BUCKET_TOKEN",
        "dist/scoop/zeroclaw.json",
        "push --dry-run origin HEAD",
        "Contents: Read and write",
        ".architecture[\"64bit\"].url = $url",
        ".architecture[\"64bit\"].hash = $hash",
    ] {
        assert!(
            scoop.contains(required),
            "Scoop publisher is missing packaging invariant: {required}"
        );
    }
    for forbidden in [
        "gh api \"repos/${SCOOP_BUCKET_REPO}\" --jq '.permissions.push'",
        "cat > \"$manifest_file\" <<MANIFEST",
    ] {
        assert!(
            !scoop.contains(forbidden),
            "Scoop publisher must not contain duplicate or heuristic path: {forbidden}"
        );
    }

    assert!(
        scoop.contains(
            "bash scripts/release/scoop_metadata.sh dist/scoop/zeroclaw.json \"$version\""
        ),
        "pub-scoop.yml must materialize publisher metadata from the canonical manifest"
    );
    assert!(
        !scoop.contains("https://github.com/${GITHUB_REPOSITORY}/releases/download/"),
        "pub-scoop.yml must not rebuild a release URL independently of the canonical manifest"
    );

    let aur = workflow("pub-aur.yml");
    let publish_job = aur
        .split_once("  publish-aur:\n")
        .map(|(_, job)| job)
        .expect("AUR publisher must define the publish-aur job");
    assert!(
        publish_job
            .lines()
            .take(5)
            .any(|line| line == "    timeout-minutes: 20"),
        "the publish-aur job must have a bounded job timeout"
    );
    assert!(
        !aur.contains("ssh -T -o"),
        "AUR clone/push is the authoritative authentication check"
    );
    for required in [
        "group: aur-publish-${{ github.repository }}-${{ inputs.dry_run }}",
        "ref: refs/tags/${{ inputs.release_tag }}\n          path: release-source",
        "release-source/dist/aur/PKGBUILD",
        "release-source/dist/aur/.SRCINFO",
        "if: inputs.dry_run == false\n        timeout-minutes: 12",
        "\"${guard_command[@]}\" || return $?",
        "case \"$attempt_status\" in",
        "unexpected status ${attempt_status}",
        "Generated PKGBUILD is not valid Bash syntax",
        "tarball_url=\"https://github.com/zeroclaw-labs/zeroclaw/archive/refs/tags/${RELEASE_TAG}.tar.gz\"",
        "require_exact_line \"$PKGBUILD_FILE\" \"$expected_pkgbuild_source\"",
        "require_exact_line \"$SRCINFO_FILE\" \"$expected_srcinfo_source\"",
        "package metadata is missing, malformed, or inconsistent",
        "stopped to prevent a downgrade",
        "package files changed without a version-tuple change",
        "Release metadata is pinned to the immutable tag",
        "corrected source change must ship under a new release tag",
    ] {
        assert!(
            aur.contains(required),
            "AUR publisher is missing release-safety invariant: {required}"
        );
    }

    let workflow_call_inputs = aur
        .split_once("  workflow_call:\n")
        .and_then(|(_, remainder)| remainder.split_once("  workflow_dispatch:\n"))
        .map(|(block, _)| block)
        .expect("AUR publisher must define workflow_call before workflow_dispatch");
    assert!(
        !workflow_call_inputs.contains("allow_downgrade"),
        "automated reusable callers must not be able to authorize an AUR downgrade"
    );
    let manual_inputs = aur
        .split_once("  workflow_dispatch:\n")
        .and_then(|(_, remainder)| remainder.split_once("\nconcurrency:\n"))
        .map(|(block, _)| block)
        .expect("AUR publisher must define manual dispatch inputs");
    assert!(
        manual_inputs.contains("allow_downgrade:"),
        "manual recovery must expose an explicit downgrade override"
    );
    let downgrade_input = manual_inputs
        .split_once("allow_downgrade:")
        .map(|(_, block)| block)
        .expect("manual dispatch must expose allow_downgrade");
    assert!(
        downgrade_input.contains("default: false"),
        "manual downgrade authorization must default to false"
    );

    let guard_call = "scripts/release/aur_version_guard.sh";
    assert_eq!(
        aur.matches("scripts/release/aur_version_guard.sh").count(),
        2,
        "the AUR guard must validate generated metadata and each fresh clone"
    );

    let clone_position = aur
        .find("git clone --quiet ssh://aur@aur.archlinux.org/zeroclawlabs.git")
        .expect("AUR publisher must clone the authoritative package state");
    let guard_position = aur
        .rfind(guard_call)
        .expect("AUR publisher must enforce monotonic versions");
    let overwrite_position = aur
        .find("cp \"$PKGBUILD_FILE\" \"$work_dir/PKGBUILD\"")
        .expect("AUR publisher must update PKGBUILD");
    assert!(
        clone_position < guard_position && guard_position < overwrite_position,
        "the AUR monotonic guard must inspect each fresh clone before package metadata is overwritten"
    );

    let input_validation_position = aur
        .find("      - name: Validate release tag input\n")
        .expect("release tag input must be validated");
    let release_checkout_position = aur
        .find("      - name: Check out release package metadata\n")
        .expect("release package metadata must use an isolated checkout");
    assert!(
        input_validation_position < release_checkout_position,
        "release_tag must be validated before it is used as a checkout ref"
    );

    let generated_validation = aur
        .split_once("      - name: Validate generated AUR metadata\n")
        .and_then(|(_, remainder)| remainder.split_once("      - name: Push to AUR\n"))
        .map(|(step, _)| step)
        .expect("generated AUR metadata must be validated before the push step");
    assert!(
        generated_validation.contains(guard_call)
            && generated_validation.contains("\"$SRCINFO_FILE\" \"$SRCINFO_FILE\"")
            && generated_validation.contains("\"$PKGBUILD_FILE\" \"$PKGBUILD_FILE\""),
        "dry-run must exercise target-side metadata validation without an AUR clone"
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_srcinfo = root.join("dist/aur/.SRCINFO");
    let source_pkgbuild = root.join("dist/aur/PKGBUILD");
    let source_guard = bash_command()
        .arg(bash_path(
            &root.join("scripts/release/aur_version_guard.sh"),
        ))
        .arg(bash_path(&source_srcinfo))
        .arg(bash_path(&source_srcinfo))
        .arg(bash_path(&source_pkgbuild))
        .arg(bash_path(&source_pkgbuild))
        .output()
        .expect("validate checked-in AUR package metadata");
    assert!(
        source_guard.status.success(),
        "checked-in PKGBUILD and .SRCINFO version tuples must agree: {}",
        command_diagnostics(&source_guard)
    );

    let freshness = workflow("aur-freshness-check.yml");
    assert!(
        freshness.contains(
            "aur_epoch_pkgver=\"${aur_full%%-*}\"\n          aur_version=\"${aur_epoch_pkgver#*:}\""
        ),
        "AUR freshness must remove pkgrel and epoch before comparing pkgver to the release"
    );
    assert!(
        freshness.contains("sort -V | tail -n 1")
            && freshness.contains("AUR is newer than the release")
            && freshness.contains("source_epoch=\"$(git show")
            && freshness.contains("git show \"${tag}:dist/aur/.SRCINFO\"")
            && freshness.contains("\"$aur_epoch\" != \"$source_epoch\"")
            && freshness.contains("Do not use allow_downgrade across epochs")
            && freshness.contains("cut a new release tag")
            && freshness.contains("this check remains red until that tag is published"),
        "freshness must compare the published epoch and scope downgrade recovery advice"
    );
}

/// The `target:` entries of one job's matrix, in order, whether an entry
/// starts with the target or lists it after another field.
fn matrix_targets(job: &str) -> Vec<String> {
    job.lines()
        .map(|line| line.trim())
        .map(|line| line.strip_prefix("- ").unwrap_or(line))
        .filter_map(|line| line.strip_prefix("target: "))
        .map(|target| target.trim().to_owned())
        .collect()
}

/// The release proves plugin support from the archives it publishes: one
/// Linux job builds the smoke packages, every archive that carries the plugin
/// host is executed on a runner of its own platform, and neither the GitHub
/// Release nor the container images wait on anything less. The leg set itself
/// is pinned to the registry's per-target policy by an xtask test; this gate
/// pins the wiring around it.
#[test]
fn plugin_execution_smoke_gates_the_release() {
    let release = workflow("release-stable-manual.yml");

    // Neither job publishes anything, so each holds exactly a read-only token
    // and its checkout leaves none behind in the tree.
    let packages = yaml_block(&release, "  plugin-smoke-packages:\n");
    let smoke = yaml_block(&release, "  plugin-smoke:\n");
    for job in [packages, smoke] {
        assert_eq!(
            yaml_block(job, "    permissions:\n").trim_end(),
            "      contents: read",
            "a plugin smoke job must hold exactly a read-only contents token"
        );
        assert!(
            job.contains("persist-credentials: false"),
            "a plugin smoke job must not persist the checkout token"
        );
    }
    for required in [
        "needs: [validate]\n",
        "targets: wasm32-wasip2",
        "run: bash scripts/ci/build_plugin_smoke_packages.sh \"$RUNNER_TEMP/plugin-smoke-packages\"",
        "name: plugin-smoke-packages",
        "retention-days: 14",
    ] {
        assert!(
            packages.contains(required),
            "the smoke package build is missing invariant: {required}"
        );
    }

    // The whole invocation is pinned so nothing can be appended to it that
    // would swallow its exit status.
    let release_invocation = "bash scripts/ci/plugin_artifact_smoke.sh \\\n            \
--binary \"$binary\" \\\n            \
--expect host \\\n            \
--packages plugin-smoke-packages \\\n            \
--label \"${{ matrix.target }}\" \\\n            \
--archive \"$archive\" \\\n            \
--evidence \"plugin-smoke-${{ matrix.target }}.md\"\n";
    for required in [
        "needs: [build, plugin-smoke-packages]",
        "fail-fast: false",
        "name: zeroclaw-${{ matrix.target }}",
        "name: plugin-smoke-packages",
        release_invocation,
    ] {
        assert!(
            smoke.contains(required),
            "the plugin smoke job is missing invariant: {required}"
        );
    }
    assert!(
        !smoke.contains("continue-on-error"),
        "every plugin smoke leg must block the release"
    );
    assert!(
        !smoke.contains("rust-toolchain"),
        "the smoke legs run the packaged binary and need no toolchain"
    );
    // The smoke step itself carries no condition. The only step-level `if` in
    // either smoke job is the evidence upload's, which runs after a failed
    // check too. A step key can be written indented under the step or as the
    // step's first key behind `- `, so both forms count.
    let step_conditions = |job: &str| -> Vec<String> {
        job.lines()
            .filter_map(|line| {
                let indent = line.len() - line.trim_start().len();
                let trimmed = line.trim_start();
                let key = match (indent, trimmed.strip_prefix("- ")) {
                    (6, Some(first_key)) => first_key,
                    (8, None) => trimmed,
                    _ => return None,
                };
                key.starts_with("if:").then(|| key.to_owned())
            })
            .collect()
    };
    assert_eq!(
        step_conditions(smoke),
        ["if: ${{ !cancelled() }}"],
        "the release smoke step must run unconditionally"
    );

    // Each leg executes an archive the build matrix produces, and the manual
    // cross-platform workflow rehearses exactly the same legs: standard dist
    // consumes its product archive, while dist-broad retains the raw route.
    let build = yaml_block(&release, "  build:\n");
    let smoke_targets = matrix_targets(smoke);
    assert!(!smoke_targets.is_empty(), "the plugin smoke has no legs");
    for target in &smoke_targets {
        assert!(
            build.contains(&format!("target: {target}\n")),
            "plugin smoke leg {target} has no build leg"
        );
    }
    let manual = workflow("cross-platform-build-manual.yml");
    let rehearsal = yaml_block(&manual, "  plugin-smoke:\n");
    assert_eq!(
        matrix_targets(rehearsal),
        smoke_targets,
        "the manual workflow must rehearse exactly the release smoke legs"
    );
    // Build legs without a smoke leg (ARM32, Android) must not take the
    // rehearsal down with them: it requires the packages to succeed, not every
    // build leg, and skips only for a cancelled run, a release-tools-only run,
    // or failed packages. The invocation is pinned whole, as in the release.
    let rehearsal_invocation = "bash scripts/ci/plugin_artifact_smoke.sh \\\n            \
--binary \"$binary\" \\\n            \
--expect host \\\n            \
--packages plugin-smoke-packages \\\n            \
--label \"$SMOKE_LABEL\" \\\n            \
${archive_args[@]+\"${archive_args[@]}\"} \\\n            \
--evidence \"$SMOKE_EVIDENCE\"\n";
    for required in [
        "needs: [build, plugin-smoke-packages]",
        "if: ${{ !cancelled() && inputs.qualification_profile == 'normal' && !inputs.release_tools_only && needs.plugin-smoke-packages.result == 'success' }}",
        "name: zeroclaw-manual-${{ inputs.distribution }}-${{ matrix.target }}",
        rehearsal_invocation,
    ] {
        assert!(
            rehearsal.contains(required),
            "the manual plugin smoke is missing invariant: {required}"
        );
    }
    assert_eq!(
        step_conditions(rehearsal),
        [
            "if: inputs.distribution != 'dist'",
            "if: inputs.distribution == 'dist'",
            "if: inputs.distribution == 'dist'",
            "if: ${{ !cancelled() }}",
        ],
        "raw download, archive download, extraction and evidence conditions must retain their order"
    );
    for binding in [
        "if: inputs.distribution != 'dist'\n        with:\n          name: zeroclaw-manual-${{ inputs.distribution }}-${{ matrix.target }}\n          path: binary",
        "if: inputs.distribution == 'dist'\n        with:\n          name: zeroclaw-manual-archive-dist-${{ matrix.target }}\n          path: archive",
    ] {
        assert!(
            rehearsal.contains(binding),
            "each distribution selector must bind its corresponding artifact: {binding}"
        );
    }
    let archive_extraction = yaml_block(
        rehearsal,
        "      - name: Verify and extract standard release archive\n",
    );
    assert!(archive_extraction.starts_with("        if: inputs.distribution == 'dist'\n"));
    assert!(archive_extraction.contains("id: archive"));
    assert!(archive_extraction.contains("--archive \"archive/zeroclaw-$target.$ext\""));
    assert!(archive_extraction.contains("--manifest archive/archive-evidence/input-manifest.json"));
    // Download/extraction conditions must not make the actual smoke optional
    // or let it ignore a failed control. Scope these checks to that step.
    let rehearsal_smoke = yaml_block(
        rehearsal,
        "      - name: Run the plugin smoke against the built binary\n",
    );
    assert!(
        step_conditions(rehearsal_smoke).is_empty(),
        "the rehearsal smoke step must run unconditionally"
    );
    assert!(
        !rehearsal_smoke.contains("continue-on-error"),
        "the actual rehearsal smoke step must propagate every failed control"
    );
    for binding in [
        "SMOKE_DISTRIBUTION: ${{ inputs.distribution }}",
        "SMOKE_BINARY: ${{ steps.archive.outputs.binary }}",
        "SMOKE_ARCHIVE: ${{ steps.archive.outputs.archive }}",
        "SMOKE_BINARY_NAME: ${{ runner.os == 'Windows' && 'zeroclaw.exe' || 'zeroclaw' }}",
        "SMOKE_LABEL: ${{ inputs.distribution }} / ${{ matrix.target }}",
        "SMOKE_EVIDENCE: plugin-smoke-${{ matrix.target }}.md",
    ] {
        assert!(rehearsal_smoke.contains(binding));
    }
    assert!(!explicit_bash_body(rehearsal_smoke, "          ").contains("${{"));
    assert!(rehearsal_smoke.contains(rehearsal_invocation));
    assert!(rehearsal_smoke.contains(
        "if [ \"$SMOKE_DISTRIBUTION\" = dist ]; then\n            binary=\"$SMOKE_BINARY\"\n          else\n            binary=\"binary/$SMOKE_BINARY_NAME\"\n          fi"
    ));
    assert!(rehearsal_smoke.contains(
        "archive_args=()\n          if [ \"$SMOKE_DISTRIBUTION\" = dist ]; then\n            archive_args=(--archive \"$SMOKE_ARCHIVE\")\n          fi"
    ));
    assert!(
        yaml_block(&manual, "  plugin-smoke-packages:\n")
            .contains("run: bash scripts/ci/build_plugin_smoke_packages.sh"),
        "the manual workflow must build the smoke packages the same way"
    );

    let github_release = yaml_block(&release, "  publish:\n");
    let needs = github_release
        .lines()
        .find(|line| line.trim_start().starts_with("needs: ["))
        .expect("the GitHub Release must declare needs");
    assert!(
        needs.contains(" plugin-smoke,") || needs.contains(" plugin-smoke]"),
        "the GitHub Release must wait for the plugin smoke"
    );
    let docker = yaml_block(&release, "  docker:\n");
    assert!(
        docker.contains("needs: [validate, build, plugin-smoke]"),
        "the container images repackage smoke-gated archives and must wait for the smoke"
    );
    assert!(
        !docker.lines().any(|line| line.starts_with("    if:")),
        "the container images must keep the implicit success() gate over the smoke"
    );
}

/// GitHub retires the macOS 14 runner image on 2026-11-02 and fails jobs on it
/// during scheduled brownouts until then. The plugin smoke runs a macOS leg and
/// blocks both the GitHub Release and the container images, so one leg on that
/// image stops a release. No workflow may name it.
#[test]
fn workflows_never_name_the_retired_macos_14_runner() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut scanned = 0;
    let mut offenders = Vec::new();
    for entry in fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("failed to list {}: {error}", directory.display()))
    {
        let entry = entry.expect("readable workflow directory entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let text = fs::read_to_string(entry.path())
            .unwrap_or_else(|error| panic!("failed to read {name}: {error}"));
        scanned += 1;
        offenders.extend(
            text.lines()
                .enumerate()
                .filter(|(_, line)| line.contains("macos-14"))
                .map(|(index, _)| format!("{name}:{}", index + 1)),
        );
    }
    assert!(scanned > 0, "no workflow files in {}", directory.display());
    assert!(
        offenders.is_empty(),
        "macos-14 is a retired runner image; use macos-15: {offenders:?}"
    );
}

#[test]
fn crates_io_publisher_is_preflighted_gated_and_resumable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let release = workflow("release-stable-manual.yml");
    let publisher = workflow("pub-crates.yml");
    let script_path = root.join("scripts/release/publish-crates.sh");
    let script = fs::read_to_string(&script_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", script_path.display()));

    for required in [
        "uses: ./.github/workflows/pub-crates.yml",
        "release_tag: ${{ needs.validate.outputs.tag }}",
        "release_sha: ${{ github.sha }}",
    ] {
        assert!(
            release.contains(required),
            "stable release is missing crates.io wiring: {required}"
        );
    }
    let crates_call = yaml_block(&release, "  crates:\n");
    assert!(
        crates_call
            .contains("secrets:\n      CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}"),
        "the reusable crates.io workflow must receive the existing repository secret explicitly"
    );
    assert!(
        !crates_call.contains("secrets: inherit"),
        "the crates.io caller must pass only the named registry secret"
    );

    // The crates.io preflight runs beside the binary builds and gates the
    // GitHub Release, so a release whose crates cannot publish never becomes
    // public. It gets no token; the late `crates` call uploads what it verified.
    let early = yaml_block(&release, "  crates-preflight:\n");
    for required in [
        "needs: [validate]\n",
        "uses: ./.github/workflows/pub-crates.yml",
        "release_tag: ${{ needs.validate.outputs.tag }}",
        "release_sha: ${{ github.sha }}",
        "stage: preflight",
        "dry_run: true",
    ] {
        assert!(
            early.contains(required),
            "early crates.io preflight is missing invariant: {required}"
        );
    }
    assert!(
        !early.contains("secrets"),
        "the early crates.io preflight cannot upload and must not receive any secret"
    );
    let github_release = yaml_block(&release, "  publish:\n");
    assert!(
        github_release.contains(
            "needs: [validate, release-notes, build, build-desktop, build-desktop-linux, build-desktop-windows, sbom, crates-preflight, plugin-smoke]"
        ),
        "the GitHub Release must wait for the crates.io preflight and the plugin smoke"
    );
    assert!(
        !github_release
            .lines()
            .any(|line| line.starts_with("    if:")),
        "the GitHub Release must keep the implicit success() gate over crates-preflight"
    );
    for required in [
        "needs: [validate, publish, crates-preflight]",
        "needs.crates-preflight.result == 'success'",
        "stage: publish",
        "verified_web_dist_digest: ${{ needs.crates-preflight.outputs.web_dist_digest }}",
        "dry_run: false",
    ] {
        assert!(
            crates_call.contains(required),
            "the late crates.io call is missing invariant: {required}"
        );
    }

    assert!(
        publisher.contains(
            "  group: ${{ inputs.stage == 'preflight' && format('crates-io-preflight-{0}', github.run_id) || 'crates-io-publish' }}\n  cancel-in-progress: false"
        ),
        "only calls that can upload may share the serialised publish group"
    );

    // A version bump cannot merge unless the crates it announces package and
    // compile. The Quality Gate runs the same tokenless preflight stage on
    // pull requests and merge-queue entries that change the workspace version.
    let quality_gate = workflow("ci.yml");
    let bump_detector = yaml_block(&quality_gate, "  crates-preflight-changes:\n");
    assert!(
        bump_detector.contains(
            "BASE_SHA: ${{ github.event.pull_request.base.sha || github.event.merge_group.base_sha }}"
        ) && bump_detector
            .contains("bash scripts/ci/crates_preflight_trigger.sh \"$EVENT_NAME\" \"$BASE_SHA\""),
        "the version-bump detector must compare against the PR or merge-queue base"
    );
    let bump_preflight = yaml_block(&quality_gate, "  crates-preflight:\n");
    for required in [
        "needs: [crates-preflight-changes]",
        "if: needs.crates-preflight-changes.outputs.run == 'true'",
        "uses: ./.github/workflows/pub-crates.yml",
        "release_tag: ${{ needs.crates-preflight-changes.outputs.tag }}",
        "release_sha: ${{ github.sha }}",
        "stage: preflight",
        "dry_run: true",
    ] {
        assert!(
            bump_preflight.contains(required),
            "the version-bump crates.io preflight is missing invariant: {required}"
        );
    }
    assert!(
        !bump_preflight.contains("secrets"),
        "pull requests, including forks, must never hand the publisher a secret"
    );
    let gate_needs = yaml_block(&quality_gate, "  gate:\n")
        .lines()
        .find(|line| line.starts_with("    needs: ["))
        .expect("the required gate must declare needs");
    for job in ["crates-preflight-changes", "crates-preflight"] {
        assert!(
            gate_needs.contains(&format!(" {job},")) || gate_needs.contains(&format!(" {job}]")),
            "CI Required Gate must block on {job}"
        );
    }
    assert!(
        quality_gate.contains("run: bash scripts/ci/crates_preflight_trigger.test.sh"),
        "the version-bump trigger must keep its process test in CI"
    );
    assert!(
        publisher.contains(
            "save-if: ${{ github.event_name != 'pull_request' && github.event_name != 'merge_group' }}"
        ),
        "pull-request runs of the publisher may read the Rust cache but never write it"
    );

    let workflow_call = yaml_block(&publisher, "  workflow_call:\n");
    for required in [
        "CARGO_REGISTRY_TOKEN:\n        description: \"Repository-scoped crates.io token; referenced only by the protected publish job\"\n        required: false",
        "stage:\n",
        "default: all",
        "verified_web_dist_digest:\n",
        "value: ${{ jobs.preflight.outputs.web_dist_digest }}",
    ] {
        assert!(
            workflow_call.contains(required),
            "the reusable publisher is missing workflow_call invariant: {required}"
        );
    }

    let preflight = yaml_block(&publisher, "  preflight:\n");
    let publish = yaml_block(&publisher, "  publish:\n");
    for required in [
        "ref: ${{ inputs.release_sha || inputs.release_tag }}",
        "fetch-depth: 0",
        "bash .release-tooling/scripts/release/resolve_crates_release.sh",
        "TOOLING_SHA: ${{ github.workflow_sha }}",
        "tooling_sha: ${{ steps.meta.outputs.tooling_sha }}",
        "STAGE: ${{ inputs.stage }}",
        "cargo test --locked --test architecture publish_contract",
        "CARGO_TARGET_DIR: ${{ runner.temp }}/crates-io-package-target",
        "./.release-tooling/scripts/release/publish-crates.sh",
    ] {
        assert!(
            preflight.contains(required),
            "crates.io preflight is missing invariant: {required}"
        );
    }
    assert!(
        !preflight.contains("CARGO_REGISTRY_TOKEN"),
        "the reversible preflight must never reference the registry token"
    );
    // The publish stage reuses the preflight stage's verification, so every
    // verification step skips there, while tag resolution never does.
    let skips = |step: &str| {
        step.lines()
            .any(|line| line == "        if: steps.meta.outputs.stage != 'publish'")
    };
    for step in preflight.split("\n      - ").skip(1) {
        let verifies = [
            "rust-toolchain@",
            "rust-cache@",
            "setup-node@",
            "cargo web build",
            "web_dist_digest.sh",
            "publish_contract",
            "publish-crates.sh",
            "upload-artifact@",
        ]
        .iter()
        .any(|marker| step.contains(marker));
        if verifies {
            assert!(
                skips(step),
                "a preflight verification step must skip in the publish stage:\n{step}"
            );
        } else {
            assert!(
                !skips(step),
                "tag resolution and the summary must run in every stage:\n{step}"
            );
        }
    }
    assert!(
        publish.contains("if: ${{ inputs.dry_run == false && inputs.stage != 'preflight' }}"),
        "the preflight stage must never reach the upload job"
    );
    assert!(
        publish.contains("if [[ -z \"${CARGO_REGISTRY_TOKEN:-}\" ]]; then"),
        "an optional token declaration relies on the publish step failing closed without it"
    );

    // Recovery: release scripts run from a pinned checkout that is separate
    // from the tree being packaged, so a publisher fix merged after the tag
    // reaches that release without moving the tag. Both jobs use the tooling
    // commit the resolver approved and keep it out of the packaged tree.
    for (job, name, reference) in [
        (preflight, "preflight", "${{ github.workflow_sha }}"),
        (
            publish,
            "publish",
            "${{ needs.preflight.outputs.tooling_sha }}",
        ),
    ] {
        for required in [
            format!("          ref: {reference}\n          path: .release-tooling\n"),
            "          sparse-checkout: |\n            /scripts/release/\n          sparse-checkout-cone-mode: false\n          persist-credentials: false\n".to_string(),
            format!("          TOOLING_SHA: {reference}\n"),
            "test \"$(git -C .release-tooling rev-parse HEAD)\" = \"$TOOLING_SHA\"".to_string(),
            "echo '/.release-tooling/' >> .git/info/exclude".to_string(),
            "PUBLISH_SOURCE_ROOT: ${{ github.workspace }}".to_string(),
        ] {
            assert!(
                job.contains(&required),
                "crates.io {name} job is missing release-tooling invariant: {required}"
            );
        }
        assert!(
            !job.contains("./scripts/release/") && !job.contains("bash scripts/release/"),
            "crates.io {name} job must run release scripts from .release-tooling only"
        );
    }

    for required in [
        "environment:\n      name: crates-io",
        "ref: ${{ needs.preflight.outputs.sha }}",
        "CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}",
        "./.release-tooling/scripts/release/publish-crates.sh --execute",
    ] {
        assert!(
            publish.contains(required),
            "crates.io publish job is missing invariant: {required}"
        );
    }

    // The dashboard is built once. Preflight records its digest and hands the
    // verified tree to the publish job, which must not rebuild it and must make
    // the publisher recheck the digest before anything reaches crates.io.
    for required in [
        "cargo web build",
        "web_dist_digest: ${{ steps.web_digest.outputs.digest || inputs.verified_web_dist_digest }}",
        "bash .release-tooling/scripts/release/web_dist_digest.sh web/dist",
        "WEB_DIST_DIGEST: ${{ steps.web_digest.outputs.digest }}",
        "uses: actions/upload-artifact@",
        "name: crates-io-web-dist",
        "include-hidden-files: true",
        "retention-days: 30",
    ] {
        assert!(
            preflight.contains(required),
            "crates.io preflight is missing web bundle invariant: {required}"
        );
    }
    let preflight_run = preflight
        .split_once("      - name: Preflight\n")
        .expect("crates.io preflight must define the Preflight step")
        .1;
    assert!(
        preflight_run.contains("uses: actions/upload-artifact@"),
        "the web bundle artifact must be uploaded only after the verifying dry run"
    );
    for required in [
        "uses: actions/download-artifact@",
        "name: crates-io-web-dist",
        "path: web/dist/",
        "WEB_DIST_DIGEST: ${{ needs.preflight.outputs.web_dist_digest }}",
        "^[0-9a-f]{64}$",
    ] {
        assert!(
            publish.contains(required),
            "crates.io publish job is missing web bundle invariant: {required}"
        );
    }
    for forbidden in ["cargo web build", "actions/setup-node@", "npm "] {
        assert!(
            !publish.contains(forbidden),
            "the crates.io publish job must reuse the verified bundle, not rebuild it: {forbidden}"
        );
    }
    assert!(
        !release.contains("name: crates-io-web-dist"),
        "the crates.io artifact name must stay distinct from the release run's artifacts"
    );

    for required in [
        "EXECUTE=0",
        "--execute) EXECUTE=1",
        "cargo metadata --format-version 1 --no-deps",
        "git diff --quiet",
        "git ls-files --others --exclude-standard",
        "web/dist/index.html",
        "bash \"$SCRIPT_DIR/web_dist_digest.sh\" web/dist",
        "web/dist does not match the bundle preflight verified",
        "cargo publish --dry-run --locked --allow-dirty",
        "--locked --no-verify --allow-dirty",
        "python3 \"$SCRIPT_DIR/publish_order.py\" \"$VERSION\" <<<\"$META\"",
        "wait_for_registry_version",
        "will skip what already landed",
    ] {
        assert!(
            script.contains(required),
            "publish-crates.sh is missing safety contract: {required}"
        );
    }
}

#[test]
fn aur_publisher_rejects_stale_release_downgrades() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let guard_script = root.join("scripts/release/aur_version_guard.sh");
    let temp = tempfile::tempdir().expect("create temporary AUR package directory");
    let target_srcinfo = temp.path().join("target.SRCINFO");
    let current_srcinfo = temp.path().join("current.SRCINFO");
    let target_pkgbuild = temp.path().join("target.PKGBUILD");
    let current_pkgbuild = temp.path().join("current.PKGBUILD");

    let srcinfo = |epoch: Option<u32>, version: &str, release: &str| {
        let epoch = epoch.map_or_else(String::new, |value| format!("epoch = {value}\n"));
        format!(
            "pkgbase = zeroclawlabs\n{epoch}pkgver = {version}\npkgrel = {release}\npkgname = zeroclawlabs\n"
        )
    };
    let pkgbuild = |epoch: Option<u32>, version: &str, release: &str| {
        let epoch = epoch.map_or_else(String::new, |value| format!("epoch={value}\n"));
        format!("pkgname=zeroclawlabs\n{epoch}pkgver={version}\npkgrel={release}\n")
    };

    let run_guard = |target_metadata: &str,
                     current_metadata: &str,
                     target_build: &str,
                     current_build: &str,
                     allow_downgrade: bool| {
        fs::write(&target_srcinfo, target_metadata).expect("write target AUR .SRCINFO");
        fs::write(&current_srcinfo, current_metadata).expect("write current AUR .SRCINFO");
        fs::write(&target_pkgbuild, target_build).expect("write target AUR PKGBUILD");
        fs::write(&current_pkgbuild, current_build).expect("write current AUR PKGBUILD");
        let mut command = bash_command();
        command.arg(bash_path(&guard_script));
        if allow_downgrade {
            command.arg("--allow-downgrade");
        }
        command
            .arg(bash_path(&target_srcinfo))
            .arg(bash_path(&current_srcinfo))
            .arg(bash_path(&target_pkgbuild))
            .arg(bash_path(&current_pkgbuild))
            .output()
            .expect("run AUR monotonic package guard")
    };

    let same_build = pkgbuild(None, "1.2.3", "1");
    let equal = srcinfo(None, "1.2.3", "1");
    let output = run_guard(&equal, &equal, &same_build, &same_build, false);
    assert!(
        output.status.success(),
        "an unchanged package must be idempotent: {}",
        command_diagnostics(&output)
    );

    for (target, current) in [("1.2.4", "1.2.3"), ("1.10.0", "1.9.9")] {
        let output = run_guard(
            &srcinfo(None, target, "1"),
            &srcinfo(None, current, "1"),
            &pkgbuild(None, target, "1"),
            &pkgbuild(None, current, "1"),
            false,
        );
        assert!(
            output.status.success(),
            "target {target} should be allowed over {current}: {}",
            command_diagnostics(&output)
        );
    }

    let older = srcinfo(None, "1.9.9", "1");
    let newer = srcinfo(None, "1.10.0", "1");
    let older_build = pkgbuild(None, "1.9.9", "1");
    let newer_build = pkgbuild(None, "1.10.0", "1");
    let output = run_guard(&older, &newer, &older_build, &newer_build, false);
    assert_eq!(
        output.status.code(),
        Some(3),
        "an older workflow must return the dedicated downgrade status"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Refusing AUR downgrade"),
        "downgrade rejection must explain why publishing stopped"
    );

    let output = run_guard(&older, &newer, &older_build, &newer_build, true);
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("Manual AUR downgrade override"),
        "an explicit manual override must permit a deliberate rollback"
    );

    let output = run_guard(
        &srcinfo(None, "1.2.3", "1"),
        &srcinfo(None, "1.2.3", "2"),
        &pkgbuild(None, "1.2.3", "1"),
        &pkgbuild(None, "1.2.3", "2"),
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "pkgrel must participate in monotonic package ordering"
    );

    let output = run_guard(
        &srcinfo(None, "2.0.0", "1"),
        &srcinfo(Some(1), "1.0.0", "1"),
        &pkgbuild(None, "2.0.0", "1"),
        &pkgbuild(Some(1), "1.0.0", "1"),
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "epoch must take precedence over pkgver"
    );

    let output = run_guard(
        &srcinfo(None, "2.0.0", "1"),
        &srcinfo(Some(1), "1.0.0", "1"),
        &pkgbuild(None, "2.0.0", "1"),
        &pkgbuild(Some(1), "1.0.0", "1"),
        true,
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "manual downgrade authorization must not cross an epoch boundary"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Refusing manual AUR downgrade override across an epoch boundary"),
        "cross-epoch rejection must give actionable recovery guidance"
    );

    let changed_build = format!("{same_build}# changed metadata\n");
    let output = run_guard(&equal, &equal, &changed_build, &same_build, false);
    assert_eq!(
        output.status.code(),
        Some(4),
        "different package files must not reuse an existing version tuple"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "a changed package must ship under a new version tuple from a new release tag"
        ),
        "same-version drift guidance must account for immutable release metadata"
    );

    let output = run_guard(&equal, &equal, &changed_build, &same_build, true);
    assert_eq!(
        output.status.code(),
        Some(4),
        "manual downgrade authorization must not permit same-version rewrites"
    );

    let malformed = srcinfo(None, "not-a-version", "1");
    let output = run_guard(&equal, &malformed, &same_build, &same_build, false);
    assert_command_failure(
        &output,
        2,
        "Current AUR pkgver is not numeric",
        "unparseable current AUR state must return a hard validation failure",
    );
    let output = run_guard(&equal, &malformed, &same_build, &same_build, true);
    assert_command_failure(
        &output,
        2,
        "Current AUR pkgver is not numeric",
        "manual downgrade authorization must not permit malformed AUR state",
    );

    let extra_equals = equal.replace("pkgver = 1.2.3", "pkgver = 1.2.3 = junk");
    let output = run_guard(&equal, &extra_equals, &same_build, &same_build, false);
    assert_command_failure(
        &output,
        2,
        "Current AUR pkgver is not numeric",
        "SRCINFO values with trailing equals data must not be truncated",
    );

    let malformed_build = same_build.replace("pkgver=1.2.3", "pkgver=1.2.3=junk");
    let output = run_guard(&equal, &equal, &same_build, &malformed_build, false);
    assert_command_failure(
        &output,
        2,
        "Current PKGBUILD pkgver is not numeric",
        "PKGBUILD values with trailing equals data must not be truncated",
    );

    let duplicate = format!("{equal}pkgver = 9.9.9\n");
    let output = run_guard(&equal, &duplicate, &same_build, &same_build, false);
    assert_command_failure(
        &output,
        2,
        "Expected exactly one pkgver in Current .SRCINFO; found 2",
        "multiple pkgver fields must fail closed",
    );

    let mismatched_build = pkgbuild(None, "1.2.3", "2");
    let output = run_guard(&equal, &equal, &mismatched_build, &same_build, false);
    assert_command_failure(
        &output,
        2,
        "Generated AUR .SRCINFO and PKGBUILD disagree",
        "generated PKGBUILD and .SRCINFO version tuples must agree",
    );

    fs::write(&target_srcinfo, &equal).expect("restore target AUR .SRCINFO");
    fs::write(&target_pkgbuild, &same_build).expect("restore target AUR PKGBUILD");
    fs::remove_file(&current_srcinfo).expect("remove current AUR .SRCINFO");
    fs::remove_file(&current_pkgbuild).expect("remove current AUR PKGBUILD");
    let output = bash_command()
        .arg(bash_path(&guard_script))
        .arg(bash_path(&target_srcinfo))
        .arg(bash_path(&current_srcinfo))
        .arg(bash_path(&target_pkgbuild))
        .arg(bash_path(&current_pkgbuild))
        .output()
        .expect("run AUR guard with missing current metadata");
    assert_command_failure(
        &output,
        2,
        "cloned AUR repository is unexpectedly empty",
        "an empty clone must not implicitly authorize a first publish",
    );

    fs::write(&current_srcinfo, &equal).expect("restore only current AUR .SRCINFO");
    let output = bash_command()
        .arg(bash_path(&guard_script))
        .arg(bash_path(&target_srcinfo))
        .arg(bash_path(&current_srcinfo))
        .arg(bash_path(&target_pkgbuild))
        .arg(bash_path(&current_pkgbuild))
        .output()
        .expect("run AUR guard with partial current metadata");
    assert_command_failure(
        &output,
        2,
        "cloned AUR repository is partially populated",
        "a partially populated cloned package must fail closed",
    );
}

#[test]
fn scoop_credential_canary_fails_closed_without_weakening_generic_dry_runs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let gate = root.join("scripts/release/scoop_credential_gate.sh");

    let run_gate = |dry_run: &str,
                    credential_canary: &str,
                    bucket_repo: Option<&str>,
                    bucket_token: Option<&str>| {
        let mut command = bash_command();
        command
            .arg(bash_path(&gate))
            .env("DRY_RUN", dry_run)
            .env("CREDENTIAL_CANARY", credential_canary)
            .env_remove("SCOOP_BUCKET_REPO")
            .env_remove("GH_TOKEN");
        if let Some(repo) = bucket_repo {
            command.env("SCOOP_BUCKET_REPO", repo);
        }
        if let Some(token) = bucket_token {
            command.env("GH_TOKEN", token);
        }
        command.output().expect("run Scoop credential gate")
    };

    let generic_dry_run = run_gate("true", "false", None, None);
    assert!(
        generic_dry_run.status.success(),
        "a generic dry run may omit bucket credentials: {}",
        command_diagnostics(&generic_dry_run)
    );
    assert_eq!(generic_dry_run.stdout, b"skip\n");

    for (repo, token, missing) in [
        (None, Some("test-token"), "repository"),
        (Some("example/scoop-bucket"), None, "token"),
    ] {
        let canary = run_gate("true", "true", repo, token);
        assert!(
            !canary.status.success(),
            "credential canary must fail when the {missing} is missing"
        );
    }

    let configured_canary = run_gate(
        "true",
        "true",
        Some("example/scoop-bucket"),
        Some("test-token"),
    );
    assert!(
        configured_canary.status.success(),
        "configured credential canary must reach the authorization probe: {}",
        command_diagnostics(&configured_canary)
    );
    assert_eq!(configured_canary.stdout, b"probe\n");

    for (repo, token, missing) in [
        (None, Some("test-token"), "repository"),
        (Some("example/scoop-bucket"), None, "token"),
    ] {
        let publish = run_gate("false", "false", repo, token);
        assert!(
            !publish.status.success(),
            "real publish must fail when the {missing} is missing"
        );
    }

    for (dry_run, credential_canary, variable) in [
        ("yes", "false", "DRY_RUN"),
        ("", "false", "DRY_RUN"),
        ("true", "yes", "CREDENTIAL_CANARY"),
        ("true", "", "CREDENTIAL_CANARY"),
    ] {
        let invalid = run_gate(
            dry_run,
            credential_canary,
            Some("example/scoop-bucket"),
            Some("test-token"),
        );
        assert!(
            !invalid.status.success(),
            "invalid {variable} value must fail closed"
        );
    }

    let canary_workflow = workflow("scoop-bucket-canary.yml");
    let canary_triggers = yaml_block(&canary_workflow, "on:\n");
    assert!(
        canary_triggers.contains("- cron: \"23 7 * * 1\""),
        "the Scoop canary must keep its weekly schedule trigger"
    );
    assert!(
        canary_triggers.contains("  workflow_dispatch:"),
        "the Scoop canary must stay manually dispatchable for credential rotation proof"
    );
    let canary_resolve_job = yaml_block(&canary_workflow, "  latest-release:\n");
    assert!(
        !canary_resolve_job.lines().any(|line| {
            line.starts_with("    if:") || line.starts_with("    continue-on-error:")
        }),
        "the Scoop canary tag resolver must run and fail closed on every scheduled invocation"
    );
    let canary_job = yaml_block(&canary_workflow, "  rehearse:\n");
    assert!(
        !canary_job.lines().any(|line| {
            line.starts_with("    if:") || line.starts_with("    continue-on-error:")
        }),
        "the Scoop canary rehearsal must run and fail closed on every scheduled invocation"
    );
    for required in [
        "uses: ./.github/workflows/pub-scoop.yml",
        "dry_run: true",
        "credential_canary: true",
        "SCOOP_BUCKET_TOKEN: ${{ secrets.SCOOP_BUCKET_TOKEN }}",
    ] {
        assert!(
            canary_job.contains(required),
            "Scoop canary is missing fail-closed invariant: {required}"
        );
    }
    assert!(
        !canary_job.contains("secrets: inherit"),
        "Scoop canary must receive only the named bucket token"
    );
    let canary_secrets = yaml_block(canary_job, "    secrets:\n");
    let canary_secret_names = canary_secrets
        .lines()
        .filter(|line| line.starts_with("      ") && !line.starts_with("       "))
        .map(str::trim)
        .collect::<Vec<_>>();
    assert_eq!(
        canary_secret_names,
        ["SCOOP_BUCKET_TOKEN: ${{ secrets.SCOOP_BUCKET_TOKEN }}"],
        "Scoop canary must map exactly the one secret its callee declares"
    );

    let release_workflow = workflow("release-stable-manual.yml");
    let release_scoop_job = yaml_block(&release_workflow, "  scoop:\n");
    for required in [
        "uses: ./.github/workflows/pub-scoop.yml",
        "dry_run: false",
        "SCOOP_BUCKET_TOKEN: ${{ secrets.SCOOP_BUCKET_TOKEN }}",
    ] {
        assert!(
            release_scoop_job.contains(required),
            "real Scoop publisher caller is missing invariant: {required}"
        );
    }
    assert!(
        !release_scoop_job.contains("secrets: inherit"),
        "real Scoop publisher must receive only the named bucket token"
    );
    let release_scoop_secrets = yaml_block(release_scoop_job, "    secrets:\n");
    let release_scoop_secret_names = release_scoop_secrets
        .lines()
        .filter(|line| line.starts_with("      ") && !line.starts_with("       "))
        .map(str::trim)
        .collect::<Vec<_>>();
    assert_eq!(
        release_scoop_secret_names,
        ["SCOOP_BUCKET_TOKEN: ${{ secrets.SCOOP_BUCKET_TOKEN }}"],
        "real Scoop caller must map exactly the one secret its callee declares"
    );
    assert!(
        !release_scoop_job
            .lines()
            .any(|line| line.starts_with("    continue-on-error:")),
        "real Scoop publisher failures must stay fatal"
    );
    let release_scoop_conditions = release_scoop_job
        .lines()
        .filter(|line| line.starts_with("    if:"))
        .collect::<Vec<_>>();
    assert_eq!(
        release_scoop_conditions,
        ["    if: ${{ !cancelled() && needs.publish.result == 'success' }}"],
        "real Scoop publisher must stay gated only on a successful publish"
    );

    let publisher_workflow = workflow("pub-scoop.yml");
    let publisher_triggers = yaml_block(&publisher_workflow, "on:\n");
    let publisher_dispatch = yaml_block(publisher_triggers, "  workflow_dispatch:\n");
    assert!(
        yaml_block(publisher_dispatch, "      dry_run:\n").contains("default: true"),
        "manual Scoop publisher dispatch must stay non-destructive by default"
    );
    let workflow_call = yaml_block(&publisher_workflow, "  workflow_call:\n");
    let workflow_call_secrets = yaml_block(workflow_call, "    secrets:\n");
    let scoop_token = yaml_block(workflow_call_secrets, "      SCOOP_BUCKET_TOKEN:\n");
    assert!(
        scoop_token.contains("required: true"),
        "reusable Scoop publisher must require its declared bucket token"
    );
    let declared_secrets = workflow_call_secrets
        .lines()
        .filter(|line| line.starts_with("      ") && !line.starts_with("       "))
        .collect::<Vec<_>>();
    assert_eq!(
        declared_secrets,
        ["      SCOOP_BUCKET_TOKEN:"],
        "reusable Scoop publisher must declare exactly one secret"
    );

    let publisher_job = yaml_block(&publisher_workflow, "  publish-scoop:\n");
    assert!(
        !publisher_job.lines().any(|line| {
            line.starts_with("    if:") || line.starts_with("    continue-on-error:")
        }),
        "the Scoop publisher job must run and fail closed on every invocation, including canary dry runs"
    );
    let push_step = yaml_block(publisher_job, "      - name: Push to Scoop bucket\n");
    assert_eq!(
        push_step
            .lines()
            .filter(|line| line.starts_with("        if:"))
            .collect::<Vec<_>>(),
        ["        if: inputs.dry_run == false"],
        "the bucket write must stay gated on a non-dry-run so canary rehearsals never push"
    );
    assert!(
        !push_step
            .lines()
            .any(|line| line.starts_with("        continue-on-error:")),
        "real bucket write failures must stay fatal"
    );
    let publisher_env = yaml_block(publisher_job, "    env:\n");
    let canary_env = "CREDENTIAL_CANARY: ${{ inputs.credential_canary }}";
    assert_eq!(
        publisher_env.matches(canary_env).count(),
        1,
        "publisher job-level env must map credential_canary into the tested gate exactly once"
    );
    assert_eq!(
        publisher_workflow.matches("CREDENTIAL_CANARY").count(),
        1,
        "credential_canary must have exactly one uppercase env binding, at publisher job scope"
    );
    let dry_run_env = "DRY_RUN: ${{ inputs.dry_run }}";
    assert_eq!(
        publisher_env.matches(dry_run_env).count(),
        1,
        "publisher job-level env must map dry_run into the tested gate exactly once"
    );
    assert_eq!(
        publisher_workflow.matches("DRY_RUN").count(),
        1,
        "dry_run must have exactly one uppercase env binding, at publisher job scope"
    );

    let validate_step = yaml_block(
        publisher_job,
        "      - name: Validate Scoop publish configuration\n",
    );
    assert!(
        !validate_step.lines().any(|line| {
            line.starts_with("        if:") || line.starts_with("        continue-on-error:")
        }),
        "the Scoop credential gate step must run and fail closed on every invocation, including canary dry runs"
    );
    assert!(
        validate_step.contains("gate_result=\"$(bash scripts/release/scoop_credential_gate.sh)\""),
        "Scoop publisher must enforce the tested credential gate"
    );
    assert!(
        validate_step.contains("push --dry-run origin HEAD"),
        "the authorization probe must live in the unconditional credential gate step"
    );
    assert_eq!(
        publisher_workflow
            .matches("push --dry-run origin HEAD")
            .count(),
        1,
        "Scoop publisher must keep exactly one authoritative authorization probe"
    );
}

#[test]
fn scoop_metadata_template_is_not_evaluated() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let metadata_script = root.join("scripts/release/scoop_metadata.sh");
    let script = fs::read_to_string(&metadata_script)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", metadata_script.display()));
    assert!(
        !script.contains("eval "),
        "canonical Scoop URL templates must never be evaluated as shell code"
    );
}

#[test]
#[cfg(unix)]
fn scoop_publisher_metadata_follows_canonical_url_template() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let metadata_script = root.join("scripts/release/scoop_metadata.sh");
    let temp = tempfile::tempdir().expect("create temporary Scoop manifest directory");
    let manifest_path = temp.path().join("zeroclaw.json");
    fs::write(
        &manifest_path,
        r#"{
  "autoupdate": {
    "architecture": {
      "64bit": {
        "url": "https://downloads.example.test/renamed/repository/releases/v$version/zeroclaw-renamed.zip"
      }
    }
  }
}"#,
    )
    .expect("write temporary Scoop manifest");

    let output = bash_command()
        .arg(bash_path(&metadata_script))
        .arg(bash_path(&manifest_path))
        .arg("1.2.3")
        .output()
        .expect("run Scoop metadata materializer");
    assert!(
        output.status.success(),
        "Scoop metadata materializer failed: {}",
        command_diagnostics(&output)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse Scoop publisher metadata");

    assert_eq!(
        metadata["zip_url"],
        "https://downloads.example.test/renamed/repository/releases/v1.2.3/zeroclaw-renamed.zip"
    );
    assert_eq!(metadata["asset_name"], "zeroclaw-renamed.zip");
    assert_eq!(
        metadata["sums_url"],
        "https://downloads.example.test/renamed/repository/releases/v1.2.3/SHA256SUMS"
    );

    let output = bash_command()
        .arg(bash_path(&metadata_script))
        .arg(bash_path(&manifest_path))
        .arg("v1.2.3")
        .output()
        .expect("run Scoop version validation");
    assert!(
        !output.status.success(),
        "metadata materializer must independently validate the release version"
    );

    for invalid_template in [
        "",
        "https://downloads.example.test/releases/v$version/\nzeroclaw.zip",
        "https://downloads.example.test/releases/latest/zeroclaw.zip",
    ] {
        let invalid_manifest = serde_json::json!({
            "autoupdate": {
                "architecture": {
                    "64bit": {"url": invalid_template}
                }
            }
        });
        fs::write(
            &manifest_path,
            serde_json::to_vec(&invalid_manifest).expect("serialize invalid Scoop manifest"),
        )
        .expect("write invalid Scoop manifest");
        let output = bash_command()
            .arg(bash_path(&metadata_script))
            .arg(bash_path(&manifest_path))
            .arg("1.2.3")
            .output()
            .expect("run Scoop metadata validation");
        assert!(
            !output.status.success(),
            "invalid canonical template must fail closed: {invalid_template:?}"
        );
    }
}
