use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use zeroclaw_anti_slop::changed::{changed_rust_lines, collect_rust_files};
use zeroclaw_anti_slop::{RULES, check_source};

const DEFAULT_ROOTS: &[&str] = &["."];

#[derive(Debug, Default)]
struct Args {
    changed_since: Option<String>,
    help: bool,
    list_rules: bool,
    roots: Vec<PathBuf>,
    summary: bool,
}

fn main() -> ExitCode {
    match parse_args(env::args().skip(1)) {
        Ok(args) => run(args),
        Err(error) => {
            eprintln!("anti-slop: {error}\n\n{}", usage());
            ExitCode::from(2)
        }
    }
}

fn run(mut args: Args) -> ExitCode {
    if args.help {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    if args.list_rules {
        for (name, description) in RULES {
            println!("{name}: {description}");
        }
        return ExitCode::SUCCESS;
    }
    if args.roots.is_empty() {
        args.roots = DEFAULT_ROOTS.iter().map(PathBuf::from).collect();
    }
    let repo = match env::current_dir() {
        Ok(repo) => repo,
        Err(error) => {
            eprintln!("anti-slop: failed to determine current directory: {error}");
            return ExitCode::from(2);
        }
    };

    let changed = match args.changed_since.as_deref() {
        Some(base) => match changed_rust_lines(&repo, base, &args.roots) {
            Ok(changed) => Some(changed),
            Err(error) => {
                eprintln!("anti-slop: {error}");
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let files = match &changed {
        Some(changed) => changed
            .files()
            .filter(|path| repo.join(path).is_file())
            .cloned()
            .collect::<Vec<_>>(),
        None => match collect_rust_files(&repo, &args.roots) {
            Ok(files) => files,
            Err(error) => {
                eprintln!("anti-slop: {error}");
                return ExitCode::from(2);
            }
        },
    };
    let mut violation_count = 0;
    let mut file_counts = BTreeMap::<PathBuf, usize>::new();
    let mut rule_counts = BTreeMap::<&'static str, usize>::new();
    let mut failed = false;
    for path in &files {
        let source = match fs::read_to_string(repo.join(path)) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("{}: failed to read: {error}", path.display());
                failed = true;
                continue;
            }
        };
        let diagnostics = match check_source(path, &source) {
            Ok(diagnostics) => diagnostics,
            Err(error) => {
                eprintln!("{}: failed to parse: {error}", path.display());
                failed = true;
                continue;
            }
        };
        let baseline_diagnostics = match &changed {
            Some(changed) => match changed.baseline_source(&repo, path) {
                Ok(Some(source)) => match check_source(changed.baseline_path(path), &source) {
                    Ok(diagnostics) => diagnostics
                        .into_iter()
                        .map(|diagnostic| (diagnostic.line, diagnostic.column, diagnostic.rule))
                        .collect::<BTreeSet<_>>(),
                    Err(error) => {
                        eprintln!("{} at merge-base: failed to parse: {error}", path.display());
                        failed = true;
                        continue;
                    }
                },
                Ok(None) => BTreeSet::new(),
                Err(error) => {
                    eprintln!("{} at merge-base: {error}", path.display());
                    failed = true;
                    continue;
                }
            },
            None => BTreeSet::new(),
        };
        for diagnostic in diagnostics {
            if let Some(changed) = &changed {
                let existed_at_merge_base = changed
                    .old_line_for_new(path, diagnostic.line)
                    .is_some_and(|old_line| {
                        baseline_diagnostics.contains(&(
                            old_line,
                            diagnostic.column,
                            diagnostic.rule,
                        ))
                    });
                if !changed.contains(path, diagnostic.line) && existed_at_merge_base {
                    continue;
                }
            }
            if !args.summary {
                println!(
                    "{}:{}:{}: {}: {}",
                    path.display(),
                    diagnostic.line,
                    diagnostic.column,
                    diagnostic.rule,
                    diagnostic.message
                );
            }
            violation_count += 1;
            *file_counts.entry(path.clone()).or_default() += 1;
            *rule_counts.entry(diagnostic.rule).or_default() += 1;
        }
    }

    if failed {
        eprintln!("anti-slop: one or more files could not be checked");
        return ExitCode::from(2);
    }
    if violation_count > 0 {
        if args.summary {
            print_summary(&rule_counts, &file_counts);
        }
        println!(
            "anti-slop: found {violation_count} violation{} in {} checked Rust file{}",
            if violation_count == 1 { "" } else { "s" },
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        );
        return ExitCode::FAILURE;
    }
    println!(
        "anti-slop: checked {} Rust file{}; no violations",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    ExitCode::SUCCESS
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--changed-since" => {
                let base = args
                    .next()
                    .ok_or_else(|| "--changed-since requires a git revision".to_string())?;
                parsed.changed_since = Some(base);
            }
            "--list-rules" => parsed.list_rules = true,
            "--summary" => parsed.summary = true,
            "-h" | "--help" => parsed.help = true,
            _ if argument.starts_with('-') => return Err(format!("unknown option: {argument}")),
            _ => parsed.roots.push(PathBuf::from(argument)),
        }
    }
    Ok(parsed)
}

fn usage() -> &'static str {
    "Usage: anti-slop [--changed-since REV] [--list-rules] [--summary] [PATH ...]\n\
     \n\
     With --changed-since, findings on touched lines and findings newly exposed\n\
     since REV's merge-base with HEAD are enforced. Without it, every Rust file\n\
     below PATH is checked."
}

fn print_summary(
    rule_counts: &BTreeMap<&'static str, usize>,
    file_counts: &BTreeMap<PathBuf, usize>,
) {
    let mut rules: Vec<_> = rule_counts.iter().collect();
    rules.sort_by_key(|(rule, count)| (std::cmp::Reverse(**count), *rule));
    println!("By rule:");
    for (rule, count) in rules {
        println!("  {count:>5}  {rule}");
    }

    let mut files: Vec<_> = file_counts.iter().collect();
    files.sort_by(|(left_path, left_count), (right_path, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_path.cmp(right_path))
    });
    println!("Top files:");
    for (path, count) in files.into_iter().take(20) {
        println!("  {count:>5}  {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_changed_base_and_paths() {
        let args = parse_args([
            "--changed-since".to_string(),
            "origin/master".to_string(),
            "src".to_string(),
        ])
        .expect("arguments should parse");
        assert_eq!(args.changed_since.as_deref(), Some("origin/master"));
        assert_eq!(args.roots, [PathBuf::from("src")]);
    }

    #[test]
    fn rejects_unknown_options() {
        let error = parse_args(["--wat".to_string()]).expect_err("unknown option must fail");
        assert!(error.contains("unknown option"));
    }

    #[test]
    fn recognizes_help_without_treating_it_as_an_error() {
        let args = parse_args(["--help".to_string()]).expect("help should parse");
        assert!(args.help);
    }

    #[test]
    fn recognizes_summary_mode() {
        let args = parse_args(["--summary".to_string()]).expect("summary should parse");
        assert!(args.summary);
    }
}
