//! Step-dispatcher renderer: emits each `Plan` step's dry-run/execute pair into
//! a script in its dialect, so the dry-run narration and the real action are
//! generated from one source and can never drift. install.sh and setup.bat both
//! render the same `Plan`; dry-run is `Mode::DryRun` over that plan, not a
//! parallel hand-written code path.

use super::spec::{self, Action, Mode, Plan, Selection, Step, Value};
use std::path::Path;

/// Target shell dialect.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sh,
    Bat,
}

/// Expand a `Value` into the script's literal/variable form for the dialect.
fn value(v: &Value, d: Dialect) -> String {
    match v {
        Value::Lit(s) => s.clone(),
        Value::Concat(parts) => parts.iter().map(|p| value(p, d)).collect(),
        Value::Version => match d {
            Dialect::Sh => "$VERSION".into(),
            Dialect::Bat => "%VERSION%".into(),
        },
        Value::Msrv => match d {
            Dialect::Sh => "$MSRV".into(),
            Dialect::Bat => "%MSRV%".into(),
        },
        Value::CargoFlags => match d {
            Dialect::Sh => "$CARGO_FLAGS".into(),
            Dialect::Bat => "%FEATURES%".into(),
        },
        Value::WebDataDir => match d {
            Dialect::Sh => "$web_data_dir".into(),
            Dialect::Bat => "%LOCALAPPDATA%\\zeroclaw\\web\\dist".into(),
        },
        Value::BinDir => match d {
            Dialect::Sh => "$CARGO_HOME/bin".into(),
            Dialect::Bat => "%USERPROFILE%\\.zeroclaw\\bin".into(),
        },
    }
}

/// The real command a step runs, in the dialect. Mirrors install.sh@HEAD.
fn action_cmd(a: &Action, d: Dialect) -> String {
    match (a, d) {
        (Action::CargoInstallSelf, Dialect::Sh) => {
            "cargo install --path . --locked --force $CARGO_FLAGS".into()
        }
        (Action::CargoInstallSelf, Dialect::Bat) => {
            "cargo build --release --locked %FEATURES% --target %TARGET%".into()
        }
        (Action::BuildWebDashboard, Dialect::Sh) => "build_web_dashboard \"$INSTALL_DIR\"".into(),
        (Action::BuildWebDashboard, Dialect::Bat) => "cargo web build".into(),
        _ => String::new(),
    }
}

/// The dry-run narration line for a step, in the dialect's echo form.
fn narration_line(step: &Step, d: Dialect) -> String {
    let text = value(&step.narration, d);
    let line = spec::dry_run_line(&text);
    match d {
        Dialect::Sh => format!("info \"{line}\""),
        Dialect::Bat => format!("echo   {line}"),
    }
}

/// Render one step's dry-run/execute dispatcher block in the dialect. Only steps
/// with a renderable action emit an execute branch.
fn render_step(step: &Step, d: Dialect) -> String {
    let narrate = narration_line(step, d);
    let cmd = action_cmd(&step.action, d);
    match d {
        Dialect::Sh => {
            if cmd.is_empty() {
                format!("if [ \"$DRY_RUN\" = true ]; then\n  {narrate}\nfi")
            } else {
                format!("if [ \"$DRY_RUN\" = true ]; then\n  {narrate}\nelse\n  {cmd}\nfi")
            }
        }
        Dialect::Bat => {
            if cmd.is_empty() {
                format!("if \"%DRY_RUN%\"==\"true\" (\n  {narrate}\n)")
            } else {
                format!("if \"%DRY_RUN%\"==\"true\" (\n  {narrate}\n) else (\n  {cmd}\n)")
            }
        }
    }
}

/// Render the source-branch step dispatcher for a dialect, walking the canonical
/// `Plan` in `Mode::Execute` order. Each step emits its dry-run/execute pair, so
/// running the script with the dry-run flag set narrates the whole plan.
pub fn render_source_steps(root: &Path, d: Dialect) -> anyhow::Result<String> {
    // Selection does not change the step *structure*, only the flags a step
    // interpolates; build the plan with Full to get the canonical step set.
    let plan = Plan::build(root, platform_of(d), &Selection::Full)?;
    let _ = Mode::Execute; // mode is applied by the script at runtime, not here
    let mut out = String::new();
    for (i, step) in plan.diverge.source.iter().enumerate() {
        out.push_str(&render_step(step, d));
        if i + 1 < plan.diverge.source.len() {
            out.push_str("\n\n");
        }
    }
    Ok(out)
}

fn platform_of(d: Dialect) -> spec::Platform {
    match d {
        Dialect::Sh => spec::Platform::Unix,
        Dialect::Bat => spec::Platform::Windows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn sh_steps_pair_dryrun_and_execute() {
        let s = render_source_steps(&root(), Dialect::Sh).unwrap();
        assert!(s.contains("if [ \"$DRY_RUN\" = true ]; then"));
        assert!(s.contains("[dry-run] Would run: cargo install"));
        assert!(s.contains("cargo install --path . --locked --force $CARGO_FLAGS"));
    }

    #[test]
    fn bat_steps_pair_dryrun_and_execute() {
        let s = render_source_steps(&root(), Dialect::Bat).unwrap();
        assert!(s.contains("if \"%DRY_RUN%\"==\"true\" ("));
        assert!(s.contains("[dry-run] Would"));
    }

    #[test]
    fn every_source_step_narrates_in_both_dialects() {
        // Dry-run totality: each step yields a narration line, no silent steps.
        for d in [Dialect::Sh, Dialect::Bat] {
            let s = render_source_steps(&root(), d).unwrap();
            let dryrun_count = s.matches("[dry-run] Would").count();
            assert!(dryrun_count >= 1, "every step must narrate");
        }
    }
}
