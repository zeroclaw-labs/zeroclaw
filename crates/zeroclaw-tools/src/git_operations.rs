use async_trait::async_trait;
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::policy::SecurityPolicy;

use crate::util_helpers::clean_verbatim_path;

/// Runtime-owned boundary applied to write-classified Git subprocesses.
///
/// The tool crate owns Git operation classification and must not depend on a
/// concrete runtime sandbox. The production registry injects the per-agent
/// sandbox through this narrow command-wrapping contract instead.
///
/// The boundary receives a bare `git` command before the tool applies Git
/// arguments, its working directory, or its environment. Boundaries that need
/// to remove inherited environment variables must use individual
/// [`std::process::Command::env_remove`] calls; a blanket `env_clear` is
/// superseded by the tool's Git environment hardening. Git-specific variables
/// set by a boundary are also discarded: the tool owns the Git environment.
pub trait GitCommandBoundary: Send + Sync {
    fn wrap_command(&self, command: &mut std::process::Command) -> anyhow::Result<()>;
}

struct UnconfiguredGitCommandBoundary;

impl GitCommandBoundary for UnconfiguredGitCommandBoundary {
    fn wrap_command(&self, _command: &mut std::process::Command) -> anyhow::Result<()> {
        anyhow::bail!("Git write commands require a configured execution boundary")
    }
}

#[cfg(test)]
#[derive(Default)]
struct DirectGitCommandBoundary;

#[cfg(test)]
impl GitCommandBoundary for DirectGitCommandBoundary {
    fn wrap_command(&self, _command: &mut std::process::Command) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Git operations tool for structured repository management.
/// Provides safe, parsed git operations with JSON output.
#[derive(Clone)]
pub struct GitOperationsTool {
    security: Arc<SecurityPolicy>,
    git_command_boundary: Arc<dyn GitCommandBoundary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RepositoryAuthorization {
    Authorized(PathBuf),
    NotFound,
    DiscoveryBoundaryReached,
    Denied,
}

#[derive(Default)]
struct ModuleMetadata {
    configs: BTreeSet<PathBuf>,
    objects: BTreeSet<PathBuf>,
}

struct ValidatedRepository {
    root: PathBuf,
    git_dir: PathBuf,
}

const READ_GIT_CONFIG_OVERRIDES: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.pager=cat",
    "-c",
    "log.showSignature=false",
    "-c",
    "log.mailmap=false",
];
const GIT_LOG_FORMAT: &str = "--pretty=format:%H|%an|%ae|%ad|%s";

impl GitOperationsTool {
    /// Construct the tool without a runtime execution boundary.
    ///
    /// Read-classified operations remain available. Write-classified operations
    /// fail closed; production callers should use
    /// [`Self::new_with_command_boundary`].
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self::new_with_command_boundary(security, Arc::new(UnconfiguredGitCommandBoundary))
    }

    /// Construct the tool with the runtime-owned execution boundary used for
    /// write-classified Git subprocesses.
    pub fn new_with_command_boundary(
        security: Arc<SecurityPolicy>,
        git_command_boundary: Arc<dyn GitCommandBoundary>,
    ) -> Self {
        Self {
            security,
            git_command_boundary,
        }
    }

    /// Sanitize git arguments to prevent injection attacks
    fn sanitize_git_args(&self, args: &str) -> anyhow::Result<Vec<String>> {
        let mut result = Vec::new();
        for arg in args.split_whitespace() {
            // Block dangerous git options that could lead to command injection
            let arg_lower = arg.to_lowercase();
            if arg_lower.starts_with("--exec=")
                || arg_lower.starts_with("--upload-pack=")
                || arg_lower.starts_with("--receive-pack=")
                || arg_lower.starts_with("--pager=")
                || arg_lower.starts_with("--editor=")
                || arg_lower == "--no-verify"
                || arg_lower.contains("$(")
                || arg_lower.contains('`')
                || arg.contains('|')
                || arg.contains(';')
                || arg.contains('>')
            {
                anyhow::bail!("Blocked potentially dangerous git argument: {arg}");
            }
            // Block `-c` config injection (exact match or `-c=...` prefix).
            // This must not false-positive on `--cached` or `-cached`.
            if arg_lower == "-c" || arg_lower.starts_with("-c=") {
                anyhow::bail!("Blocked potentially dangerous git argument: {arg}");
            }
            result.push(arg.to_string());
        }
        Ok(result)
    }

    /// Check if an operation requires write access.
    fn requires_write_access(&self, operation: &str, args: &serde_json::Value) -> bool {
        matches!(
            operation,
            "commit" | "add" | "checkout" | "reset" | "revert"
        ) || (operation == "stash"
            && !matches!(
                args.get("action").and_then(|value| value.as_str()),
                Some("list")
            ))
            || (operation == "worktree"
                && !matches!(
                    args.get("subcommand").and_then(|value| value.as_str()),
                    Some("list")
                ))
    }

    /// Return whether a repository's Git metadata is within any authorized root.
    ///
    /// Git normally discovers repositories by walking to parent directories. That
    /// would let an approved child path operate on an unapproved parent repository,
    /// so discovery must stop after it leaves every root that authorized the
    /// requested path.
    /// Linked worktrees use a `.git` file that points at a per-worktree Git
    /// directory, which then points at a common Git directory. Both indirection
    /// targets must be physical and independently authorized for the requested
    /// operation.
    fn has_repository_within_authorized_roots(
        &self,
        working_dir: &Path,
        authorized_roots: &[PathBuf],
        requires_write_access: bool,
    ) -> RepositoryAuthorization {
        let discovery_is_unbounded = !self.security.workspace_only;
        let mut current_dir = working_dir;
        loop {
            let git_metadata = current_dir.join(".git");
            if let Ok(metadata) = std::fs::symlink_metadata(&git_metadata) {
                let current_dir_is_authorized = discovery_is_unbounded
                    || authorized_roots
                        .iter()
                        .any(|root| current_dir.starts_with(root));
                // Git binds to the first `.git` entry it discovers. A rejected
                // entry must therefore deny the operation rather than allowing
                // discovery to continue to an ancestor repository.
                if metadata.file_type().is_dir() {
                    return if current_dir_is_authorized
                        && self
                            .metadata_directory_is_authorized(&git_metadata, requires_write_access)
                    {
                        RepositoryAuthorization::Authorized(current_dir.to_path_buf())
                    } else {
                        RepositoryAuthorization::Denied
                    };
                }
                if metadata.file_type().is_file() {
                    return if current_dir_is_authorized
                        && self.metadata_path_is_authorized(&git_metadata, requires_write_access)
                        && self.linked_worktree_metadata_is_authorized(
                            &git_metadata,
                            requires_write_access,
                        ) {
                        RepositoryAuthorization::Authorized(current_dir.to_path_buf())
                    } else {
                        RepositoryAuthorization::Denied
                    };
                }
                // A `.git` symlink or other non-regular entry is not an
                // acceptable metadata boundary and must not fall through.
                return RepositoryAuthorization::Denied;
            }
            let Some(parent) = current_dir.parent() else {
                return RepositoryAuthorization::NotFound;
            };
            if !discovery_is_unbounded
                && !authorized_roots.iter().any(|root| parent.starts_with(root))
            {
                return RepositoryAuthorization::DiscoveryBoundaryReached;
            }
            current_dir = parent;
        }
    }

    fn linked_worktree_metadata_is_authorized(
        &self,
        git_file: &Path,
        requires_write_access: bool,
    ) -> bool {
        let Ok(contents) = std::fs::read_to_string(git_file) else {
            return false;
        };
        let Some(gitdir) = contents.strip_prefix("gitdir: ") else {
            return false;
        };
        let gitdir = gitdir.trim_end_matches(['\r', '\n']);
        if gitdir.is_empty() || gitdir.contains('\n') || gitdir.contains('\r') {
            return false;
        }
        let gitdir = Path::new(gitdir);
        let gitdir = if gitdir.is_absolute() {
            gitdir.to_path_buf()
        } else {
            git_file
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(gitdir)
        };
        if !std::fs::symlink_metadata(&gitdir).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            return false;
        }
        let Ok(gitdir) = gitdir.canonicalize() else {
            return false;
        };
        // A gitfile without `commondir` is a submodule-style indirection.
        // `metadata_directory_is_authorized` checks the gitdir itself and,
        // when present, its linked-worktree common directory.
        self.metadata_directory_is_authorized(&gitdir, requires_write_access)
    }

    fn metadata_directory_is_authorized(&self, path: &Path, requires_write_access: bool) -> bool {
        if !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
            || !self.metadata_path_is_authorized(path, requires_write_access)
        {
            return false;
        }

        let commondir_file = path.join("commondir");
        match std::fs::symlink_metadata(&commondir_file) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => return false,
            Ok(metadata) if !metadata.file_type().is_file() => return false,
            Ok(_) => {}
        }
        let Ok(commondir) = std::fs::read_to_string(&commondir_file) else {
            return false;
        };
        let commondir = commondir.trim_end_matches(['\r', '\n']);
        if commondir.is_empty() || commondir.contains('\n') || commondir.contains('\r') {
            return false;
        }
        let commondir = Path::new(commondir);
        let commondir = if commondir.is_absolute() {
            commondir.to_path_buf()
        } else {
            path.join(commondir)
        };
        if !std::fs::symlink_metadata(&commondir)
            .is_ok_and(|metadata| metadata.file_type().is_dir())
        {
            return false;
        }
        let Ok(commondir) = commondir.canonicalize() else {
            return false;
        };
        self.metadata_path_is_authorized(&commondir, requires_write_access)
    }

    fn metadata_path_is_authorized(&self, path: &Path, requires_write_access: bool) -> bool {
        self.security.is_resolved_path_readable(path)
            && (!requires_write_access || self.security.is_resolved_path_allowed(path))
    }

    /// Validate the complete, currently reachable repository metadata tree before
    /// handing it to Git.  Validating only `.git`/`gitdir`/`commondir` leaves
    /// Git free to follow a later internal symlink (for example `index` or
    /// `objects`) outside the policy boundary.
    fn validate_metadata_closure(
        &self,
        repository_root: &Path,
        requires_write_access: bool,
    ) -> anyhow::Result<PathBuf> {
        let git = repository_root.join(".git");
        let metadata = std::fs::symlink_metadata(&git)?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("Git metadata symlink is not authorized");
        }
        let git_dir = if metadata.file_type().is_dir() {
            git.canonicalize()?
        } else {
            let contents = std::fs::read_to_string(&git)?;
            let value = contents
                .strip_prefix("gitdir: ")
                .ok_or_else(|| anyhow::Error::msg("Invalid Git metadata file"))?
                .trim_end_matches(['\r', '\n']);
            if value.is_empty() || value.contains(['\r', '\n']) {
                anyhow::bail!("Invalid Git metadata file");
            }
            let target = Path::new(value);
            let target = if target.is_absolute() {
                target.to_path_buf()
            } else {
                git.parent()
                    .ok_or_else(|| anyhow::Error::msg("Invalid Git metadata path"))?
                    .join(target)
            };
            if std::fs::symlink_metadata(&target)?.file_type().is_symlink() {
                anyhow::bail!("Git metadata symlink is not authorized");
            }
            target.canonicalize()?
        };
        let common_file = git_dir.join("commondir");
        let common_dir = if common_file.exists() {
            if std::fs::symlink_metadata(&common_file)?
                .file_type()
                .is_symlink()
            {
                anyhow::bail!("Git metadata symlink is not authorized");
            }
            let value = std::fs::read_to_string(&common_file)?;
            let value = value.trim_end_matches(['\r', '\n']);
            if value.is_empty() || value.contains(['\r', '\n']) {
                anyhow::bail!("Invalid Git commondir");
            }
            let target = Path::new(value);
            let target = if target.is_absolute() {
                target.to_path_buf()
            } else {
                git_dir.join(target)
            };
            if std::fs::symlink_metadata(&target)?.file_type().is_symlink() {
                anyhow::bail!("Git metadata symlink is not authorized");
            }
            target.canonicalize()?
        } else {
            git_dir.clone()
        };
        let mut visited = BTreeSet::new();
        let mut alternate_visited = BTreeSet::new();
        let mut module_metadata = ModuleMetadata::default();
        let module_roots = [git_dir.join("modules"), common_dir.join("modules")];
        self.validate_metadata_tree(
            &git_dir,
            requires_write_access,
            &mut visited,
            &mut module_metadata,
            &module_roots,
        )?;
        self.validate_metadata_tree(
            &common_dir,
            requires_write_access,
            &mut visited,
            &mut module_metadata,
            &module_roots,
        )?;
        self.validate_object_alternates(
            &git_dir,
            requires_write_access,
            &mut visited,
            &mut alternate_visited,
            &mut module_metadata,
            &module_roots,
        )?;
        self.validate_object_alternates(
            &common_dir,
            requires_write_access,
            &mut visited,
            &mut alternate_visited,
            &mut module_metadata,
            &module_roots,
        )?;
        self.validate_metadata_config(
            &git_dir,
            repository_root,
            requires_write_access,
            &mut visited,
            &mut module_metadata,
            &module_roots,
        )?;
        if common_dir != git_dir {
            self.validate_metadata_config(
                &common_dir,
                repository_root,
                requires_write_access,
                &mut visited,
                &mut module_metadata,
                &module_roots,
            )?;
        }
        let module_configs = module_metadata.configs.iter().cloned().collect::<Vec<_>>();
        let module_objects = module_metadata.objects.iter().cloned().collect::<Vec<_>>();
        for config in module_configs {
            self.validate_module_metadata_config(&config, requires_write_access)?;
        }
        for objects in module_objects {
            self.validate_object_alternates_at(
                &objects,
                requires_write_access,
                &mut visited,
                &mut alternate_visited,
                &mut module_metadata,
                &module_roots,
            )?;
        }
        Ok(git_dir)
    }

    fn validate_metadata_tree(
        &self,
        root: &Path,
        write: bool,
        visited: &mut BTreeSet<PathBuf>,
        module_metadata: &mut ModuleMetadata,
        module_roots: &[PathBuf],
    ) -> anyhow::Result<()> {
        let root = match root.canonicalize() {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !visited.insert(root.clone()) {
            return Ok(());
        }
        if !self.metadata_path_is_authorized(&root, write) {
            anyhow::bail!("Git metadata is not authorized");
        }
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let path = entry.path();
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if meta.file_type().is_symlink() {
                anyhow::bail!("Git metadata symlink is not authorized");
            }
            if !self.metadata_path_is_authorized(&path, write) {
                anyhow::bail!("Git metadata is not authorized");
            }
            if module_roots.iter().any(|root| path.starts_with(root)) {
                match path.file_name().and_then(|name| name.to_str()) {
                    Some("config") | Some("config.worktree") => {
                        module_metadata.configs.insert(path.clone());
                    }
                    Some("objects") if meta.file_type().is_dir() => {
                        module_metadata.objects.insert(path.clone());
                    }
                    _ => {}
                }
            }
            if meta.file_type().is_dir() {
                self.validate_metadata_tree(&path, write, visited, module_metadata, module_roots)?;
            } else if !meta.file_type().is_file() {
                // Git's fsmonitor daemon and lock-file lifecycle can leave
                // sockets/FIFOs in metadata. They are not pathname edges that
                // Git follows for these operations; symlinks above remain
                // rejected before any command is launched.
                continue;
            }
        }
        Ok(())
    }

    fn validate_object_alternates(
        &self,
        root: &Path,
        write: bool,
        visited: &mut BTreeSet<PathBuf>,
        alternate_visited: &mut BTreeSet<PathBuf>,
        module_metadata: &mut ModuleMetadata,
        module_roots: &[PathBuf],
    ) -> anyhow::Result<()> {
        let objects = root.join("objects");
        self.validate_object_alternates_at(
            &objects,
            write,
            visited,
            alternate_visited,
            module_metadata,
            module_roots,
        )
    }

    fn validate_object_alternates_at(
        &self,
        objects: &Path,
        write: bool,
        visited: &mut BTreeSet<PathBuf>,
        alternate_visited: &mut BTreeSet<PathBuf>,
        module_metadata: &mut ModuleMetadata,
        module_roots: &[PathBuf],
    ) -> anyhow::Result<()> {
        let objects = match objects.canonicalize() {
            Ok(objects) => objects,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !alternate_visited.insert(objects.clone()) {
            return Ok(());
        }
        let alternate_file = objects.join("info/alternates");
        if objects.join("info/http-alternates").exists() {
            anyhow::bail!("Remote Git alternates are not authorized");
        }
        if !alternate_file.exists() {
            return Ok(());
        }
        if std::fs::symlink_metadata(&alternate_file)?
            .file_type()
            .is_symlink()
        {
            anyhow::bail!("Git metadata symlink is not authorized");
        }
        for line in std::fs::read_to_string(alternate_file)?.split('\n') {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.contains('\r') {
                anyhow::bail!("Invalid Git object alternate");
            }
            // Git also accepts C-quoted alternate paths.  This validator does
            // not interpret that grammar, so reject the form rather than
            // validating its literal quote-prefixed spelling.
            if line.starts_with('"') {
                anyhow::bail!("Quoted Git object alternates are not authorized");
            }
            let target = Path::new(line);
            let target = if target.is_absolute() {
                target.to_path_buf()
            } else {
                objects.join(target)
            };
            if std::fs::symlink_metadata(&target)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                anyhow::bail!("Git metadata symlink is not authorized");
            }
            self.validate_metadata_tree(&target, write, visited, module_metadata, module_roots)?;
            self.validate_object_alternates_at(
                &target,
                write,
                visited,
                alternate_visited,
                module_metadata,
                module_roots,
            )?;
        }
        Ok(())
    }

    fn validate_metadata_config(
        &self,
        directory: &Path,
        repository_root: &Path,
        write: bool,
        visited: &mut BTreeSet<PathBuf>,
        module_metadata: &mut ModuleMetadata,
        module_roots: &[PathBuf],
    ) -> anyhow::Result<()> {
        for config in [directory.join("config"), directory.join("config.worktree")] {
            if !config.exists() {
                continue;
            }
            let keys = Self::git_config_values(&config, &["--name-only", "--list"])?;
            if keys.iter().any(|key| {
                key.eq_ignore_ascii_case("include.path")
                    || (key.to_ascii_lowercase().starts_with("includeif.")
                        && key.to_ascii_lowercase().ends_with(".path"))
            }) {
                anyhow::bail!("Repository config includes are not authorized");
            }
            let hooks_values = Self::git_config_values(&config, &["--get-all", "core.hooksPath"])?;
            self.validate_configured_metadata_paths(&config, &keys, repository_root, write)?;
            if write
                && hooks_values.is_empty()
                && keys
                    .iter()
                    .any(|key| key.eq_ignore_ascii_case("core.hookspath"))
            {
                anyhow::bail!("Empty Git hooks path is not authorized");
            }
            for value in hooks_values {
                if !write {
                    continue;
                }
                // Git expands `~`, strips its `:(optional)` pathname prefix,
                // and interpolates `%(prefix)/` before resolving hooksPath.
                // Do not validate those raw spellings as in-worktree paths.
                if value.starts_with('~') || value.starts_with(":(") || value.starts_with("%(") {
                    anyhow::bail!("Interpolated Git hooks path is not authorized");
                }
                if Path::new(&value)
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
                {
                    anyhow::bail!("Git hooks path is not authorized");
                }
                let path = Path::new(&value);
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    repository_root.join(path)
                };
                if std::fs::symlink_metadata(&path)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    anyhow::bail!("Git metadata symlink is not authorized");
                }
                if path.exists() {
                    self.validate_metadata_tree(
                        &path,
                        write,
                        visited,
                        module_metadata,
                        module_roots,
                    )?;
                } else if !self.metadata_path_is_authorized(&path, false) {
                    anyhow::bail!("Git hooks path is not authorized");
                }
            }
        }
        Ok(())
    }

    fn validate_configured_metadata_paths(
        &self,
        config: &Path,
        keys: &[String],
        repository_root: &Path,
        write: bool,
    ) -> anyhow::Result<()> {
        for key in ["core.attributesFile", "core.excludesFile"] {
            let values = Self::git_config_values(config, &["--get-all", key])?;
            if values.is_empty()
                && keys
                    .iter()
                    .any(|configured| configured.eq_ignore_ascii_case(key))
            {
                anyhow::bail!("Empty Git metadata config path is not authorized");
            }
            for value in values {
                self.validate_configured_metadata_path(&value, Some(repository_root), write)?;
            }
        }
        Ok(())
    }

    fn validate_configured_metadata_path(
        &self,
        value: &str,
        relative_base: Option<&Path>,
        write: bool,
    ) -> anyhow::Result<()> {
        // Git expands `~` and `%(prefix)` in pathname configuration values.
        // The closure deliberately does not interpret those spellings; treating
        // them as literal in-worktree paths would validate a different file.
        if value.is_empty()
            || value.starts_with('~')
            || value.starts_with("%(")
            || value.starts_with(":(")
        {
            anyhow::bail!("Interpolated Git metadata config path is not authorized");
        }
        let configured = Path::new(value);
        if configured
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            anyhow::bail!("Git metadata config path is not authorized");
        }
        let path = if configured.is_absolute() {
            configured.to_path_buf()
        } else {
            relative_base
                .ok_or_else(|| {
                    anyhow::Error::msg("Relative module metadata path is not authorized")
                })?
                .join(configured)
        };
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                    anyhow::bail!("Git metadata config path is not authorized");
                }
                if !self.metadata_path_is_authorized(&path, write) {
                    anyhow::bail!("Git metadata config path is not authorized");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !self.metadata_path_is_authorized(&path, false) {
                    anyhow::bail!("Git metadata config path is not authorized");
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn validate_module_metadata_config(&self, config: &Path, write: bool) -> anyhow::Result<()> {
        let keys = Self::git_config_values(config, &["--name-only", "--list"])?;
        if keys.iter().any(|key| {
            key.eq_ignore_ascii_case("include.path")
                || (key.to_ascii_lowercase().starts_with("includeif.")
                    && key.to_ascii_lowercase().ends_with(".path"))
        }) {
            anyhow::bail!("Repository config includes are not authorized");
        }
        if write
            && keys
                .iter()
                .any(|key| key.eq_ignore_ascii_case("core.hookspath"))
        {
            // A module config is discovered from its Git directory, not its
            // worktree, so a relative hooksPath cannot be resolved safely here.
            anyhow::bail!("Module Git hooks path is not authorized");
        }
        for key in ["core.attributesFile", "core.excludesFile"] {
            for value in Self::git_config_values(config, &["--get-all", key])? {
                self.validate_configured_metadata_path(&value, None, write)?;
            }
        }
        Ok(())
    }

    fn git_config_values(config: &Path, query: &[&str]) -> anyhow::Result<Vec<String>> {
        let mut command = std::process::Command::new("git");
        command
            .args(["config", "--file"])
            .arg(clean_verbatim_path(config))
            .arg("--no-includes")
            .arg("--null")
            .args(query)
            .current_dir(Self::git_subprocess_current_dir(
                config.parent().unwrap_or_else(|| Path::new(".")),
            ))
            .stdin(std::process::Stdio::null());
        Self::configure_git_base_environment(&mut command);
        command.env("GIT_CONFIG_NOSYSTEM", "1");
        let output = command.output()?;
        if !output.status.success() {
            if output.status.code() == Some(1) {
                return Ok(Vec::new());
            }
            anyhow::bail!("Cannot inspect Git metadata configuration");
        }
        output
            .stdout
            .split(|byte| *byte == b'\0')
            .filter(|value| !value.is_empty())
            .map(|value| {
                std::str::from_utf8(value)
                    .map(str::to_owned)
                    .map_err(Into::into)
            })
            .collect()
    }

    fn candidate_path(&self, raw_path: &str) -> anyhow::Result<PathBuf> {
        if raw_path.contains('\0') {
            anyhow::bail!("Path not allowed: contains null byte");
        }
        if Path::new(raw_path)
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            anyhow::bail!("Path not allowed: parent-directory traversal is not allowed");
        }
        let path = Path::new(raw_path);
        Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.security.workspace_dir.join(path)
        })
    }

    fn ensure_worktree_add_target_allowed(&self, raw_path: &str) -> anyhow::Result<PathBuf> {
        let candidate = self.candidate_path(raw_path)?;
        let parent = candidate
            .parent()
            .ok_or_else(|| anyhow::Error::msg("Worktree path must have a parent directory"))?;
        let name = candidate.file_name().ok_or_else(|| {
            anyhow::Error::msg("Worktree path must include a final path component")
        })?;
        let parent = parent.canonicalize().map_err(|error| {
            anyhow::Error::msg(format!(
                "Cannot resolve worktree parent '{}': {error}",
                parent.display()
            ))
        })?;
        let target = parent.join(name);
        if !self.metadata_path_is_authorized(&target, true) {
            anyhow::bail!(
                "Worktree path '{}' resolves outside the workspace or allowed roots",
                raw_path
            );
        }
        Ok(target)
    }

    fn ensure_worktree_remove_target_allowed(&self, raw_path: &str) -> anyhow::Result<PathBuf> {
        let candidate = self.candidate_path(raw_path)?;
        let resolved = candidate.canonicalize().map_err(|error| {
            anyhow::Error::msg(format!(
                "Cannot resolve worktree path '{}': {error}",
                raw_path
            ))
        })?;
        if !self.metadata_path_is_authorized(&resolved, true) {
            anyhow::bail!(
                "Worktree path '{}' resolves outside the workspace or allowed roots",
                raw_path
            );
        }
        Ok(resolved)
    }

    /// Resolve an explicit path through the security policy, or return the
    /// policy's canonical workspace directory when no path is provided.
    fn resolve_working_dir(
        &self,
        path: Option<&str>,
        requires_write_access: bool,
    ) -> anyhow::Result<std::path::PathBuf> {
        let base = match path {
            Some(p) if !p.is_empty() => {
                let candidate = if std::path::Path::new(p).is_absolute() {
                    std::path::PathBuf::from(p)
                } else {
                    self.security.workspace_dir.join(p)
                };
                let resolved = candidate.canonicalize().map_err(|e| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "path": p,
                                "error": format!("{}", e),
                            })),
                        "git_operations: cannot resolve path"
                    );
                    anyhow::Error::msg(crate::i18n::get_required_tool_string_with_args(
                        "tool-git-operations-error-path-not-authorized",
                        &[("path", p)],
                    ))
                })?;
                if !self.security.is_resolved_path_readable(&resolved)
                    || (requires_write_access && !self.security.is_resolved_path_allowed(&resolved))
                {
                    anyhow::bail!(crate::i18n::get_required_tool_string_with_args(
                        "tool-git-operations-error-path-not-authorized",
                        &[("path", p)],
                    ));
                }
                resolved
            }
            _ => self
                .security
                .workspace_dir
                .canonicalize()
                .map_err(|error| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "path": self.security.workspace_dir,
                                "error": format!("{error}"),
                            })),
                        "git_operations: cannot resolve workspace path"
                    );
                    anyhow::Error::msg(crate::i18n::get_required_tool_string_with_args(
                        "tool-git-operations-error-path-not-authorized",
                        &[("path", &self.security.workspace_dir.display().to_string())],
                    ))
                })?,
        };
        Ok(base)
    }

    async fn run_git_command(
        &self,
        args: &[&str],
        working_dir: &std::path::Path,
    ) -> anyhow::Result<String> {
        let repository = self
            .validated_repository_root_async(working_dir, true)
            .await?;
        let mut command = tokio::process::Command::new("git");
        self.git_command_boundary
            .wrap_command(command.as_std_mut())
            .map_err(|error| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{error}")})),
                    "git_operations: write command boundary rejected Git subprocess"
                );
                error
            })?;
        let boundary_env = Self::non_git_command_env_snapshot(command.as_std());
        Self::bind_git_worktree(command.as_std_mut(), &repository.root, &repository.git_dir);
        command
            .args(args)
            .current_dir(Self::git_subprocess_current_dir(working_dir))
            .stdin(std::process::Stdio::null());
        self.configure_git_environment(command.as_std_mut(), working_dir, true)?;
        Self::restore_command_env(command.as_std_mut(), boundary_env);
        let output = command.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git command failed: {stderr}");
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run a read-classified Git command without repository-configured command
    /// hooks. `status` can invoke `core.fsmonitor`; `log` and `stash list` can
    /// invoke `gpg.program` for signature display; `log` disables unsupported
    /// mailmap resolution. Together with the fixed raw author placeholders in
    /// `git_log`, that prevents Git from reading `.mailmap` or `mailmap.file`.
    /// `diff` also disables its external-diff, text-conversion, and nested
    /// submodule-diff paths at the call site below.
    async fn run_git_read_command(
        &self,
        args: &[&str],
        working_dir: &std::path::Path,
    ) -> anyhow::Result<String> {
        let repository = self
            .validated_repository_root_async(working_dir, false)
            .await?;
        let filter_drivers = self
            .configured_filter_drivers(working_dir, &repository)
            .await?;
        let mut command = tokio::process::Command::new("git");
        Self::bind_git_worktree(command.as_std_mut(), &repository.root, &repository.git_dir);
        command
            .args(READ_GIT_CONFIG_OVERRIDES)
            .current_dir(Self::git_subprocess_current_dir(working_dir))
            .stdin(std::process::Stdio::null());
        Self::disable_filter_drivers(command.as_std_mut(), &filter_drivers);
        command.args(args);
        self.configure_git_environment(command.as_std_mut(), working_dir, false)?;
        let output = command.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git command failed: {stderr}");
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Return all Git filter drivers defined by the effective configuration.
    ///
    /// Attribute lookup alone is insufficient because Git may need to inspect
    /// content before it knows which configured driver applies. Reading config
    /// key names does not run a driver, so this preflight lets the real read
    /// command disable every configured clean, smudge, and process filter.
    async fn configured_filter_drivers(
        &self,
        working_dir: &Path,
        repository: &ValidatedRepository,
    ) -> anyhow::Result<Vec<String>> {
        let mut command = tokio::process::Command::new("git");
        Self::bind_git_worktree(command.as_std_mut(), &repository.root, &repository.git_dir);
        command
            .args([
                "config",
                "--null",
                "--name-only",
                "--includes",
                "--get-regexp",
                r"^filter\..*\.(clean|smudge|process|required)$",
            ])
            .current_dir(Self::git_subprocess_current_dir(working_dir))
            .stdin(std::process::Stdio::null());
        self.configure_git_environment(command.as_std_mut(), working_dir, false)?;
        let output = command.output().await?;
        if !output.status.success() {
            if output.status.code() == Some(1) {
                return Ok(Vec::new());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git filter configuration query failed: {stderr}");
        }

        let mut drivers = Vec::new();
        for key in output.stdout.split(|byte| *byte == b'\0') {
            if key.is_empty() {
                continue;
            }
            let key = std::str::from_utf8(key)?;
            let Some(driver) = Self::filter_driver_from_config_key(key) else {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "git_operations: Git filter configuration cannot be disabled safely"
                );
                anyhow::bail!("Git filter configuration cannot be disabled safely: {key}");
            };
            drivers.push(driver);
        }
        drivers.sort_unstable();
        drivers.dedup();
        Ok(drivers)
    }

    fn filter_driver_from_config_key(key: &str) -> Option<String> {
        let driver = key
            .strip_prefix("filter.")?
            .strip_suffix(".clean")
            .or_else(|| key.strip_prefix("filter.")?.strip_suffix(".smudge"))
            .or_else(|| key.strip_prefix("filter.")?.strip_suffix(".process"))
            .or_else(|| key.strip_prefix("filter.")?.strip_suffix(".required"))?;
        if driver.is_empty() || driver.contains('=') {
            return None;
        }
        Some(driver.to_owned())
    }

    fn disable_filter_drivers(command: &mut std::process::Command, filter_drivers: &[String]) {
        for driver in filter_drivers {
            for setting in ["clean=", "smudge=", "process=", "required=false"] {
                command.arg("-c").arg(format!("filter.{driver}.{setting}"));
            }
        }
    }

    fn configure_git_environment(
        &self,
        command: &mut std::process::Command,
        working_dir: &Path,
        requires_write_access: bool,
    ) -> anyhow::Result<()> {
        // Git accepts a broad and evolving set of environment overrides. Start
        // from an empty Git-specific environment and add back only the fixed,
        // non-interactive values this invocation needs below.
        Self::configure_git_base_environment(command);
        if self.security.workspace_only {
            let authorized_roots = if requires_write_access {
                self.security.approved_write_roots(working_dir)
            } else {
                self.security.approved_read_roots(working_dir)
            };
            let Some(outermost_root) = authorized_roots
                .iter()
                .filter(|root| working_dir.starts_with(root))
                .min_by_key(|root| root.components().count())
            else {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "path": working_dir })),
                    "git_operations: Git discovery ceiling has no authorized root"
                );
                anyhow::bail!(
                    "Git discovery ceiling cannot determine an authorized root for '{}'",
                    working_dir.display()
                );
            };
            if let Some(root) = outermost_root.parent() {
                let ceiling = Self::git_discovery_ceiling_path(root)?;
                // Git parses this variable as a platform-separated path list.
                // A separator in an authorized path would make Git discard or
                // misinterpret the ceiling, reopening parent discovery.
                let path_separator = if cfg!(windows) { ';' } else { ':' };
                if ceiling
                    .as_os_str()
                    .to_string_lossy()
                    .contains(path_separator)
                {
                    anyhow::bail!(
                        "Git discovery ceiling cannot represent authorized root '{}'",
                        ceiling.display()
                    );
                }
                command.env("GIT_CEILING_DIRECTORIES", ceiling);
            }
        }
        Ok(())
    }

    fn configure_git_base_environment(command: &mut std::process::Command) {
        let inherited_non_git_env = std::env::vars_os()
            .filter(|(name, _)| !Self::is_git_environment_variable(name))
            .collect::<Vec<_>>();
        command.env_clear().envs(inherited_non_git_env);
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat");
    }

    /// Preserve sandbox-provided environment entries while retaining the Git
    /// environment allowlist configured after command wrapping. A boundary
    /// must not be able to restore a Git-specific override that this tool
    /// deliberately strips before execution.
    fn non_git_command_env_snapshot(
        command: &std::process::Command,
    ) -> Vec<(OsString, Option<OsString>)> {
        command
            .get_envs()
            .filter(|(name, _)| !Self::is_git_environment_variable(name))
            .map(|(name, value)| {
                (
                    name.to_os_string(),
                    value.map(std::ffi::OsStr::to_os_string),
                )
            })
            .collect()
    }

    fn restore_command_env(
        command: &mut std::process::Command,
        environment: Vec<(OsString, Option<OsString>)>,
    ) {
        for (name, value) in environment {
            match value {
                Some(value) => {
                    command.env(name, value);
                }
                None => {
                    command.env_remove(name);
                }
            }
        }
    }

    async fn validated_repository_root_async(
        &self,
        working_dir: &Path,
        requires_write_access: bool,
    ) -> anyhow::Result<ValidatedRepository> {
        let tool = self.clone();
        let working_dir = working_dir.to_path_buf();
        let validation_working_dir = working_dir.clone();
        let validation = tokio::task::spawn_blocking(move || {
            tool.validated_repository_root(&validation_working_dir, requires_write_access)
        })
        .await
        .map_err(|error| {
            anyhow::Error::msg(format!("Git metadata validation task failed: {error}"))
        })?;
        validation.map_err(|error| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": working_dir,
                        "requires_write_access": requires_write_access,
                        "error_key": "tool-git-operations-error-repository-not-authorized",
                    })),
                "git_operations: repository metadata validation failed"
            );
            error.context(crate::i18n::get_required_tool_string_with_args(
                "tool-git-operations-error-repository-not-authorized",
                &[("path", &working_dir.display().to_string())],
            ))
        })
    }

    fn validated_repository_root(
        &self,
        working_dir: &Path,
        requires_write_access: bool,
    ) -> anyhow::Result<ValidatedRepository> {
        let authorized_roots = if requires_write_access {
            self.security.approved_write_roots(working_dir)
        } else {
            self.security.approved_read_roots(working_dir)
        };
        match self.has_repository_within_authorized_roots(
            working_dir,
            &authorized_roots,
            requires_write_access,
        ) {
            RepositoryAuthorization::Authorized(repository_root) => {
                let git_dir =
                    self.validate_metadata_closure(&repository_root, requires_write_access)?;
                Ok(ValidatedRepository {
                    root: repository_root,
                    git_dir,
                })
            }
            _ => anyhow::bail!(
                "Git repository authorization changed before command execution for '{}'",
                working_dir.display()
            ),
        }
    }

    fn is_git_environment_variable(name: &std::ffi::OsStr) -> bool {
        name.to_string_lossy()
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GIT_"))
    }

    fn bind_git_worktree(
        command: &mut std::process::Command,
        repository_root: &Path,
        git_dir: &Path,
    ) {
        command
            .arg("--git-dir")
            .arg(clean_verbatim_path(git_dir))
            .arg("--work-tree")
            .arg(clean_verbatim_path(repository_root));
    }

    fn git_subprocess_current_dir(path: &Path) -> PathBuf {
        clean_verbatim_path(path)
    }

    fn git_discovery_ceiling_path(root: &Path) -> anyhow::Result<PathBuf> {
        #[cfg(windows)]
        {
            let root = clean_verbatim_path(root);
            let Some(root) = root.to_str() else {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "git_operations: Git discovery ceiling has non-Unicode root"
                );
                anyhow::bail!("Git discovery ceiling cannot represent non-Unicode authorized root");
            };
            Ok(PathBuf::from(root))
        }
        #[cfg(not(windows))]
        {
            Ok(root.to_path_buf())
        }
    }

    fn git_worktree_path(path: &Path) -> PathBuf {
        clean_verbatim_path(path)
    }

    async fn git_status(
        &self,
        _args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let output = self
            .run_git_read_command(
                &[
                    "--no-optional-locks",
                    "status",
                    "--ignore-submodules=dirty",
                    "--porcelain=2",
                    "--branch",
                ],
                working_dir,
            )
            .await?;

        // Parse git status output into structured format
        let mut result = serde_json::Map::new();
        let mut branch = String::new();
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();

        for line in output.lines() {
            if line.starts_with("# branch.head ") {
                branch = line.trim_start_matches("# branch.head ").to_string();
            } else if let Some(rest) = line.strip_prefix("1 ") {
                // Ordinary changed entry
                let mut parts = rest.splitn(3, ' ');
                if let (Some(staging), Some(path)) = (parts.next(), parts.next())
                    && !staging.is_empty()
                {
                    let status_char = staging.chars().next().unwrap_or(' ');
                    if status_char != '.' && status_char != ' ' {
                        staged.push(json!({"path": path, "status": status_char}));
                    }
                    let status_char = staging.chars().nth(1).unwrap_or(' ');
                    if status_char != '.' && status_char != ' ' {
                        unstaged.push(json!({"path": path, "status": status_char}));
                    }
                }
            } else if let Some(rest) = line.strip_prefix("? ") {
                untracked.push(rest.to_string());
            }
        }

        result.insert("branch".to_string(), json!(branch));
        result.insert("staged".to_string(), json!(staged));
        result.insert("unstaged".to_string(), json!(unstaged));
        result.insert("untracked".to_string(), json!(untracked));
        result.insert(
            "clean".to_string(),
            json!(staged.is_empty() && unstaged.is_empty() && untracked.is_empty()),
        );

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)
                .unwrap_or_default()
                .into(),
            error: None,
        })
    }

    async fn git_diff(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let files = args.get("files").and_then(|v| v.as_str()).unwrap_or(".");
        let cached = args
            .get("cached")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Validate files argument against injection patterns
        self.sanitize_git_args(files)?;

        let mut git_args = vec![
            "--no-optional-locks",
            "diff",
            "--ignore-submodules=dirty",
            // Override repository `diff.submodule=diff`, whose nested Git
            // process does not inherit this command's external-diff guards.
            "--submodule=short",
            "--no-ext-diff",
            "--no-textconv",
            "--unified=3",
        ];
        if cached {
            git_args.push("--cached");
        }
        git_args.push("--");
        git_args.push(files);

        let output = self.run_git_read_command(&git_args, working_dir).await?;

        // Parse diff into structured hunks
        let mut result = serde_json::Map::new();
        let mut hunks = Vec::new();
        let mut current_file = String::new();
        let mut current_hunk = serde_json::Map::new();
        let mut lines = Vec::new();

        for line in output.lines() {
            if line.starts_with("diff --git ") {
                if !lines.is_empty() {
                    current_hunk.insert("lines".to_string(), json!(lines));
                    if !current_hunk.is_empty() {
                        hunks.push(serde_json::Value::Object(current_hunk.clone()));
                    }
                    lines = Vec::new();
                    current_hunk = serde_json::Map::new();
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    current_file = parts[3].trim_start_matches("b/").to_string();
                    current_hunk.insert("file".to_string(), json!(current_file));
                }
            } else if line.starts_with("@@ ") {
                if !lines.is_empty() {
                    current_hunk.insert("lines".to_string(), json!(lines));
                    if !current_hunk.is_empty() {
                        hunks.push(serde_json::Value::Object(current_hunk.clone()));
                    }
                    lines = Vec::new();
                    current_hunk = serde_json::Map::new();
                    current_hunk.insert("file".to_string(), json!(current_file));
                }
                current_hunk.insert("header".to_string(), json!(line));
            } else if !line.is_empty() {
                lines.push(json!({
                    "text": line,
                    "type": if line.starts_with('+') { "add" }
                           else if line.starts_with('-') { "delete" }
                           else { "context" }
                }));
            }
        }

        if !lines.is_empty() {
            current_hunk.insert("lines".to_string(), json!(lines));
            if !current_hunk.is_empty() {
                hunks.push(serde_json::Value::Object(current_hunk));
            }
        }

        result.insert("hunks".to_string(), json!(hunks));
        result.insert("file_count".to_string(), json!(hunks.len()));

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)
                .unwrap_or_default()
                .into(),
            error: None,
        })
    }

    async fn git_log(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let limit_raw = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
        let limit = usize::try_from(limit_raw).unwrap_or(usize::MAX).min(1000);
        let limit_str = limit.to_string();

        let output = self
            .run_git_read_command(
                &[
                    "--no-optional-locks",
                    "log",
                    &format!("-{limit_str}"),
                    GIT_LOG_FORMAT,
                    "--date=iso",
                ],
                working_dir,
            )
            .await?;

        let mut commits = Vec::new();

        for line in output.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 5 {
                commits.push(json!({
                    "hash": parts[0],
                    "author": parts[1],
                    "email": parts[2],
                    "date": parts[3],
                    "message": parts[4]
                }));
            }
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({ "commits": commits }))
                .unwrap_or_default()
                .into(),
            error: None,
        })
    }

    async fn git_branch(
        &self,
        _args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let output = self
            .run_git_read_command(
                &[
                    "--no-optional-locks",
                    "branch",
                    "--format=%(refname:short)|%(HEAD)",
                ],
                working_dir,
            )
            .await?;

        let mut branches = Vec::new();
        let mut current = String::new();

        for line in output.lines() {
            if let Some((name, head)) = line.split_once('|') {
                let is_current = head == "*";
                if is_current {
                    current = name.to_string();
                }
                branches.push(json!({
                    "name": name,
                    "current": is_current
                }));
            }
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "current": current,
                "branches": branches
            }))
            .unwrap_or_default()
            .into(),
            error: None,
        })
    }

    fn truncate_commit_message(message: &str) -> String {
        if message.chars().count() > 2000 {
            format!("{}...", message.chars().take(1997).collect::<String>())
        } else {
            message.to_string()
        }
    }

    async fn git_commit(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "message"})),
                    "git_operations: missing message parameter"
                );
                anyhow::Error::msg("Missing 'message' parameter")
            })?;

        let trimmed_lines: Vec<&str> = message.lines().map(|l| l.trim_end()).collect();
        // Drop leading blank lines.
        let trimmed_lines = trimmed_lines
            .iter()
            .copied()
            .skip_while(|l| l.is_empty())
            .collect::<Vec<_>>();
        // Collapse runs of more than 2 consecutive blank lines to 2.
        let mut sanitized_lines: Vec<&str> = Vec::with_capacity(trimmed_lines.len());
        let mut consecutive_blanks = 0usize;
        for line in &trimmed_lines {
            if line.is_empty() {
                consecutive_blanks += 1;
                if consecutive_blanks <= 2 {
                    sanitized_lines.push(line);
                }
            } else {
                consecutive_blanks = 0;
                sanitized_lines.push(line);
            }
        }
        // Drop trailing blank lines.
        while sanitized_lines.last().is_some_and(|l: &&str| l.is_empty()) {
            sanitized_lines.pop();
        }
        let sanitized = sanitized_lines.join("\n");

        if sanitized.is_empty() {
            anyhow::bail!("Commit message cannot be empty");
        }

        // Limit message length
        let message = Self::truncate_commit_message(&sanitized);

        let output = self
            .run_git_command(&["commit", "-m", &message], working_dir)
            .await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Committed: {message}").into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Commit failed: {e}")),
            }),
        }
    }

    async fn git_add(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let paths = args.get("paths").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "paths"})),
                "git_operations: missing paths parameter"
            );
            anyhow::Error::msg("Missing 'paths' parameter")
        })?;

        // Validate paths against injection patterns. Returns each
        // whitespace-separated pathspec as its own argument so the join is
        // not handed to git as a single literal path.
        let sanitized = self.sanitize_git_args(paths)?;
        if sanitized.is_empty() {
            anyhow::bail!("No paths to stage");
        }

        let mut git_args: Vec<&str> = vec!["add", "--"];
        git_args.extend(sanitized.iter().map(String::as_str));

        let output = self.run_git_command(&git_args, working_dir).await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Staged: {}", sanitized.join(" ")).into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Add failed: {e}")),
            }),
        }
    }

    async fn git_checkout(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let branch = args.get("branch").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "branch"})),
                "git_operations: missing branch parameter"
            );
            anyhow::Error::msg("Missing 'branch' parameter")
        })?;

        // Sanitize branch name
        let sanitized = self.sanitize_git_args(branch)?;

        if sanitized.is_empty() || sanitized.len() > 1 {
            anyhow::bail!("Invalid branch specification");
        }

        let branch_name = &sanitized[0];

        // Block dangerous branch names
        if branch_name.contains('@') || branch_name.contains('^') || branch_name.contains('~') {
            anyhow::bail!("Branch name contains invalid characters");
        }

        let output = self
            .run_git_command(&["checkout", branch_name], working_dir)
            .await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Switched to branch: {branch_name}").into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Checkout failed: {e}")),
            }),
        }
    }

    async fn git_stash(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("push");

        let output = match action {
            "push" | "save" => {
                let message = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto-stash")
                    .to_string();
                let keep_index = args
                    .get("keep_index")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let include_untracked = args
                    .get("include_untracked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let paths_raw = args
                    .get("paths")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let mut cmd: Vec<String> =
                    vec!["stash".into(), "push".into(), "-m".into(), message];
                if keep_index {
                    cmd.push("-k".into());
                }
                if include_untracked {
                    cmd.push("-u".into());
                }
                if !paths_raw.is_empty() {
                    cmd.push("--".into());
                    for p in paths_raw.split_whitespace() {
                        cmd.push(p.to_string());
                    }
                }
                let cmd_refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
                self.run_git_command(&cmd_refs, working_dir).await
            }
            "pop" => self.run_git_command(&["stash", "pop"], working_dir).await,
            "list" => {
                self.run_git_read_command(&["--no-optional-locks", "stash", "list"], working_dir)
                    .await
            }
            "drop" => {
                let index_raw = args.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                let index = i32::try_from(index_raw).map_err(|_| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"index": index_raw})),
                        "git_operations: stash index too large"
                    );
                    anyhow::Error::msg(format!("stash index too large: {index_raw}"))
                })?;
                self.run_git_command(
                    &["stash", "drop", &format!("stash@{{{index}}}")],
                    working_dir,
                )
                .await
            }
            _ => anyhow::bail!("Unknown stash action: {action}. Use: push, pop, list, drop"),
        };

        match output {
            Ok(out) => Ok(ToolResult {
                success: true,
                output: out.into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Stash {action} failed: {e}")),
            }),
        }
    }

    fn authorized_worktree_list_path(&self, raw_path: &str) -> Option<PathBuf> {
        let Ok(path) = self.resolve_working_dir(Some(raw_path), false) else {
            return None;
        };
        let roots = self.security.approved_read_roots(&path);
        if matches!(
            self.has_repository_within_authorized_roots(&path, &roots, false),
            RepositoryAuthorization::Authorized(_)
        ) {
            Some(clean_verbatim_path(&path))
        } else {
            None
        }
    }

    fn parse_worktree_list(&self, output: &str, active_worktree: &Path) -> serde_json::Value {
        let mut worktrees = Vec::new();
        let mut current_path = String::new();
        let mut current_branch = String::new();
        let mut current_head = String::new();
        let mut is_detached = false;
        for line in output.lines() {
            if line.is_empty() {
                if !current_path.is_empty()
                    && let Some(authorized_path) = self.authorized_worktree_list_path(&current_path)
                {
                    worktrees.push((
                        authorized_path.to_string_lossy().into_owned(),
                        std::mem::take(&mut current_branch),
                        std::mem::take(&mut current_head),
                        is_detached,
                    ));
                }
                current_path.clear();
                current_branch.clear();
                current_head.clear();
                is_detached = false;
            } else if let Some(path) = line.strip_prefix("worktree ") {
                current_path = path.to_string();
            } else if let Some(head) = line.strip_prefix("HEAD ") {
                current_head = head.to_string();
            } else if let Some(branch) = line.strip_prefix("branch ") {
                current_branch = branch.trim_start_matches("refs/heads/").to_string();
            } else if line == "detached" {
                is_detached = true;
            }
        }
        if !current_path.is_empty()
            && let Some(authorized_path) = self.authorized_worktree_list_path(&current_path)
        {
            worktrees.push((
                authorized_path.to_string_lossy().into_owned(),
                current_branch,
                current_head,
                is_detached,
            ));
        }
        let active_worktree = active_worktree
            .canonicalize()
            .unwrap_or_else(|_| active_worktree.to_path_buf());
        let active_worktree = clean_verbatim_path(&active_worktree);
        let active_index = worktrees
            .iter()
            .enumerate()
            .filter(|(_, (path, ..))| active_worktree.starts_with(Path::new(path)))
            .max_by_key(|(_, (path, ..))| Path::new(path).components().count())
            .map(|(index, _)| index);
        json!({
            "worktrees": worktrees.into_iter().enumerate().map(|(index, (path, branch, head, detached))| json!({
                "path": path,
                "branch": if detached { "HEAD" } else { &branch },
                "head": head,
                "detached": detached,
                "active": active_index == Some(index),
            })).collect::<Vec<_>>(),
        })
    }

    async fn git_worktree(
        &self,
        args: serde_json::Value,
        working_dir: &Path,
    ) -> anyhow::Result<ToolResult> {
        let subcommand = args
            .get("subcommand")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                anyhow::Error::msg("Missing 'subcommand' parameter. Use: list, add, remove, prune")
            })?;

        match subcommand {
            "list" => {
                let output = self
                    .run_git_read_command(
                        &["--no-optional-locks", "worktree", "list", "--porcelain"],
                        working_dir,
                    )
                    .await?;
                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(
                        &self.parse_worktree_list(&output, working_dir),
                    )
                    .unwrap_or_default()
                    .into(),
                    error: None,
                })
            }
            "add" => {
                let raw_path = args
                    .get("worktree_path")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| {
                        anyhow::Error::msg("Missing 'worktree_path' parameter for worktree add")
                    })?;
                self.sanitize_git_args(raw_path)?;
                let worktree_path = self.ensure_worktree_add_target_allowed(raw_path)?;
                let git_worktree_path = Self::git_worktree_path(&worktree_path);
                let git_worktree_path = git_worktree_path.to_str().ok_or_else(|| {
                    anyhow::Error::msg("Worktree path must be valid UTF-8 for git execution")
                })?;
                let branch = args
                    .get("branch")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let mut git_args = vec!["worktree", "add", "--", git_worktree_path];
                if !branch.is_empty() {
                    self.sanitize_git_args(branch)?;
                    git_args.push(branch);
                }
                self.run_git_command(&git_args, working_dir).await?;
                Ok(ToolResult {
                    success: true,
                    output: format!("Worktree added at: {git_worktree_path}").into(),
                    error: None,
                })
            }
            "remove" => {
                let raw_path = args
                    .get("worktree_path")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| {
                        anyhow::Error::msg("Missing 'worktree_path' parameter for worktree remove")
                    })?;
                self.sanitize_git_args(raw_path)?;
                let worktree_path = self.ensure_worktree_remove_target_allowed(raw_path)?;
                let git_worktree_path = Self::git_worktree_path(&worktree_path);
                let git_worktree_path = git_worktree_path.to_str().ok_or_else(|| {
                    anyhow::Error::msg("Worktree path must be valid UTF-8 for git execution")
                })?;
                self.run_git_command(&["worktree", "remove", git_worktree_path], working_dir)
                    .await?;
                Ok(ToolResult {
                    success: true,
                    output: format!("Worktree removed: {git_worktree_path}").into(),
                    error: None,
                })
            }
            "prune" => {
                self.run_git_command(&["worktree", "prune"], working_dir)
                    .await?;
                Ok(ToolResult {
                    success: true,
                    output: "Worktree prune completed".into(),
                    error: None,
                })
            }
            _ => anyhow::bail!(
                "Unknown worktree subcommand: {subcommand}. Use: list, add, remove, prune"
            ),
        }
    }
}

#[async_trait]
impl Tool for GitOperationsTool {
    fn name(&self) -> &str {
        "git_operations"
    }

    fn description(&self) -> &str {
        "Perform structured Git operations (status, diff, log, branch, commit, add, checkout, stash, worktree). Provides parsed JSON output and integrates with security policy for autonomy controls."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "branch", "commit", "add", "checkout", "stash", "worktree"],
                    "description": "Git operation to perform"
                },
                "subcommand": {
                    "type": "string",
                    "enum": ["list", "add", "remove", "prune"],
                    "description": "Worktree subcommand"
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (for 'commit' operation); stash message (for 'stash push', defaults to 'auto-stash')"
                },
                "paths": {
                    "type": "string",
                    "description": "Space-separated file paths. For 'add', files to stage. For 'stash push', pathspecs to scope the stash to — without this, the entire working tree is stashed."
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name for the 'checkout' operation or 'worktree add' subcommand"
                },
                "worktree_path": {
                    "type": "string",
                    "description": "Filesystem path for the worktree (for 'worktree add' and 'worktree remove' subcommands). Relative paths resolve under the workspace; absolute paths must stay inside the workspace or configured allowed roots."
                },
                "files": {
                    "type": "string",
                    "description": "File or path to diff (for 'diff' operation, default: '.')"
                },
                "cached": {
                    "type": "boolean",
                    "description": "Show staged changes (for 'diff' operation)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of log entries (for 'log' operation, default: 10)"
                },
                "action": {
                    "type": "string",
                    "enum": ["push", "pop", "list", "drop"],
                    "description": "Stash action (for 'stash' operation)"
                },
                "index": {
                    "type": "integer",
                    "description": "Stash index (for 'stash' with 'drop' action)"
                },
                "keep_index": {
                    "type": "boolean",
                    "description": "For 'stash push': preserve staged changes in the working tree after stashing — only unstaged changes go into the stash."
                },
                "include_untracked": {
                    "type": "boolean",
                    "description": "For 'stash push': also stash untracked files (-u). Without this, `git stash push` only touches tracked files."
                },
                "path": {
                    "type": "string",
                    "description": "Optional repository path authorized by the agent's workspace policy. Defaults to workspace root."
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let operation = match args.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'operation' parameter".into()),
                });
            }
        };

        let requires_write_access = self.requires_write_access(operation, &args);
        let path = args.get("path").and_then(|v| v.as_str());
        let working_dir = match self.resolve_working_dir(path, requires_write_access) {
            Ok(d) => d,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };

        // Repository discovery must not escape the root that authorized this path.
        let authorized_roots = if requires_write_access {
            self.security.approved_write_roots(&working_dir)
        } else {
            self.security.approved_read_roots(&working_dir)
        };
        let repository_authorization = self.has_repository_within_authorized_roots(
            &working_dir,
            &authorized_roots,
            requires_write_access,
        );
        let error_key = match repository_authorization {
            RepositoryAuthorization::Authorized(_) => None,
            RepositoryAuthorization::NotFound => Some("tool-git-operations-error-not-in-repo"),
            // Do not inspect beyond the authorization boundary to learn whether
            // a parent repository exists. The caller must choose a repository
            // whose metadata is reachable within the applicable grant.
            RepositoryAuthorization::DiscoveryBoundaryReached => {
                Some("tool-git-operations-error-repository-outside-authorized-roots")
            }
            RepositoryAuthorization::Denied => {
                let path_display = working_dir.display().to_string();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "path": path_display,
                            "operation": operation,
                            "requires_write_access": requires_write_access,
                        })),
                    "git_operations: repository metadata is not authorized"
                );
                Some("tool-git-operations-error-repository-not-authorized")
            }
        };
        if let Some(error_key) = error_key {
            let path_display = working_dir.display().to_string();
            let error_msg = crate::i18n::get_required_tool_string_with_args(
                error_key,
                &[("path", &path_display)],
            );
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error_msg),
            });
        }

        // Check autonomy level for write operations
        if requires_write_access {
            if !self.security.can_act() {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(
                        "Action blocked: git write operations require higher autonomy level".into(),
                    ),
                });
            }

            match self.security.autonomy {
                AutonomyLevel::ReadOnly => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some("Action blocked: read-only mode".into()),
                    });
                }
                AutonomyLevel::Supervised | AutonomyLevel::Full => {}
            }
        }

        // Record action for rate limiting
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        // Execute the requested operation
        match operation {
            "status" => self.git_status(args, &working_dir).await,
            "diff" => self.git_diff(args, &working_dir).await,
            "log" => self.git_log(args, &working_dir).await,
            "branch" => self.git_branch(args, &working_dir).await,
            "commit" => self.git_commit(args, &working_dir).await,
            "add" => self.git_add(args, &working_dir).await,
            "checkout" => self.git_checkout(args, &working_dir).await,
            "stash" => self.git_stash(args, &working_dir).await,
            "worktree" => self.git_worktree(args, &working_dir).await,
            _ => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Unknown operation: {operation}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;
    use zeroclaw_config::policy::SecurityPolicy;

    #[cfg(unix)]
    struct MarkerGitCommandBoundary {
        marker: PathBuf,
    }

    #[cfg(unix)]
    impl GitCommandBoundary for MarkerGitCommandBoundary {
        fn wrap_command(&self, command: &mut std::process::Command) -> anyhow::Result<()> {
            let program = command.get_program().to_os_string();
            let args = command
                .get_args()
                .map(std::ffi::OsStr::to_os_string)
                .collect::<Vec<_>>();
            let mut replacement = std::process::Command::new(program);
            replacement
                .args(args)
                .env("ZEROCLAW_GIT_BOUNDARY_MARKER", &self.marker)
                .env("GIT_CONFIG_GLOBAL", "/must-not-survive-boundary");
            *command = replacement;
            Ok(())
        }
    }

    #[cfg(unix)]
    struct RejectingGitCommandBoundary;

    #[cfg(unix)]
    impl GitCommandBoundary for RejectingGitCommandBoundary {
        fn wrap_command(&self, _command: &mut std::process::Command) -> anyhow::Result<()> {
            anyhow::bail!("test boundary rejection")
        }
    }

    fn test_tool(dir: &std::path::Path) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: dir.to_path_buf(),
            ..SecurityPolicy::default()
        });
        test_tool_with_security(security)
    }

    fn test_tool_with_security(security: Arc<SecurityPolicy>) -> GitOperationsTool {
        GitOperationsTool::new_with_command_boundary(security, Arc::new(DirectGitCommandBoundary))
    }

    /// Initialise a git repo for tests with commit/tag signing disabled and a
    /// fixed identity. Tests run real `git commit`; without this they inherit
    /// the developer's global `commit.gpgsign`, blocking the suite on a
    /// hardware-key tap.
    fn git_init_no_sign(dir: &std::path::Path, extra_init: &[&str]) {
        let mut init = vec!["init"];
        init.extend_from_slice(extra_init);
        for args in [
            init.as_slice(),
            &["config", "user.email", "test@test.com"],
            &["config", "user.name", "Test"],
            &["config", "commit.gpgsign", "false"],
            &["config", "tag.gpgsign", "false"],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
        }
    }

    fn git_config_set_path(dir: &std::path::Path, key: &str, value: &std::path::Path) {
        let output = std::process::Command::new("git")
            .args(["config", key])
            .arg(value)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "test setup must configure {key}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn test_tool_with_allowed_root(
        dir: &std::path::Path,
        allowed_root: std::path::PathBuf,
    ) -> GitOperationsTool {
        test_tool_with_allowed_roots(dir, vec![allowed_root])
    }

    fn test_tool_with_allowed_roots(
        dir: &std::path::Path,
        allowed_roots: Vec<std::path::PathBuf>,
    ) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: dir.to_path_buf(),
            allowed_roots,
            ..SecurityPolicy::default()
        });
        test_tool_with_security(security)
    }

    fn test_tool_with_read_only_root(
        dir: &std::path::Path,
        read_only_root: std::path::PathBuf,
    ) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: dir.to_path_buf(),
            allowed_roots_read_only: vec![read_only_root],
            ..SecurityPolicy::default()
        });
        test_tool_with_security(security)
    }

    fn test_tool_with_allowed_and_read_only_roots(
        dir: &std::path::Path,
        allowed_root: std::path::PathBuf,
        read_only_root: std::path::PathBuf,
    ) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: dir.to_path_buf(),
            allowed_roots: vec![allowed_root],
            allowed_roots_read_only: vec![read_only_root],
            ..SecurityPolicy::default()
        });
        test_tool_with_security(security)
    }

    fn test_tool_with_write_only_root(
        dir: &std::path::Path,
        write_only_root: std::path::PathBuf,
    ) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: dir.to_path_buf(),
            allowed_roots_write_only: vec![write_only_root],
            ..SecurityPolicy::default()
        });
        test_tool_with_security(security)
    }

    #[test]
    fn sanitize_git_blocks_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // Should block dangerous arguments
        assert!(tool.sanitize_git_args("--exec=rm -rf /").is_err());
        assert!(tool.sanitize_git_args("$(echo pwned)").is_err());
        assert!(tool.sanitize_git_args("`malicious`").is_err());
        assert!(tool.sanitize_git_args("arg | cat").is_err());
        assert!(tool.sanitize_git_args("arg; rm file").is_err());
    }

    #[test]
    fn sanitize_git_blocks_pager_editor_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.sanitize_git_args("--pager=less").is_err());
        assert!(tool.sanitize_git_args("--editor=vim").is_err());
    }

    #[test]
    fn sanitize_git_blocks_config_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // Exact `-c` flag (config injection)
        assert!(tool.sanitize_git_args("-c core.sshCommand=evil").is_err());
        assert!(tool.sanitize_git_args("-c=core.pager=less").is_err());
    }

    #[test]
    fn worktree_targets_reject_paths_outside_authorized_roots() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let tool = test_tool(workspace.path());

        assert!(
            tool.ensure_worktree_add_target_allowed(
                outside.path().join("new-worktree").to_str().unwrap()
            )
            .is_err()
        );

        let existing = outside.path().join("old-worktree");
        std::fs::create_dir(&existing).unwrap();
        assert!(
            tool.ensure_worktree_remove_target_allowed(existing.to_str().unwrap())
                .is_err()
        );
    }

    #[test]
    fn git_commands_clear_ambient_repository_overrides() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());
        let mut command = std::process::Command::new("git");
        command
            .env("GIT_DIR", "/outside/repository")
            .env("GIT_COMMON_DIR", "/outside/common")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_EXEC_PATH", "/outside/git-exec")
            .env("GIT_CONFIG", "/outside/git-config")
            .env("git_dir", "/outside/case-variant-repository");

        let resolved_tmp = tmp.path().canonicalize().unwrap();
        tool.configure_git_environment(&mut command, &resolved_tmp, false)
            .unwrap();

        for name in [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_CONFIG_COUNT",
            "GIT_EXEC_PATH",
            "GIT_CONFIG",
            "git_dir",
        ] {
            assert!(
                !command.get_envs().any(|(key, _)| key == name),
                "{name} must not reach Git"
            );
        }
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "GIT_TERMINAL_PROMPT" && value == Some("0".as_ref())),
            "Git must remain non-interactive"
        );
    }

    #[test]
    fn git_base_environment_preserves_system_config() {
        let mut command = std::process::Command::new("git");
        GitOperationsTool::configure_git_base_environment(&mut command);

        assert!(
            !command
                .get_envs()
                .any(|(name, _)| name == std::ffi::OsStr::new("GIT_CONFIG_NOSYSTEM")),
            "ordinary Git commands must retain system configuration"
        );
    }

    #[test]
    fn git_read_commands_disable_mailmap_with_raw_author_placeholders() {
        assert!(
            READ_GIT_CONFIG_OVERRIDES
                .windows(2)
                .any(|args| args == ["-c", "log.mailmap=false"]),
            "read commands must disable Git mailmap initialization"
        );
        assert!(
            GIT_LOG_FORMAT.contains("%an") && GIT_LOG_FORMAT.contains("%ae"),
            "git log must render raw author identity fields"
        );
        assert!(
            !["%aN", "%aE", "%aL"]
                .iter()
                .any(|placeholder| GIT_LOG_FORMAT.contains(placeholder)),
            "git log must not render mailmap-aware author identity fields"
        );
    }

    #[tokio::test]
    async fn git_operations_bind_core_worktree_to_authorized_repository() {
        let repository = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &["tracked.txt"]).await;
        std::fs::write(repository.path().join("tracked.txt"), "authorized change").unwrap();
        std::fs::write(outside.path().join("outside.txt"), "outside change").unwrap();

        let configured = std::process::Command::new("git")
            .args([
                "config",
                "core.worktree",
                outside.path().to_str().expect("temporary path is UTF-8"),
            ])
            .current_dir(repository.path())
            .status()
            .unwrap();
        assert!(configured.success(), "failed to configure core.worktree");

        let tool = test_tool(repository.path());
        let status = tool.execute(json!({"operation": "status"})).await.unwrap();
        assert!(status.success, "status failed: {status:?}");
        assert!(
            status.output.to_string().contains("\"clean\": false"),
            "status must observe the changed authorized repository: {status:?}"
        );
        assert!(
            !status.output.to_string().contains("outside.txt"),
            "status must not expose the configured outside worktree: {status:?}"
        );

        let diff = tool.execute(json!({"operation": "diff"})).await.unwrap();
        assert!(diff.success, "diff failed: {diff:?}");
        assert!(
            diff.output.to_string().contains("authorized change"),
            "diff must remain bound to the authorized repository: {diff:?}"
        );
        assert!(
            !diff.output.to_string().contains("outside change"),
            "diff must not expose the configured outside worktree: {diff:?}"
        );

        let added = tool
            .execute(json!({"operation": "add", "paths": "tracked.txt"}))
            .await
            .unwrap();
        assert!(added.success, "add failed: {added:?}");
        let staged = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(repository.path())
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&staged.stdout).contains("tracked.txt"),
            "add must stage only in the authorized repository"
        );
        assert!(
            std::fs::read_to_string(outside.path().join("outside.txt")).unwrap()
                == "outside change",
            "outside working tree contents must remain unchanged"
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_worktree_argument_preserves_non_unicode_path_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let repository = PathBuf::from(std::ffi::OsString::from_vec(
            b"/tmp/repository-\xff".to_vec(),
        ));
        let git_dir = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/git-dir-\xff".to_vec()));
        let mut command = std::process::Command::new("git");
        GitOperationsTool::bind_git_worktree(&mut command, &repository, &git_dir);
        let args = command.get_args().collect::<Vec<_>>();

        assert_eq!(args[0], "--git-dir");
        assert_eq!(args[1].as_bytes(), git_dir.as_os_str().as_bytes());
        assert_eq!(args[2], "--work-tree");
        assert_eq!(args[3].as_bytes(), repository.as_os_str().as_bytes());
    }

    #[cfg(windows)]
    #[test]
    fn git_worktree_arguments_clean_windows_verbatim_prefixes() {
        let repository = PathBuf::from(r"\\?\C:\repository");
        let git_dir = PathBuf::from(r"\\?\C:\repository\.git");
        let mut command = std::process::Command::new("git");
        GitOperationsTool::bind_git_worktree(&mut command, &repository, &git_dir);
        let args = command.get_args().collect::<Vec<_>>();

        assert_eq!(args[1], r"C:\repository\.git");
        assert_eq!(args[3], r"C:\repository");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn git_operations_status_accepts_a_canonical_windows_working_directory() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let canonical = repository.path().canonicalize().unwrap();
        assert!(
            canonical.to_string_lossy().starts_with(r"\\?\"),
            "Windows canonical paths must exercise the verbatim-prefix path"
        );

        let status = test_tool(repository.path())
            .execute(json!({"operation": "status", "path": &canonical}))
            .await
            .unwrap();
        assert!(
            status.success,
            "status must start Git from a cleaned canonical directory: {status:?}"
        );
    }

    #[test]
    fn sanitize_git_blocks_no_verify() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.sanitize_git_args("--no-verify").is_err());
    }

    #[test]
    fn sanitize_git_blocks_redirect_in_args() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.sanitize_git_args("file.txt > /tmp/out").is_err());
    }

    #[test]
    fn sanitize_git_cached_not_blocked() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // --cached must NOT be blocked by the `-c` check
        assert!(tool.sanitize_git_args("--cached").is_ok());
        // Other safe flags starting with -c prefix
        assert!(tool.sanitize_git_args("-cached").is_ok());
    }

    #[test]
    fn sanitize_git_allows_safe() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // Should allow safe arguments
        assert!(tool.sanitize_git_args("main").is_ok());
        assert!(tool.sanitize_git_args("feature/test-branch").is_ok());
        assert!(tool.sanitize_git_args("--cached").is_ok());
        assert!(tool.sanitize_git_args("src/main.rs").is_ok());
        assert!(tool.sanitize_git_args(".").is_ok());
    }

    #[test]
    fn requires_write_detection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.requires_write_access("commit", &json!({})));
        assert!(tool.requires_write_access("add", &json!({})));
        assert!(tool.requires_write_access("checkout", &json!({})));
        assert!(tool.requires_write_access("stash", &json!({})));
        assert!(tool.requires_write_access("stash", &json!({"action": "push"})));
        assert!(tool.requires_write_access("worktree", &json!({"subcommand": "add"})));
        assert!(tool.requires_write_access("worktree", &json!({"subcommand": "remove"})));
        assert!(tool.requires_write_access("worktree", &json!({"subcommand": "prune"})));

        assert!(!tool.requires_write_access("status", &json!({})));
        assert!(!tool.requires_write_access("diff", &json!({})));
        assert!(!tool.requires_write_access("log", &json!({})));
        assert!(!tool.requires_write_access("branch", &json!({})));
        assert!(!tool.requires_write_access("stash", &json!({"action": "list"})));
        assert!(!tool.requires_write_access("worktree", &json!({"subcommand": "list"})));
        assert!(!tool.requires_write_access("status", &json!({"subcommand": "add"})));
    }

    #[tokio::test]
    async fn git_operations_preserve_authorized_linked_worktree_lifecycle() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[]).await;
        let tool = test_tool(tmp.path());

        let schema = tool.parameters_schema();
        assert!(
            schema["properties"]["operation"]["enum"]
                .as_array()
                .is_some_and(|operations| operations.iter().any(|value| value == "worktree")),
            "the public Git operation schema must advertise worktree"
        );

        let linked_worktree = tmp.path().join("linked-worktree");
        let created_branch = std::process::Command::new("git")
            .args(["branch", "linked-worktree-branch"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(
            created_branch.success(),
            "test setup must create an unused worktree branch"
        );
        let added = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "add",
                "worktree_path": &linked_worktree,
                "branch": "linked-worktree-branch",
            }))
            .await
            .unwrap();
        assert!(added.success, "worktree add failed: {added:?}");
        assert!(linked_worktree.join(".git").is_file());

        let option_like_path = tmp.path().join("option-like-worktree");
        let option_like_branch = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "add",
                "worktree_path": &option_like_path,
                "branch": "--detach",
            }))
            .await;
        assert!(
            option_like_branch.is_err(),
            "an option-like branch must not be interpreted as a worktree option: {option_like_branch:?}"
        );
        assert!(
            !option_like_path.exists(),
            "an option-like branch must not create a worktree"
        );

        let listed = tool
            .execute(json!({"operation": "worktree", "subcommand": "list"}))
            .await
            .unwrap();
        assert!(listed.success, "worktree list failed: {listed:?}");
        let worktrees: serde_json::Value = serde_json::from_str(&listed.output).unwrap();
        let linked_worktree_path = clean_verbatim_path(&linked_worktree.canonicalize().unwrap())
            .to_string_lossy()
            .into_owned();
        assert!(
            worktrees["worktrees"]
                .as_array()
                .is_some_and(|entries| entries
                    .iter()
                    .any(|entry| entry["path"] == linked_worktree_path)),
            "worktree list must include the authorized linked worktree: {listed:?}"
        );

        let status = tool
            .execute(json!({"operation": "status", "path": &linked_worktree}))
            .await
            .unwrap();
        assert!(status.success, "linked worktree status failed: {status:?}");

        let pruned = tool
            .execute(json!({"operation": "worktree", "subcommand": "prune"}))
            .await
            .unwrap();
        assert!(pruned.success, "worktree prune failed: {pruned:?}");

        let removed = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "remove",
                "worktree_path": &linked_worktree,
            }))
            .await
            .unwrap();
        assert!(removed.success, "worktree remove failed: {removed:?}");
        assert!(
            !linked_worktree.exists(),
            "worktree remove must remove the linked worktree"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_write_operations_run_repository_hooks_inside_the_injected_boundary() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let marker = repository.path().join("boundary-marker");
        let hook = repository.path().join(".git/hooks/post-checkout");
        std::fs::write(
            &hook,
            "#!/bin/sh\nprintf '%s|%s|%s|%s' \"$GIT_TERMINAL_PROMPT\" \"$GIT_PAGER\" \"${GIT_CONFIG_GLOBAL-unset}\" \"$PWD\" > \"$ZEROCLAW_GIT_BOUNDARY_MARKER\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();
        let branch = "boundary-worktree";
        let status = std::process::Command::new("git")
            .args(["branch", branch])
            .current_dir(repository.path())
            .status()
            .unwrap();
        assert!(status.success(), "test setup must create a worktree branch");

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: repository.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new_with_command_boundary(
            security,
            Arc::new(MarkerGitCommandBoundary {
                marker: marker.clone(),
            }),
        );
        let worktree = repository.path().join("boundary-worktree");
        let result = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "add",
                "worktree_path": &worktree,
                "branch": branch,
            }))
            .await
            .unwrap();

        assert!(result.success, "worktree add failed: {result:?}");
        let canonical_worktree = worktree.canonicalize().unwrap();
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!("0|cat|unset|{}", canonical_worktree.display(),),
            "a replacing boundary must preserve the Git environment, cwd, and its non-Git marker"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_write_operations_fail_closed_when_the_command_boundary_rejects_execution() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let branch = "rejected-boundary-worktree";
        let status = std::process::Command::new("git")
            .args(["branch", branch])
            .current_dir(repository.path())
            .status()
            .unwrap();
        assert!(status.success(), "test setup must create a worktree branch");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: repository.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new_with_command_boundary(
            security,
            Arc::new(RejectingGitCommandBoundary),
        );
        let worktree = repository.path().join("rejected-boundary-worktree");
        let error = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "add",
                "worktree_path": &worktree,
                "branch": branch,
            }))
            .await
            .unwrap_err();

        assert!(
            format!("{error}").contains("test boundary rejection"),
            "the boundary rejection must remain actionable in normal user-facing rendering: {error}"
        );
        assert!(
            !worktree.exists(),
            "the rejected boundary must not create a worktree"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_write_operations_require_an_injected_execution_boundary() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let branch = "unconfigured-boundary-worktree";
        let status = std::process::Command::new("git")
            .args(["branch", branch])
            .current_dir(repository.path())
            .status()
            .unwrap();
        assert!(status.success(), "test setup must create a worktree branch");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: repository.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security);
        let worktree = repository.path().join("unconfigured-boundary-worktree");
        let error = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "add",
                "worktree_path": &worktree,
                "branch": branch,
            }))
            .await
            .unwrap_err();

        assert!(
            format!("{error:#}").contains("require a configured execution boundary"),
            "an unconfigured constructor must fail closed: {error:#}"
        );
        assert!(
            !worktree.exists(),
            "an unconfigured constructor must not create a worktree"
        );
    }

    #[tokio::test]
    async fn git_operations_preserve_linked_worktree_lifecycle_across_authorized_roots() {
        let workspace = TempDir::new().unwrap();
        let allowed_root = TempDir::new().unwrap();
        bootstrap_repo(workspace.path(), &[]).await;
        let tool = test_tool_with_allowed_root(workspace.path(), allowed_root.path().to_path_buf());
        let linked_worktree = allowed_root.path().join("linked-worktree");

        let added = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "add",
                "worktree_path": &linked_worktree,
            }))
            .await
            .unwrap();
        assert!(added.success, "worktree add failed: {added:?}");

        let status = tool
            .execute(json!({"operation": "status", "path": &linked_worktree}))
            .await
            .unwrap();
        assert!(
            status.success,
            "linked worktree status across authorized roots failed: {status:?}"
        );

        let removed = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "remove",
                "worktree_path": &linked_worktree,
            }))
            .await
            .unwrap();
        assert!(removed.success, "worktree remove failed: {removed:?}");
        assert!(
            !linked_worktree.exists(),
            "worktree remove must remove the separately authorized linked worktree"
        );
    }

    #[tokio::test]
    async fn git_worktree_list_omits_sibling_worktrees_outside_the_read_grant() {
        let workspace = TempDir::new().unwrap();
        let sibling_parent = TempDir::new().unwrap();
        let allowed_root = TempDir::new().unwrap();
        bootstrap_repo(workspace.path(), &[]).await;
        let sibling = sibling_parent.path().join("sibling-worktree");
        let added = std::process::Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&sibling)
            .current_dir(workspace.path())
            .status()
            .unwrap();
        assert!(added.success(), "test setup must create sibling worktree");

        let authorized = allowed_root.path().join("authorized-worktree");
        let added = std::process::Command::new("git")
            .args(["branch", "authorized-worktree-branch"])
            .current_dir(workspace.path())
            .status()
            .unwrap();
        assert!(added.success(), "test setup must create an unused branch");
        let added = std::process::Command::new("git")
            .args(["worktree", "add"])
            .arg(&authorized)
            .arg("authorized-worktree-branch")
            .current_dir(workspace.path())
            .status()
            .unwrap();
        assert!(
            added.success(),
            "test setup must create authorized worktree"
        );

        let tool = test_tool_with_allowed_root(workspace.path(), allowed_root.path().to_path_buf());
        let porcelain = format!(
            "worktree {}\nHEAD workspace-head\nbranch refs/heads/master\n\nworktree {}\nHEAD sibling-head\ndetached\n\nworktree {}\nHEAD authorized-head\nbranch refs/heads/authorized-worktree-branch\n\n",
            workspace.path().display(),
            sibling.display(),
            authorized.display(),
        );
        let listed = tool.parse_worktree_list(&porcelain, workspace.path());
        let worktrees = listed["worktrees"].as_array().unwrap();
        let workspace_path = clean_verbatim_path(&workspace.path().canonicalize().unwrap());
        let sibling_path = clean_verbatim_path(&sibling.canonicalize().unwrap());
        let authorized_path = clean_verbatim_path(&authorized.canonicalize().unwrap());
        assert_eq!(
            worktrees.len(),
            2,
            "only authorized worktrees must be listed: {listed}"
        );
        assert_eq!(
            worktrees
                .iter()
                .filter(|entry| entry["active"] == true)
                .count(),
            1,
            "exactly one listed worktree must be active: {listed}"
        );
        assert!(
            worktrees
                .iter()
                .any(|entry| entry["path"] == workspace_path.to_string_lossy().as_ref()),
            "the authorized worktree must remain visible: {listed}"
        );
        let workspace_entry = worktrees
            .iter()
            .find(|entry| entry["path"] == workspace_path.to_string_lossy().as_ref())
            .expect("the workspace worktree must remain visible");
        assert_eq!(workspace_entry["active"], true);
        assert!(
            !worktrees
                .iter()
                .any(|entry| entry["path"] == sibling_path.to_string_lossy().as_ref()),
            "an unauthorized sibling worktree path must not be disclosed: {listed}"
        );
        let authorized_entry = worktrees
            .iter()
            .find(|entry| entry["path"] == authorized_path.to_string_lossy().as_ref())
            .expect("the separately authorized worktree must remain visible");
        assert_eq!(authorized_entry["branch"], "authorized-worktree-branch");
        assert_eq!(authorized_entry["head"], "authorized-head");
        assert_eq!(authorized_entry["detached"], false);
        assert_eq!(authorized_entry["active"], false);
    }

    #[tokio::test]
    async fn git_worktree_list_reads_from_configured_read_only_root() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        bootstrap_repo(read_only_root.path(), &[]).await;
        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());

        let listed = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "list",
                "path": read_only_root.path(),
            }))
            .await
            .unwrap();
        assert!(listed.success, "read-only worktree list failed: {listed:?}");
        let worktrees: serde_json::Value = serde_json::from_str(&listed.output).unwrap();
        let read_only_worktree_path =
            clean_verbatim_path(&read_only_root.path().canonicalize().unwrap())
                .to_string_lossy()
                .into_owned();
        assert!(
            worktrees["worktrees"]
                .as_array()
                .is_some_and(|entries| entries
                    .iter()
                    .any(|entry| entry["path"] == read_only_worktree_path)),
            "the readable worktree must be listed: {listed:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_discover_parent_repository_when_policy_is_unrestricted() {
        let repository = TempDir::new().unwrap();
        let workspace = repository.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            workspace_only: false,
            forbidden_paths: Vec::new(),
            ..SecurityPolicy::default()
        });
        let tool = test_tool_with_security(security);

        let status = tool.execute(json!({"operation": "status"})).await.unwrap();
        assert!(
            status.success,
            "unrestricted policy must discover its parent repository: {status:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_reject_forbidden_parent_repository_when_policy_is_unrestricted() {
        let repository = TempDir::new().unwrap();
        let workspace = repository.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            workspace_only: false,
            forbidden_paths: vec![repository.path().display().to_string()],
            ..SecurityPolicy::default()
        });
        let tool = test_tool_with_security(security);

        let status = tool.execute(json!({"operation": "status"})).await.unwrap();
        assert!(
            !status.success,
            "unrestricted policy must still reject a forbidden parent repository: {status:?}"
        );
    }

    #[tokio::test]
    async fn git_credential_op_fails_fast_without_terminal_prompt() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);
        let tool = test_tool(tmp.path());

        let fetch = tool.run_git_command(
            &["fetch", "https://127.0.0.1:1/private/repo.git"],
            tmp.path(),
        );
        let res = tokio::time::timeout(std::time::Duration::from_secs(10), fetch).await;

        assert!(
            res.is_ok(),
            "git fetch hung — it likely prompted for credentials on the terminal"
        );
        assert!(
            res.unwrap().is_err(),
            "fetch to an unreachable private remote should fail, not succeed"
        );
    }

    #[tokio::test]
    async fn blocks_readonly_mode_for_write_ops() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = test_tool_with_security(security);

        let result = tool
            .execute(json!({"operation": "commit", "message": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        // can_act() returns false for ReadOnly, so we get the "higher autonomy level" message
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("higher autonomy")
        );
    }

    #[tokio::test]
    async fn allows_branch_listing_in_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = test_tool_with_security(security);

        for args in [json!({"operation": "branch"})] {
            let result = tool.execute(args).await.unwrap();
            assert!(
                result.success,
                "read-only Git operation must execute under ReadOnly autonomy: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn allows_readonly_ops_in_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = test_tool_with_security(security);

        // This will fail because there's no git repo, but it shouldn't be blocked by autonomy
        let result = tool.execute(json!({"operation": "status"})).await.unwrap();
        // The error should be about git (not about autonomy/read-only mode)
        assert!(!result.success, "Expected failure due to missing git repo");
        let error_msg = result.error.as_deref().unwrap_or("");
        assert!(
            !error_msg.contains("read-only") && !error_msg.contains("autonomy"),
            "Error should be about git, not about autonomy restrictions: {error_msg}"
        );
    }

    #[tokio::test]
    async fn rejects_missing_operation() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Missing 'operation'")
        );
    }

    #[tokio::test]
    async fn rejects_unknown_operation() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);

        let tool = test_tool(tmp.path());

        let result = tool.execute(json!({"operation": "push"})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Unknown operation")
        );
    }

    #[tokio::test]
    async fn commit_message_preserves_blank_line_between_subject_and_body() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);
        // Create an initial commit so HEAD exists.
        std::fs::write(tmp.path().join("README.md"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let tool = test_tool(tmp.path());

        let msg = "fix(foo): subject line\n\nThis is the body paragraph.\n\nSecond paragraph.";
        let result = tool
            .execute(json!({"operation": "commit", "message": msg}))
            .await
            .unwrap();
        assert!(result.success, "commit failed: {:?}", result.error);

        // Read back the raw commit message via git log.
        let log_out = std::process::Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log_msg = String::from_utf8_lossy(&log_out.stdout);

        // Subject line must be on its own line.
        assert!(
            log_msg.starts_with("fix(foo): subject line\n"),
            "subject line missing or not first: {log_msg:?}"
        );
        // A blank line must follow the subject.
        assert!(
            log_msg.contains("fix(foo): subject line\n\n"),
            "blank line between subject and body missing: {log_msg:?}"
        );
        // Body text must be present.
        assert!(
            log_msg.contains("This is the body paragraph."),
            "body paragraph missing: {log_msg:?}"
        );
    }

    #[test]
    fn truncates_multibyte_commit_message_without_panicking() {
        let long = "🦀".repeat(2500);
        let truncated = GitOperationsTool::truncate_commit_message(&long);

        assert_eq!(truncated.chars().count(), 2000);
    }

    #[test]
    fn resolve_working_dir_none_returns_workspace() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.resolve_working_dir(None, false).unwrap();
        assert_eq!(result, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn resolve_working_dir_empty_returns_workspace() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.resolve_working_dir(Some(""), false).unwrap();
        assert_eq!(result, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn resolve_working_dir_valid_subdir() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("subproject")).unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.resolve_working_dir(Some("subproject"), false).unwrap();
        let expected = tmp.path().join("subproject").canonicalize().unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn resolve_working_dir_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.resolve_working_dir(Some(".."), false);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Git path '..' is not authorized"),
            "Expected traversal rejection, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn git_operations_work_in_subdirectory() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("nested");
        std::fs::create_dir(&sub).unwrap();
        git_init_no_sign(&sub, &[]);

        let tool = test_tool(tmp.path());

        let result = tool
            .execute(json!({"operation": "status", "path": "nested"}))
            .await
            .unwrap();
        assert!(
            result.success,
            "Expected success, got error: {:?}",
            result.error
        );
        assert!(result.output.contains("branch"));
    }

    #[tokio::test]
    async fn git_operations_work_in_configured_allowed_root() {
        let workspace = TempDir::new().unwrap();
        let allowed_root = TempDir::new().unwrap();
        git_init_no_sign(allowed_root.path(), &[]);
        let tool = test_tool_with_allowed_root(workspace.path(), allowed_root.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "status", "path": allowed_root.path()}))
            .await
            .unwrap();

        assert!(
            result.success,
            "Expected success, got error: {:?}",
            result.error
        );
        assert!(result.output.contains("branch"));
    }

    #[tokio::test]
    async fn git_operations_find_parent_repository_through_allowed_parent_of_workspace() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let workspace = repository.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let tool = test_tool_with_allowed_root(&workspace, repository.path().to_path_buf());

        let result = tool.execute(json!({"operation": "status"})).await.unwrap();

        assert!(
            result.success,
            "an allowed parent repository must remain reachable: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn git_operations_find_parent_repository_regardless_of_overlapping_grant_order() {
        let workspace = TempDir::new().unwrap();
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let child = repository.path().join("authorized-child");
        std::fs::create_dir(&child).unwrap();

        for allowed_roots in [
            vec![child.clone(), repository.path().to_path_buf()],
            vec![repository.path().to_path_buf(), child.clone()],
        ] {
            let tool = test_tool_with_allowed_roots(workspace.path(), allowed_roots);
            let result = tool
                .execute(json!({"operation": "status", "path": &child}))
                .await
                .unwrap();
            assert!(
                result.success,
                "overlapping grants must reach the authorized parent repository: {:?}",
                result.error
            );
        }
    }

    #[tokio::test]
    async fn git_operations_binds_the_validated_git_dir_over_a_nested_bare_layout() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let nested_bare = repository.path().join("nested-bare");
        std::fs::create_dir_all(nested_bare.join("objects/info")).unwrap();
        std::fs::create_dir_all(nested_bare.join("refs/heads")).unwrap();
        std::fs::write(nested_bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status", "path": &nested_bare}))
            .await
            .expect("status dispatch must return a tool result");
        assert!(
            result.success,
            "Git must remain bound to the validated parent repository: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_do_not_mutate_parent_repository_via_read_only_grant() {
        let workspace = TempDir::new().unwrap();
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let writable_child = repository.path().join("writable-child");
        std::fs::create_dir(&writable_child).unwrap();
        std::fs::write(writable_child.join("new-file"), "content").unwrap();
        let index_path = repository.path().join(".git/index");
        let index_before = std::fs::read(&index_path).unwrap();
        let tool = test_tool_with_allowed_and_read_only_roots(
            workspace.path(),
            writable_child.clone(),
            repository.path().to_path_buf(),
        );

        let read = tool
            .execute(json!({"operation": "status", "path": &writable_child}))
            .await
            .unwrap();
        assert!(
            read.success,
            "the read-only parent grant should permit status: {:?}",
            read.error
        );

        let write = tool
            .execute(json!({
                "operation": "add",
                "path": &writable_child,
                "paths": "new-file"
            }))
            .await
            .unwrap();
        assert!(
            !write.success,
            "a read-only parent grant must not authorize mutation: {write:?}"
        );
        assert_eq!(
            std::fs::read(&index_path).unwrap(),
            index_before,
            "rejected mutation must not change the parent repository index"
        );
    }

    #[tokio::test]
    async fn git_operations_reject_reads_from_write_only_root() {
        let workspace = TempDir::new().unwrap();
        let write_only_root = TempDir::new().unwrap();
        git_init_no_sign(write_only_root.path(), &[]);
        let tool =
            test_tool_with_write_only_root(workspace.path(), write_only_root.path().to_path_buf());

        for operation in ["status", "diff", "log", "branch"] {
            let result = tool
                .execute(json!({"operation": operation, "path": write_only_root.path()}))
                .await
                .unwrap();
            assert!(
                !result.success,
                "{operation} must not read from a write-only root"
            );
            assert!(
                result
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("Git path")),
                "{operation} must fail during authorization: {:?}",
                result.error
            );
        }

        let write = tool
            .execute(json!({
                "operation": "add",
                "path": write_only_root.path(),
                "paths": "new-file"
            }))
            .await
            .unwrap();
        assert!(
            !write.success,
            "write-classified Git operations must not access a write-only root"
        );
        assert!(
            write
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Git path")),
            "write must fail during authorization: {:?}",
            write.error
        );
    }

    #[tokio::test]
    async fn git_operations_adds_in_configured_allowed_root() {
        let workspace = TempDir::new().unwrap();
        let allowed_root = TempDir::new().unwrap();
        git_init_no_sign(allowed_root.path(), &[]);
        std::fs::write(allowed_root.path().join("tracked.txt"), "content").unwrap();
        let tool = test_tool_with_allowed_root(workspace.path(), allowed_root.path().to_path_buf());

        let result = tool
            .execute(json!({
                "operation": "add",
                "path": allowed_root.path(),
                "paths": "tracked.txt"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "Expected success, got error: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn git_operations_reject_writes_from_read_only_root() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        git_init_no_sign(read_only_root.path(), &[]);
        std::fs::write(read_only_root.path().join("tracked.txt"), "content").unwrap();
        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());

        let result = tool
            .execute(json!({
                "operation": "add",
                "path": read_only_root.path(),
                "paths": "tracked.txt"
            }))
            .await
            .unwrap();

        assert!(!result.success, "add must not write to a read-only root");
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Git path")),
            "add must fail during authorization: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_parent_repository_above_allowed_root() {
        let workspace = TempDir::new().unwrap();
        let parent_repository = TempDir::new().unwrap();
        git_init_no_sign(parent_repository.path(), &[]);
        let allowed_child = parent_repository.path().join("allowed-child");
        std::fs::create_dir(&allowed_child).unwrap();
        std::fs::write(allowed_child.join("tracked.txt"), "content").unwrap();
        let tool = test_tool_with_allowed_root(workspace.path(), allowed_child.clone());
        let resolved_child = allowed_child.canonicalize().unwrap();

        for (args, requires_write_access) in [
            (
                json!({"operation": "status", "path": &allowed_child}),
                false,
            ),
            (
                json!({
                    "operation": "add",
                    "path": &allowed_child,
                    "paths": "tracked.txt"
                }),
                true,
            ),
        ] {
            let roots = if requires_write_access {
                tool.security.approved_write_roots(&resolved_child)
            } else {
                tool.security.approved_read_roots(&resolved_child)
            };
            assert_eq!(
                tool.has_repository_within_authorized_roots(
                    &resolved_child,
                    &roots,
                    requires_write_access,
                ),
                RepositoryAuthorization::DiscoveryBoundaryReached,
                "parent discovery must stop at the authorized root"
            );
            let result = tool.execute(args).await.unwrap();
            assert!(
                !result.success,
                "parent repository must not be usable through an allowed child: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn git_operations_rejects_parent_repository_above_workspace() {
        let parent_repository = TempDir::new().unwrap();
        git_init_no_sign(parent_repository.path(), &[]);
        let workspace = parent_repository.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let tool = test_tool(&workspace);

        let result = tool.execute(json!({"operation": "status"})).await.unwrap();
        assert!(
            !result.success,
            "parent repository must not escape workspace"
        );
        assert!(
            result.error.as_deref().is_some_and(|error| error
                .contains("No Git repository is reachable within the authorized roots")),
            "the default-policy escape must use the bounded diagnostic: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_invalid_git_directory_before_parent_repository() {
        let workspace = TempDir::new().unwrap();
        let parent_repository = TempDir::new().unwrap();
        git_init_no_sign(parent_repository.path(), &[]);
        let allowed_child = parent_repository.path().join("allowed-child");
        std::fs::create_dir_all(allowed_child.join(".git")).unwrap();
        let tool = test_tool_with_allowed_root(workspace.path(), allowed_child.clone());
        let resolved_child = allowed_child.canonicalize().unwrap();
        let mut command = std::process::Command::new("git");
        tool.configure_git_environment(&mut command, &resolved_child, false)
            .unwrap();
        let expected_ceiling = GitOperationsTool::git_discovery_ceiling_path(
            &parent_repository.path().canonicalize().unwrap(),
        )
        .unwrap();
        assert_eq!(
            command
                .get_envs()
                .find_map(|(key, value)| (key == "GIT_CEILING_DIRECTORIES").then_some(value))
                .flatten(),
            Some(expected_ceiling.as_os_str()),
            "Git must stop before the parent repository"
        );

        let result = tool
            .execute(json!({"operation": "status", "path": &allowed_child}))
            .await;
        assert!(
            matches!(result, Ok(ToolResult { success: false, .. }) | Err(_)),
            "an invalid child .git directory must not fall through to its parent repository: {result:?}"
        );
    }

    #[test]
    fn git_ceiling_cleans_windows_verbatim_prefixes() {
        assert_eq!(
            clean_verbatim_path(Path::new(r"\\?\C:\Users\me\repo")),
            PathBuf::from(r"C:\Users\me\repo")
        );
        assert_eq!(
            clean_verbatim_path(Path::new(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
        assert_eq!(
            clean_verbatim_path(Path::new("/workspace/repo")),
            PathBuf::from("/workspace/repo")
        );
    }

    #[test]
    fn git_worktree_path_cleans_windows_verbatim_prefixes() {
        assert_eq!(
            GitOperationsTool::git_worktree_path(Path::new(r"\\?\C:\Users\me\repo")),
            PathBuf::from(r"C:\Users\me\repo")
        );
        assert_eq!(
            GitOperationsTool::git_worktree_path(Path::new(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
    }

    #[test]
    fn git_subprocess_current_dir_cleans_windows_verbatim_prefixes() {
        assert_eq!(
            GitOperationsTool::git_subprocess_current_dir(Path::new(r"\\?\C:\Users\me\repo")),
            PathBuf::from(r"C:\Users\me\repo")
        );
        assert_eq!(
            GitOperationsTool::git_subprocess_current_dir(Path::new(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_commands_reject_unrepresentable_discovery_ceilings() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("parent:with-colon");
        let allowed_root = parent.join("allowed-root");
        std::fs::create_dir_all(&allowed_root).unwrap();
        let allowed_root = allowed_root.canonicalize().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: allowed_root.clone(),
            workspace_only: true,
            ..SecurityPolicy::default()
        });
        let tool = test_tool_with_security(security);
        let mut command = std::process::Command::new("git");

        let error = tool
            .configure_git_environment(&mut command, &allowed_root, false)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot represent authorized root"),
            "an unrepresentable Git ceiling must fail closed: {error}"
        );
        assert!(
            !command
                .get_envs()
                .any(|(key, value)| key == "GIT_CEILING_DIRECTORIES" && value.is_some()),
            "a failed ceiling must not leave Git discovery unbounded"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_operations_rejects_git_symlink_to_parent_repository() {
        let workspace = TempDir::new().unwrap();
        let parent_repository = TempDir::new().unwrap();
        git_init_no_sign(parent_repository.path(), &[]);
        let allowed_child = parent_repository.path().join("allowed-child");
        std::fs::create_dir(&allowed_child).unwrap();
        std::fs::write(allowed_child.join("tracked.txt"), "content").unwrap();
        std::os::unix::fs::symlink(
            parent_repository.path().join(".git"),
            allowed_child.join(".git"),
        )
        .unwrap();
        let tool = test_tool_with_allowed_root(workspace.path(), allowed_child.clone());

        for args in [
            json!({"operation": "status", "path": &allowed_child}),
            json!({
                "operation": "add",
                "path": &allowed_child,
                "paths": "tracked.txt"
            }),
        ] {
            let result = tool.execute(args).await.unwrap();
            assert!(
                !result.success,
                "Git metadata outside the allowed root must be denied: {result:?}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_operations_rejects_internal_metadata_symlink_before_git_runs() {
        let repository = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &["tracked.txt"]).await;
        std::fs::write(repository.path().join("tracked.txt"), "changed").unwrap();
        let outside_index = outside.path().join("index");
        std::fs::write(&outside_index, "outside marker").unwrap();
        std::fs::remove_file(repository.path().join(".git/index")).unwrap();
        std::os::unix::fs::symlink(&outside_index, repository.path().join(".git/index")).unwrap();

        let tool = test_tool(repository.path());
        let result = tool.execute(json!({"operation": "status"})).await;
        assert!(
            result.is_err(),
            "internal metadata symlink must be rejected: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(outside_index).unwrap(),
            "outside marker"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_repository_config_includes() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let config = repository.path().join(".git/config");
        std::fs::write(
            &config,
            format!(
                "{}\n[include]\n\tpath = /tmp/zeroclaw-test-include\n",
                std::fs::read_to_string(&config).unwrap()
            ),
        )
        .unwrap();

        let tool = test_tool(repository.path());
        let result = tool.execute(json!({"operation": "status"})).await;
        assert!(
            result.is_err(),
            "repository config includes must be rejected: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_out_of_grant_metadata_config_paths() {
        for key in ["attributesFile", "excludesFile"] {
            let repository = TempDir::new().unwrap();
            let outside = TempDir::new().unwrap();
            bootstrap_repo(repository.path(), &[]).await;
            let external_file = outside.path().join("metadata");
            std::fs::write(&external_file, "*.secret\n").unwrap();
            git_config_set_path(repository.path(), &format!("core.{key}"), &external_file);

            let error = test_tool(repository.path())
                .execute(json!({"operation": "status"}))
                .await
                .expect_err(&format!(
                    "out-of-grant core.{key} must be rejected before Git runs"
                ));
            assert!(
                error
                    .chain()
                    .any(|cause| cause.to_string() == "Git metadata config path is not authorized"),
                "core.{key} must reach metadata authorization, not fail while parsing config: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn git_operations_accepts_in_grant_metadata_config_paths() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let attributes = repository.path().join("metadata-attributes");
        let excludes = repository.path().join("metadata-excludes");
        std::fs::write(&attributes, "*.generated -text\n").unwrap();
        std::fs::write(
            &excludes,
            "metadata-attributes\nmetadata-excludes\nignored-by-config\n",
        )
        .unwrap();
        std::fs::write(repository.path().join("ignored-by-config"), "ignored\n").unwrap();
        git_config_set_path(repository.path(), "core.attributesFile", &attributes);
        git_config_set_path(repository.path(), "core.excludesFile", &excludes);

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status"}))
            .await
            .expect("in-grant metadata configuration must dispatch");
        assert!(
            result.success,
            "in-grant metadata configuration failed: {result:?}"
        );
        let output: serde_json::Value = serde_json::from_str(&result.output.to_string()).unwrap();
        assert_eq!(
            output["untracked"],
            json!([]),
            "Git must use the authorized absolute excludes path"
        );
    }

    #[tokio::test]
    async fn git_operations_preserves_relative_metadata_config_paths() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let nested = repository.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(
            repository.path().join("metadata-excludes"),
            "ignored-from-config\n",
        )
        .unwrap();
        let stage = std::process::Command::new("git")
            .args(["add", "metadata-excludes"])
            .current_dir(repository.path())
            .status()
            .unwrap();
        assert!(stage.success(), "test setup must stage the metadata source");
        let commit = std::process::Command::new("git")
            .args(["commit", "-m", "metadata source"])
            .current_dir(repository.path())
            .status()
            .unwrap();
        assert!(
            commit.success(),
            "test setup must commit the metadata source"
        );
        std::fs::write(nested.join("ignored-from-config"), "ignored\n").unwrap();
        let config = repository.path().join(".git/config");
        std::fs::write(
            &config,
            format!(
                "{}\n[core]\n\texcludesFile = metadata-excludes\n",
                std::fs::read_to_string(&config).unwrap(),
            ),
        )
        .unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status", "path": &nested}))
            .await
            .expect("relative metadata configuration must dispatch");
        assert!(
            result.success,
            "relative metadata configuration failed: {result:?}"
        );
        let output: serde_json::Value = serde_json::from_str(&result.output.to_string()).unwrap();
        assert_eq!(
            output["untracked"],
            json!([]),
            "Git must use the authorized relative excludes path from the repository root"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_operations_rejects_symlinked_metadata_config_paths() {
        let repository = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let external_file = outside.path().join("attributes");
        std::fs::write(&external_file, "*.secret\n").unwrap();
        let configured_file = repository.path().join("metadata-attributes");
        std::os::unix::fs::symlink(&external_file, &configured_file).unwrap();
        let config = repository.path().join(".git/config");
        std::fs::write(
            &config,
            format!(
                "{}\n[core]\n\tattributesFile = {}\n",
                std::fs::read_to_string(&config).unwrap(),
                configured_file.display(),
            ),
        )
        .unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status"}))
            .await;
        assert!(
            result.is_err(),
            "symlinked metadata configuration must be rejected before Git runs: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_module_config_includes() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let module_config = repository.path().join(".git/modules/sub/config");
        std::fs::create_dir_all(module_config.parent().unwrap()).unwrap();
        std::fs::write(
            &module_config,
            "[include]\n\tpath = /tmp/zeroclaw-test-include\n",
        )
        .unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status"}))
            .await;
        assert!(
            result.is_err(),
            "module config includes must be rejected: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_module_write_hooks_path() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let module_config = repository.path().join(".git/modules/sub/config");
        std::fs::create_dir_all(module_config.parent().unwrap()).unwrap();
        std::fs::write(&module_config, "[core]\n\thooksPath = hooks\n").unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "commit", "message": "test"}))
            .await
            .expect("commit dispatch must return a tool result");
        assert!(
            !result.success,
            "module hooks path must reject write command: {result:?}"
        );
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Git repository metadata at")),
            "metadata validation must use the localized tool error: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_out_of_grant_object_alternates() {
        let repository = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let outside_objects = outside.path().join("objects");
        std::fs::create_dir(&outside_objects).unwrap();
        std::fs::write(
            repository.path().join(".git/objects/info/alternates"),
            format!("{}\n", outside_objects.display()),
        )
        .unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status"}))
            .await;
        assert!(
            result.is_err(),
            "out-of-grant object alternates must be rejected: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_operations_rejects_in_grant_symlinked_object_alternate() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let alternate_objects = repository.path().join("alternate-objects");
        std::fs::create_dir(&alternate_objects).unwrap();
        let symlinked_alternate = repository.path().join("alternate-link");
        std::os::unix::fs::symlink(&alternate_objects, &symlinked_alternate).unwrap();
        std::fs::write(
            repository.path().join(".git/objects/info/alternates"),
            format!("{}\n", symlinked_alternate.display()),
        )
        .unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status"}))
            .await;
        assert!(
            result.is_err(),
            "in-grant symlinked object alternates must be rejected: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_quoted_out_of_grant_object_alternates() {
        let repository = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let outside_objects = outside.path().join("objects");
        std::fs::create_dir(&outside_objects).unwrap();
        std::fs::write(
            repository.path().join(".git/objects/info/alternates"),
            format!("\"{}\"\n", outside_objects.display()),
        )
        .unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status"}))
            .await;
        assert!(
            result.is_err(),
            "quoted out-of-grant object alternates must be rejected: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_cr_terminated_object_alternates() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        std::fs::write(
            repository.path().join(".git/objects/info/alternates"),
            "missing-alternate\r\n",
        )
        .unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status"}))
            .await;
        assert!(
            result.is_err(),
            "CR-terminated object alternates must be rejected: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_module_object_alternates() {
        let repository = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let module_alternates = repository
            .path()
            .join(".git/modules/sub/objects/info/alternates");
        std::fs::create_dir_all(module_alternates.parent().unwrap()).unwrap();
        let outside_objects = outside.path().join("objects");
        std::fs::create_dir(&outside_objects).unwrap();
        std::fs::write(
            &module_alternates,
            format!("{}\n", outside_objects.display()),
        )
        .unwrap();

        let result = test_tool(repository.path())
            .execute(json!({"operation": "status"}))
            .await;
        assert!(
            result.is_err(),
            "module object alternates must be rejected: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_accepts_cyclic_in_grant_object_alternates() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        std::fs::write(
            repository.path().join(".git/objects/info/alternates"),
            "../objects\n",
        )
        .unwrap();

        test_tool(repository.path())
            .validate_metadata_closure(repository.path(), false)
            .expect("a cyclic in-grant alternate must be bounded and remain authorized");
    }

    #[tokio::test]
    async fn git_operations_rejects_out_of_grant_write_hooks_path() {
        let repository = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        git_config_set_path(repository.path(), "core.hooksPath", outside.path());

        let error = test_tool(repository.path())
            .validate_metadata_closure(repository.path(), true)
            .expect_err("out-of-grant hooks must be rejected for write commands");
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string() == "Git metadata is not authorized"),
            "out-of-grant hooks must reach metadata authorization, not fail while parsing config: {error:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_interpolated_write_hooks_paths() {
        for hooks_path in [
            "",
            "~/zeroclaw-test-hooks",
            ":(optional)/tmp/zeroclaw-test-hooks",
            "%(prefix)/../../tmp/zeroclaw-test-hooks",
            "../../../../tmp/zeroclaw-test-hooks",
        ] {
            let repository = TempDir::new().unwrap();
            bootstrap_repo(repository.path(), &[]).await;
            let config = repository.path().join(".git/config");
            std::fs::write(
                &config,
                format!(
                    "{}\n[core]\n\thooksPath = {hooks_path}\n",
                    std::fs::read_to_string(&config).unwrap()
                ),
            )
            .unwrap();

            let result = test_tool(repository.path())
                .execute(json!({"operation": "commit", "message": "test"}))
                .await
                .expect("commit dispatch must return a tool result");
            assert!(
                !result.success,
                "interpolated hooks path must reject the write command: {result:?}"
            );
            assert!(
                result
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("Git repository metadata at")),
                "metadata validation must use the localized tool error: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn git_operations_accepts_in_grant_write_hooks_path() {
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let hooks = repository.path().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        let config = repository.path().join(".git/config");
        std::fs::write(
            &config,
            format!(
                "{}\n[core]\n\thooksPath = ./hooks\n",
                std::fs::read_to_string(&config).unwrap()
            ),
        )
        .unwrap();

        test_tool(repository.path())
            .validate_metadata_closure(repository.path(), true)
            .expect("in-grant hooks must remain authorized for write commands");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_operations_rejects_in_grant_symlinked_write_hooks_path() {
        use std::os::unix::fs::symlink;

        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;
        let hooks_target = repository.path().join("hooks-target");
        std::fs::create_dir(&hooks_target).unwrap();
        symlink(&hooks_target, repository.path().join("hooks")).unwrap();
        let config = repository.path().join(".git/config");
        std::fs::write(
            &config,
            format!(
                "{}\n[core]\n\thooksPath = ./hooks\n",
                std::fs::read_to_string(&config).unwrap()
            ),
        )
        .unwrap();

        let result =
            test_tool(repository.path()).validate_metadata_closure(repository.path(), true);
        assert!(
            result.is_err(),
            "symlinked hooks paths must be rejected even when their target is in-grant: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_accepts_hooks_path_below_incidental_modules_ancestor() {
        let parent = TempDir::new().unwrap();
        let repository = parent.path().join("modules/repository");
        std::fs::create_dir_all(&repository).unwrap();
        bootstrap_repo(&repository, &[]).await;
        std::fs::create_dir(repository.join("hooks")).unwrap();
        let config = repository.join(".git/config");
        std::fs::write(
            &config,
            format!(
                "{}\n[core]\n\thooksPath = ./hooks\n",
                std::fs::read_to_string(&config).unwrap()
            ),
        )
        .unwrap();

        test_tool(&repository)
            .validate_metadata_closure(&repository, true)
            .expect("an incidental modules ancestor must not change hooks validation");
    }

    #[cfg(unix)]
    #[test]
    fn git_worktree_list_preserves_trailing_path_whitespace() {
        let repository = TempDir::new().unwrap();
        git_init_no_sign(repository.path(), &[]);
        let worktree = repository.path().join("worktree-with-space ");
        std::fs::create_dir(&worktree).unwrap();
        let tool = test_tool(repository.path());
        let porcelain = format!(
            "worktree {}\nHEAD deadbeef\nbranch refs/heads/main\n\n",
            worktree.display()
        );

        let result = tool.parse_worktree_list(&porcelain, repository.path());
        assert_eq!(
            result["worktrees"][0]["path"],
            worktree.canonicalize().unwrap().to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_linked_worktree_metadata_outside_the_grant() {
        let workspace = TempDir::new().unwrap();
        let parent_repository = TempDir::new().unwrap();
        let linked_worktree_parent = TempDir::new().unwrap();
        bootstrap_repo(parent_repository.path(), &[]).await;
        let linked_worktree = linked_worktree_parent.path().join("linked-worktree");

        let status = std::process::Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&linked_worktree)
            .current_dir(parent_repository.path())
            .status()
            .unwrap();
        assert!(status.success(), "linked worktree setup must succeed");
        assert!(linked_worktree.join(".git").is_file());

        let tool = test_tool_with_allowed_root(workspace.path(), linked_worktree.clone());
        let resolved_worktree = linked_worktree.canonicalize().unwrap();
        let roots = tool.security.approved_read_roots(&resolved_worktree);
        assert_eq!(
            tool.has_repository_within_authorized_roots(&resolved_worktree, &roots, false),
            RepositoryAuthorization::Denied,
            "linked worktree metadata outside the grant must be denied before Git runs"
        );
        let result = tool
            .execute(json!({"operation": "status", "path": &linked_worktree}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "linked worktree metadata indirection must fail closed: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_do_not_mutate_linked_worktree_common_dir_via_read_only_grant() {
        let workspace = TempDir::new().unwrap();
        let main_repository = TempDir::new().unwrap();
        let linked_worktree_parent = TempDir::new().unwrap();
        bootstrap_repo(main_repository.path(), &[]).await;
        let linked_worktree = linked_worktree_parent.path().join("linked-worktree");
        let status = std::process::Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&linked_worktree)
            .current_dir(main_repository.path())
            .status()
            .unwrap();
        assert!(status.success(), "linked worktree setup must succeed");
        std::fs::write(linked_worktree.join("new-file"), "content").unwrap();
        let gitdir = std::fs::read_to_string(linked_worktree.join(".git"))
            .unwrap()
            .strip_prefix("gitdir: ")
            .unwrap()
            .trim()
            .to_owned();
        let index_path = PathBuf::from(gitdir).join("index");
        let index_before = std::fs::read(&index_path).unwrap();
        let tool = test_tool_with_allowed_and_read_only_roots(
            workspace.path(),
            linked_worktree.clone(),
            main_repository.path().to_path_buf(),
        );

        let result = tool
            .execute(json!({
                "operation": "add",
                "path": &linked_worktree,
                "paths": "new-file"
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a read-only common directory must not authorize linked-worktree mutation: {result:?}"
        );
        assert_eq!(
            std::fs::read(&index_path).unwrap(),
            index_before,
            "rejected linked-worktree mutation must not change its index"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_rejected_gitfile_before_an_authorized_parent() {
        let workspace = TempDir::new().unwrap();
        let external_repository = TempDir::new().unwrap();
        bootstrap_repo(workspace.path(), &[]).await;
        bootstrap_repo(external_repository.path(), &[]).await;
        let child = workspace.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(
            child.join(".git"),
            format!(
                "gitdir: {}\n",
                external_repository.path().join(".git").display()
            ),
        )
        .unwrap();
        let tool = test_tool(workspace.path());

        let result = tool
            .execute(json!({"operation": "status", "path": &child}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a rejected gitfile must not fall through to an authorized parent: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_operations_rejects_directory_metadata_with_an_unauthorized_commondir() {
        let workspace = TempDir::new().unwrap();
        let external_repository = TempDir::new().unwrap();
        bootstrap_repo(workspace.path(), &[]).await;
        bootstrap_repo(external_repository.path(), &[]).await;
        std::fs::write(
            workspace.path().join(".git/commondir"),
            format!("{}\n", external_repository.path().join(".git").display()),
        )
        .unwrap();
        let tool = test_tool(workspace.path());
        let resolved_workspace = workspace.path().canonicalize().unwrap();
        let roots = tool.security.approved_read_roots(&resolved_workspace);
        assert_eq!(
            tool.has_repository_within_authorized_roots(&resolved_workspace, &roots, false),
            RepositoryAuthorization::Denied,
            "an unauthorized commondir must be denied before Git runs"
        );

        let result = tool.execute(json!({"operation": "status"})).await.unwrap();

        assert!(
            !result.success,
            "directory metadata with an unauthorized commondir must fail closed: {result:?}"
        );
    }

    #[test]
    fn gitfile_without_commondir_is_authorized_as_a_submodule_boundary() {
        let workspace = TempDir::new().unwrap();
        let submodule = workspace.path().join("submodule");
        let gitdir = workspace.path().join("gitdir");
        std::fs::create_dir(&submodule).unwrap();
        std::fs::create_dir(&gitdir).unwrap();
        std::fs::write(
            submodule.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        let tool = test_tool(workspace.path());

        assert!(
            tool.linked_worktree_metadata_is_authorized(&submodule.join(".git"), false),
            "an independently authorized submodule gitdir must not require commondir"
        );
    }

    #[test]
    fn linked_worktree_commondir_must_stay_within_an_applicable_grant() {
        let workspace = TempDir::new().unwrap();
        let allowed_root = TempDir::new().unwrap();
        let external_common_dir = TempDir::new().unwrap();
        let linked_worktree = allowed_root.path().join("linked-worktree");
        let gitdir = allowed_root.path().join("gitdir");
        std::fs::create_dir(&linked_worktree).unwrap();
        std::fs::create_dir(&gitdir).unwrap();
        std::fs::write(
            linked_worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        std::fs::write(
            gitdir.join("commondir"),
            format!("{}\n", external_common_dir.path().display()),
        )
        .unwrap();

        let linked_worktree = linked_worktree.canonicalize().unwrap();
        let tool = test_tool_with_allowed_root(workspace.path(), allowed_root.path().to_path_buf());
        assert!(
            !tool.linked_worktree_metadata_is_authorized(&linked_worktree.join(".git"), false),
            "linked-worktree common metadata outside the grant must be rejected"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_status_in_read_only_root_does_not_run_repository_fsmonitor() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        bootstrap_repo(read_only_root.path(), &[]).await;
        let marker = workspace.path().join("fsmonitor-ran");
        let fsmonitor = format!("sh -c 'touch {}'", marker.display());
        let config = std::process::Command::new("git")
            .args(["config", "core.fsmonitor", &fsmonitor])
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(
            config.success(),
            "test repository configuration must succeed"
        );
        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "status", "path": read_only_root.path()}))
            .await
            .unwrap();

        assert!(result.success, "status failed: {:?}", result.error);
        assert!(
            !marker.exists(),
            "read-only status must not execute repository core.fsmonitor"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_read_only_log_commands_do_not_run_repository_gpg_program() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        bootstrap_repo(read_only_root.path(), &["tracked.txt"]).await;

        let marker = workspace.path().join("gpg-program-ran");
        let gpg_program = workspace.path().join("fake-gpg");
        std::fs::write(
            &gpg_program,
            format!("#!/bin/sh\ntouch {}\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&gpg_program, std::fs::Permissions::from_mode(0o700)).unwrap();

        let tree = std::process::Command::new("git")
            .args(["rev-parse", "HEAD^{tree}"])
            .current_dir(read_only_root.path())
            .output()
            .unwrap();
        assert!(tree.status.success(), "test setup must resolve HEAD tree");
        let tree = String::from_utf8(tree.stdout).unwrap();
        let signed_commit = format!(
            "tree {}\nauthor Test <test@test.com> 0 +0000\ncommitter Test <test@test.com> 0 +0000\ngpgsig -----BEGIN PGP SIGNATURE-----\n fake-signature\n -----END PGP SIGNATURE-----\n\nsigned probe\n",
            tree.trim()
        );
        let mut hasher = std::process::Command::new("git")
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .current_dir(read_only_root.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        hasher
            .stdin
            .as_mut()
            .unwrap()
            .write_all(signed_commit.as_bytes())
            .unwrap();
        let signed_commit = hasher.wait_with_output().unwrap();
        assert!(
            signed_commit.status.success(),
            "test setup must write signed commit"
        );
        let signed_commit = String::from_utf8(signed_commit.stdout).unwrap();
        let signed_commit = signed_commit.trim();
        for reference in ["refs/heads/master", "refs/stash"] {
            let update = std::process::Command::new("git")
                .args(["update-ref", reference, signed_commit])
                .current_dir(read_only_root.path())
                .status()
                .unwrap();
            assert!(update.success(), "test setup must update {reference}");
        }
        for args in [
            ["config", "gpg.program", gpg_program.to_str().unwrap()].as_slice(),
            ["config", "log.showSignature", "true"].as_slice(),
        ] {
            let config = std::process::Command::new("git")
                .args(args)
                .current_dir(read_only_root.path())
                .status()
                .unwrap();
            assert!(
                config.success(),
                "test repository configuration must succeed"
            );
        }

        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());
        for args in [
            json!({"operation": "log", "path": read_only_root.path()}),
            json!({"operation": "stash", "action": "list", "path": read_only_root.path()}),
        ] {
            let result = tool.execute(args).await.unwrap();
            assert!(result.success, "read command failed: {:?}", result.error);
            assert!(
                !marker.exists(),
                "read-only log commands must not execute repository gpg.program"
            );
        }
    }

    #[tokio::test]
    async fn git_read_only_log_does_not_read_repository_mailmap_file() {
        let workspace = TempDir::new().unwrap();
        let repository = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;

        let mailmap = outside.path().join("mailmap");
        std::fs::write(
            &mailmap,
            "Mapped Author <mapped@example.com> Test <test@test.com>\n",
        )
        .unwrap();
        for args in [
            ["config", "log.mailmap", "true"].as_slice(),
            ["config", "mailmap.file", mailmap.to_str().unwrap()].as_slice(),
        ] {
            let config = std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .status()
                .unwrap();
            assert!(
                config.success(),
                "test repository configuration must succeed"
            );
        }
        let mapped_author = std::process::Command::new("git")
            .args(["log", "--use-mailmap", "-1", "--format=%aN"])
            .current_dir(repository.path())
            .output()
            .unwrap();
        assert!(
            mapped_author.status.success(),
            "test mailmap control must succeed"
        );
        assert_eq!(
            String::from_utf8_lossy(&mapped_author.stdout).trim(),
            "Mapped Author",
            "test mailmap control must apply the configured mapping"
        );

        let tool = test_tool_with_read_only_root(workspace.path(), repository.path().to_path_buf());
        let result = tool
            .execute(json!({"operation": "log", "path": repository.path()}))
            .await
            .unwrap();

        assert!(result.success, "log failed: {:?}", result.error);
        assert!(
            result.output.to_string().contains("Test"),
            "log must preserve the commit's unmapped author: {result:?}"
        );
        assert!(
            !result.output.to_string().contains("Mapped Author"),
            "read-only log must not read repository mailmap.file: {result:?}"
        );
    }

    #[tokio::test]
    async fn git_read_only_log_does_not_read_repository_mailmap() {
        let workspace = TempDir::new().unwrap();
        let repository = TempDir::new().unwrap();
        bootstrap_repo(repository.path(), &[]).await;

        std::fs::write(
            repository.path().join(".mailmap"),
            "Mapped Author <mapped@example.com> Test <test@test.com>\n",
        )
        .unwrap();
        let config = std::process::Command::new("git")
            .args(["config", "log.mailmap", "true"])
            .current_dir(repository.path())
            .status()
            .unwrap();
        assert!(
            config.success(),
            "test repository configuration must succeed"
        );
        let mapped_author = std::process::Command::new("git")
            .args(["log", "--use-mailmap", "-1", "--format=%aN"])
            .current_dir(repository.path())
            .output()
            .unwrap();
        assert!(
            mapped_author.status.success(),
            "test mailmap control must succeed"
        );
        assert_eq!(
            String::from_utf8_lossy(&mapped_author.stdout).trim(),
            "Mapped Author",
            "test mailmap control must apply the repository mapping"
        );

        let tool = test_tool_with_read_only_root(workspace.path(), repository.path().to_path_buf());
        let result = tool
            .execute(json!({"operation": "log", "path": repository.path()}))
            .await
            .unwrap();

        assert!(result.success, "log failed: {:?}", result.error);
        assert!(
            result.output.to_string().contains("Test"),
            "log must preserve the commit's unmapped author: {result:?}"
        );
        assert!(
            !result.output.to_string().contains("Mapped Author"),
            "read-only log must not read repository .mailmap: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_read_only_commands_do_not_run_repository_clean_filters() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        bootstrap_repo(read_only_root.path(), &["tracked.txt"]).await;
        std::fs::write(
            read_only_root.path().join(".gitattributes"),
            "tracked.txt filter=marker\n",
        )
        .unwrap();
        for args in [
            ["add", ".gitattributes"].as_slice(),
            ["commit", "-m", "attributes"].as_slice(),
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(read_only_root.path())
                .status()
                .unwrap();
            assert!(status.success(), "test repository setup must succeed");
        }

        let marker = workspace.path().join("clean-filter-ran");
        let clean_filter = format!("sh -c 'touch {}; cat'", marker.display());
        let status = std::process::Command::new("git")
            .args(["config", "filter.marker.clean", &clean_filter])
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(status.success(), "test filter configuration must succeed");
        let required = std::process::Command::new("git")
            .args(["config", "filter.marker.required", "true"])
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(required.success(), "test filter must be required");
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::fs::write(read_only_root.path().join("tracked.txt"), "changed").unwrap();

        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());
        for args in [
            json!({"operation": "status", "path": read_only_root.path()}),
            json!({"operation": "diff", "path": read_only_root.path()}),
        ] {
            let result = tool.execute(args).await.unwrap();
            assert!(result.success, "read command failed: {:?}", result.error);
            assert!(
                !marker.exists(),
                "read-only Git commands must not execute repository clean filters"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_read_only_commands_do_not_run_submodule_clean_filters() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        let source_submodule = workspace.path().join("source-submodule");
        std::fs::create_dir(&source_submodule).unwrap();
        bootstrap_repo(&source_submodule, &["tracked.txt"]).await;
        std::fs::write(
            source_submodule.join(".gitattributes"),
            "tracked.txt filter=marker\n",
        )
        .unwrap();
        let attributes_commit = std::process::Command::new("git")
            .args(["add", ".gitattributes"])
            .current_dir(&source_submodule)
            .status()
            .unwrap();
        assert!(
            attributes_commit.success(),
            "test setup must stage attributes"
        );
        let attributes_commit = std::process::Command::new("git")
            .args(["commit", "-m", "attributes"])
            .current_dir(&source_submodule)
            .status()
            .unwrap();
        assert!(
            attributes_commit.success(),
            "test setup must commit attributes"
        );

        bootstrap_repo(read_only_root.path(), &[]).await;
        let submodule_add = std::process::Command::new("git")
            .args(["-c", "protocol.file.allow=always", "submodule", "add"])
            .arg(&source_submodule)
            .arg("submodule")
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(submodule_add.success(), "test setup must add submodule");
        let submodule_commit = std::process::Command::new("git")
            .args(["commit", "-am", "submodule"])
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(
            submodule_commit.success(),
            "test setup must commit submodule"
        );

        let marker = workspace.path().join("submodule-clean-filter-ran");
        let cloned_submodule = read_only_root.path().join("submodule");
        for args in [
            ["config", "user.email", "test@test.com"].as_slice(),
            ["config", "user.name", "Test"].as_slice(),
            ["config", "commit.gpgsign", "false"].as_slice(),
        ] {
            let identity = std::process::Command::new("git")
                .args(args)
                .current_dir(&cloned_submodule)
                .status()
                .unwrap();
            assert!(
                identity.success(),
                "test setup must configure clone identity"
            );
        }
        let clean_filter = format!("sh -c 'touch {}; cat'", marker.display());
        let filter_config = std::process::Command::new("git")
            .args(["config", "filter.marker.clean", &clean_filter])
            .current_dir(&cloned_submodule)
            .status()
            .unwrap();
        assert!(filter_config.success(), "test setup must configure filter");
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::fs::write(cloned_submodule.join("tracked.txt"), "changed").unwrap();

        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());
        let resolved_root = read_only_root.path().canonicalize().unwrap();
        for path in [read_only_root.path(), cloned_submodule.as_path()] {
            for args in [
                json!({"operation": "status", "path": path}),
                json!({"operation": "diff", "path": path}),
            ] {
                let result = tool.execute(args).await.unwrap();
                assert!(result.success, "read command failed: {:?}", result.error);
                assert!(
                    !marker.exists(),
                    "read-only Git commands must not execute submodule clean filters"
                );
            }
        }

        std::fs::write(cloned_submodule.join("other.txt"), "next").unwrap();
        let submodule_update = std::process::Command::new("git")
            .args(["add", "other.txt"])
            .current_dir(&cloned_submodule)
            .status()
            .unwrap();
        assert!(
            submodule_update.success(),
            "test setup must stage submodule update"
        );
        let submodule_update = std::process::Command::new("git")
            .args(["commit", "-m", "next"])
            .current_dir(&cloned_submodule)
            .status()
            .unwrap();
        assert!(
            submodule_update.success(),
            "test setup must commit submodule update"
        );
        let stage_gitlink = std::process::Command::new("git")
            .args(["add", "submodule"])
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(
            stage_gitlink.success(),
            "test setup must stage gitlink update"
        );

        let status = tool
            .run_git_read_command(
                &[
                    "--no-optional-locks",
                    "status",
                    "--ignore-submodules=dirty",
                    "--porcelain=2",
                    "--branch",
                ],
                &resolved_root,
            )
            .await
            .unwrap();
        assert!(
            status.contains("submodule"),
            "read status must retain staged superproject gitlink changes: {status:?}"
        );
        let diff = tool
            .run_git_read_command(
                &[
                    "--no-optional-locks",
                    "diff",
                    "--ignore-submodules=dirty",
                    "--cached",
                ],
                &resolved_root,
            )
            .await
            .unwrap();
        assert!(
            diff.contains("Subproject commit"),
            "read diff must retain staged superproject gitlink changes: {diff:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_read_only_diff_does_not_run_parent_external_diff_or_textconv() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        bootstrap_repo(read_only_root.path(), &["tracked.txt"]).await;
        std::fs::write(
            read_only_root.path().join(".gitattributes"),
            "tracked.txt diff=marker\n",
        )
        .unwrap();
        for args in [
            ["add", ".gitattributes"].as_slice(),
            ["commit", "-m", "attributes"].as_slice(),
        ] {
            let setup = std::process::Command::new("git")
                .args(args)
                .current_dir(read_only_root.path())
                .status()
                .unwrap();
            assert!(setup.success(), "test setup must succeed");
        }

        let external_marker = workspace.path().join("parent-external-diff-ran");
        let textconv_marker = workspace.path().join("parent-textconv-ran");
        for (key, value) in [
            (
                "diff.external",
                format!("sh -c 'touch {}'", external_marker.display()),
            ),
            (
                "diff.marker.textconv",
                format!("sh -c 'touch {}; cat'", textconv_marker.display()),
            ),
        ] {
            let config = std::process::Command::new("git")
                .args(["config", key, &value])
                .current_dir(read_only_root.path())
                .status()
                .unwrap();
            assert!(
                config.success(),
                "test repository configuration must succeed"
            );
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::fs::write(read_only_root.path().join("tracked.txt"), "changed").unwrap();

        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());
        for args in [
            json!({"operation": "diff", "path": read_only_root.path()}),
            json!({"operation": "diff", "cached": true, "path": read_only_root.path()}),
        ] {
            if args.get("cached").is_some() {
                let stage = std::process::Command::new("git")
                    .args(["add", "tracked.txt"])
                    .current_dir(read_only_root.path())
                    .status()
                    .unwrap();
                assert!(stage.success(), "test setup must stage tracked file");
            }
            let result = tool.execute(args).await.unwrap();
            assert!(result.success, "read diff failed: {:?}", result.error);
            assert!(
                !external_marker.exists() && !textconv_marker.exists(),
                "read-only diff must not execute parent external diff or textconv"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_read_only_diff_does_not_run_submodule_external_diff() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        let source_submodule = workspace.path().join("source-submodule");
        std::fs::create_dir(&source_submodule).unwrap();
        bootstrap_repo(&source_submodule, &["tracked.txt"]).await;
        bootstrap_repo(read_only_root.path(), &[]).await;

        let submodule_add = std::process::Command::new("git")
            .args(["-c", "protocol.file.allow=always", "submodule", "add"])
            .arg(&source_submodule)
            .arg("submodule")
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(submodule_add.success(), "test setup must add submodule");
        let submodule_commit = std::process::Command::new("git")
            .args(["commit", "-am", "submodule"])
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(
            submodule_commit.success(),
            "test setup must commit submodule"
        );

        let cloned_submodule = read_only_root.path().join("submodule");
        let initial_head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&cloned_submodule)
            .output()
            .unwrap();
        assert!(
            initial_head.status.success(),
            "test setup must resolve the initial submodule commit"
        );
        let initial_head = String::from_utf8(initial_head.stdout)
            .unwrap()
            .trim()
            .to_owned();
        for args in [
            ["config", "user.email", "test@test.com"].as_slice(),
            ["config", "user.name", "Test"].as_slice(),
            ["config", "commit.gpgsign", "false"].as_slice(),
        ] {
            let identity = std::process::Command::new("git")
                .args(args)
                .current_dir(&cloned_submodule)
                .status()
                .unwrap();
            assert!(
                identity.success(),
                "test setup must configure clone identity"
            );
        }
        let marker = workspace.path().join("submodule-external-diff-ran");
        let external_diff = format!("sh -c 'touch {}'", marker.display());
        let external_config = std::process::Command::new("git")
            .args(["config", "diff.external", &external_diff])
            .current_dir(&cloned_submodule)
            .status()
            .unwrap();
        assert!(
            external_config.success(),
            "test setup must configure external diff"
        );
        let parent_config = std::process::Command::new("git")
            .args(["config", "diff.submodule", "diff"])
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(
            parent_config.success(),
            "test setup must configure inline submodule diff"
        );

        std::fs::write(cloned_submodule.join("tracked.txt"), "changed").unwrap();
        let submodule_update = std::process::Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&cloned_submodule)
            .status()
            .unwrap();
        assert!(
            submodule_update.success(),
            "test setup must stage submodule update"
        );
        let submodule_update = std::process::Command::new("git")
            .args(["commit", "-m", "next"])
            .current_dir(&cloned_submodule)
            .status()
            .unwrap();
        assert!(
            submodule_update.success(),
            "test setup must commit submodule update"
        );
        let updated_head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&cloned_submodule)
            .output()
            .unwrap();
        assert!(
            updated_head.status.success(),
            "test setup must resolve the updated submodule commit"
        );
        let updated_head = String::from_utf8(updated_head.stdout)
            .unwrap()
            .trim()
            .to_owned();

        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());
        let assert_safe_diff = |result: &ToolResult| {
            assert!(result.success, "read diff failed: {:?}", result.error);
            assert!(
                result.output.to_string().contains("Subproject commit"),
                "read diff must retain the changed submodule commit: {:?}",
                result.output
            );
            assert!(
                result.output.to_string().contains(&initial_head)
                    && result.output.to_string().contains(&updated_head),
                "read diff must retain both submodule commit IDs: {:?}",
                result.output
            );
            assert!(
                !marker.exists(),
                "read-only diff must not execute submodule external diff"
            );
        };

        let unstaged = tool
            .execute(json!({"operation": "diff", "path": read_only_root.path()}))
            .await
            .unwrap();
        assert_safe_diff(&unstaged);

        let stage_gitlink = std::process::Command::new("git")
            .args(["add", "submodule"])
            .current_dir(read_only_root.path())
            .status()
            .unwrap();
        assert!(
            stage_gitlink.success(),
            "test setup must stage gitlink update"
        );
        let staged = tool
            .execute(json!({
                "operation": "diff",
                "cached": true,
                "path": read_only_root.path()
            }))
            .await
            .unwrap();
        assert_safe_diff(&staged);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_write_commands_retain_repository_clean_filters() {
        let workspace = TempDir::new().unwrap();
        bootstrap_repo(workspace.path(), &["tracked.txt"]).await;
        std::fs::write(
            workspace.path().join(".gitattributes"),
            "tracked.txt filter=marker\n",
        )
        .unwrap();
        let attributes = std::process::Command::new("git")
            .args(["add", ".gitattributes"])
            .current_dir(workspace.path())
            .status()
            .unwrap();
        assert!(attributes.success(), "test setup must stage attributes");
        let attributes = std::process::Command::new("git")
            .args(["commit", "-m", "attributes"])
            .current_dir(workspace.path())
            .status()
            .unwrap();
        assert!(attributes.success(), "test setup must commit attributes");

        let marker = workspace.path().join("write-clean-filter-ran");
        let clean_filter = format!("sh -c 'touch {}; cat'", marker.display());
        let filter = std::process::Command::new("git")
            .args(["config", "filter.marker.clean", &clean_filter])
            .current_dir(workspace.path())
            .status()
            .unwrap();
        assert!(filter.success(), "test setup must configure filter");
        std::fs::write(workspace.path().join("tracked.txt"), "changed").unwrap();

        let result = test_tool(workspace.path())
            .execute(json!({"operation": "add", "paths": "tracked.txt"}))
            .await
            .unwrap();
        assert!(result.success, "write command failed: {:?}", result.error);
        assert!(marker.exists(), "write command must retain clean filters");
    }

    #[test]
    fn filter_driver_names_allow_legal_config_subsections() {
        assert_eq!(
            GitOperationsTool::filter_driver_from_config_key("filter.my_filter.clean"),
            Some("my_filter".to_owned())
        );
        assert_eq!(
            GitOperationsTool::filter_driver_from_config_key("filter.a=b.clean"),
            None
        );
    }

    #[tokio::test]
    async fn git_status_does_not_refresh_index_in_read_only_root() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        bootstrap_repo(read_only_root.path(), &["tracked.txt"]).await;
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::fs::write(read_only_root.path().join("tracked.txt"), "initial").unwrap();
        let index_path = read_only_root.path().join(".git/index");
        let index_before = std::fs::read(&index_path).unwrap();
        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "status", "path": read_only_root.path()}))
            .await
            .unwrap();

        assert!(
            result.success,
            "Expected success, got error: {:?}",
            result.error
        );
        assert_eq!(
            std::fs::read(&index_path).unwrap(),
            index_before,
            "status must not refresh the index in a read-only root"
        );
    }

    #[tokio::test]
    async fn git_stash_list_reads_from_configured_read_only_root() {
        let workspace = TempDir::new().unwrap();
        let read_only_root = TempDir::new().unwrap();
        bootstrap_repo(read_only_root.path(), &[]).await;
        let tool =
            test_tool_with_read_only_root(workspace.path(), read_only_root.path().to_path_buf());

        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "list",
                "path": read_only_root.path()
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "Expected success, got error: {:?}",
            result.error
        );
    }

    async fn bootstrap_repo(dir: &std::path::Path, tracked_files: &[&str]) {
        git_init_no_sign(dir, &["-b", "master"]);
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        for f in tracked_files {
            std::fs::write(dir.join(f), "initial").unwrap();
        }
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn stash_push_default_stashes_staged_and_unstaged() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["staged.txt", "unstaged.txt"]).await;

        std::fs::write(tmp.path().join("staged.txt"), "s-modified").unwrap();
        std::fs::write(tmp.path().join("unstaged.txt"), "u-modified").unwrap();
        std::process::Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({"operation": "stash", "action": "push"}))
            .await
            .unwrap();
        assert!(result.success, "stash push failed: {:?}", result.error);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_out = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_out.trim().is_empty(),
            "expected clean working tree after default stash, got: {status_out:?}"
        );
    }

    #[tokio::test]
    async fn stash_push_with_keep_index_preserves_staged() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["staged.txt", "unstaged.txt"]).await;

        std::fs::write(tmp.path().join("staged.txt"), "s-modified").unwrap();
        std::fs::write(tmp.path().join("unstaged.txt"), "u-modified").unwrap();
        std::process::Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "push",
                "keep_index": true,
            }))
            .await
            .unwrap();
        assert!(result.success, "stash push -k failed: {:?}", result.error);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_out = String::from_utf8_lossy(&status.stdout).to_string();
        // `staged.txt` modification still present and staged (`M ` prefix);
        // `unstaged.txt` modification was stashed away — file matches HEAD.
        assert!(
            status_out.contains("M  staged.txt"),
            "staged modification should remain staged, status: {status_out:?}"
        );
        assert!(
            !status_out.contains("unstaged.txt"),
            "unstaged modification should have been stashed, status: {status_out:?}"
        );
    }

    #[tokio::test]
    async fn stash_push_with_paths_scopes_to_pathspec() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["a.txt", "b.txt"]).await;

        std::fs::write(tmp.path().join("a.txt"), "a-modified").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "b-modified").unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "push",
                "paths": "a.txt",
            }))
            .await
            .unwrap();
        assert!(
            result.success,
            "stash push -- a.txt failed: {:?}",
            result.error
        );

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_out = String::from_utf8_lossy(&status.stdout).to_string();
        assert!(
            !status_out.contains("a.txt"),
            "a.txt should have been stashed, status: {status_out:?}"
        );
        assert!(
            status_out.contains("b.txt"),
            "b.txt should remain modified, status: {status_out:?}"
        );
    }

    #[tokio::test]
    async fn stash_push_with_custom_message() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["a.txt"]).await;
        std::fs::write(tmp.path().join("a.txt"), "a-modified").unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "push",
                "message": "scoped-fix-wip",
            }))
            .await
            .unwrap();
        assert!(result.success, "stash push -m failed: {:?}", result.error);

        let list = std::process::Command::new("git")
            .args(["stash", "list"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let list_out = String::from_utf8_lossy(&list.stdout).to_string();
        assert!(
            list_out.contains("scoped-fix-wip"),
            "custom stash message missing from list, got: {list_out:?}"
        );
    }

    #[tokio::test]
    async fn stash_push_with_include_untracked_captures_new_files() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[]).await;
        std::fs::write(tmp.path().join("new.txt"), "untracked").unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "push",
                "include_untracked": true,
            }))
            .await
            .unwrap();
        assert!(result.success, "stash push -u failed: {:?}", result.error);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_out = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_out.trim().is_empty(),
            "expected clean tree after -u stash, got: {status_out:?}"
        );
    }

    #[tokio::test]
    async fn add_stages_multiple_space_separated_paths() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "b").unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = test_tool_with_security(security);

        let result = tool
            .execute(json!({"operation": "add", "paths": "a.txt b.txt"}))
            .await
            .unwrap();
        assert!(result.success, "add failed: {:?}", result.error);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&status.stdout);
        assert!(out.contains("A  a.txt"), "a.txt not staged: {out:?}");
        assert!(out.contains("A  b.txt"), "b.txt not staged: {out:?}");
    }

    #[tokio::test]
    async fn non_repository_error_includes_path_context_and_recovery_hint() {
        let tmp = TempDir::new().unwrap();
        // Do NOT git-init the temp dir — we want a non-repository path.
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: tmp.path().to_path_buf(),
            workspace_only: false,
            forbidden_paths: Vec::new(),
            ..SecurityPolicy::default()
        });
        let tool = test_tool_with_security(security);

        let result = tool.execute(json!({"operation": "status"})).await.unwrap();

        assert!(
            !result.success,
            "git_operations should fail when not in a repository"
        );

        let error = result.error.as_deref().unwrap_or("");
        let path_display = tmp.path().canonicalize().unwrap().display().to_string();

        // The error message must include the resolved working directory
        // path so the user can see where the tool was looking.
        assert!(
            error.contains(&path_display),
            "error should contain the working directory path '{path_display}', got: {error}"
        );

        assert!(
            include_str!("../locales/en/tools.ftl")
                .contains("tool-git-operations-error-not-in-repo = Not in a Git repository"),
            "the canonical English not-in-repository diagnostic must retain its recovery guidance"
        );
    }

    #[tokio::test]
    async fn bounded_discovery_reports_authorization_boundary_for_empty_workspace() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());
        let workspace = tmp.path().canonicalize().unwrap();
        let roots = tool.security.approved_read_roots(&workspace);
        assert_eq!(
            tool.has_repository_within_authorized_roots(&workspace, &roots, false),
            RepositoryAuthorization::DiscoveryBoundaryReached,
            "the default policy must distinguish its authorization boundary from an unbounded search"
        );
        let result = tool.execute(json!({"operation": "status"})).await.unwrap();

        assert!(
            !result.success,
            "an empty workspace is not a Git repository"
        );
        let error = result.error.as_deref().unwrap_or_default();
        assert!(
            error.contains(&workspace.display().to_string()),
            "the boundary diagnostic must include the requested path: {error}"
        );
        assert!(
            include_str!("../locales/en/tools.ftl")
                .contains("tool-git-operations-error-repository-outside-authorized-roots = No Git repository is reachable within the authorized roots"),
            "the canonical English boundary diagnostic must remain distinct from the not-in-repository message"
        );
    }
}
