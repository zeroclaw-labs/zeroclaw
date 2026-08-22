//! `zeroclaw agents export` — write an agent bundle to disk.
//!
//! The closure computation, credential scrubbing, and risk analysis all live
//! in [`zeroclaw_config::agent_bundle`]. This module is the I/O half: it
//! materializes a plan into a directory and reports to the operator what the
//! bundle carries, what it left behind, and what a receiving install would be
//! asked to grant.
//!
//! A bundle is published, not merged: it is built in a staging directory beside
//! the destination and swapped in once complete. A half-written bundle, and a
//! bundle carrying leftovers from an earlier export, are both states a
//! receiving operator has no way to detect from the manifest, so neither is
//! ever published.
//!
//! The swap is two renames, not one: the old bundle moves aside, then the
//! staged one moves in. That is rollback, not atomicity. Any failure the export
//! *returns* leaves the destination as it found it, because the rollback runs.
//! A crash between the two renames cannot roll anything back, and leaves the
//! destination absent with the old bundle intact under [`RETIRED_PREFIX`].
//! A crash-atomic directory exchange would need `RENAME_EXCHANGE`, which is
//! Linux-only, so the guarantee is stated as it is rather than widened.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use zeroclaw_config::agent_bundle::{
    self, CONFIG_FILE, ExportPlan, MANIFEST_FILE, Provenance, SKILLS_DIR, SkillBundleSource,
    WORKSPACE_DIR,
};
use zeroclaw_config::schema::Config;

use super::{mt, mta};

/// Staging directory prefix. Staging lives beside the destination so the
/// publishing rename stays within one filesystem.
const STAGING_PREFIX: &str = ".zeroclaw-export-";

/// Prefix for the previous bundle while the new one is being moved into place.
const RETIRED_PREFIX: &str = ".zeroclaw-export-old-";

/// Files carried by one copy pass.
#[derive(Debug, Default, PartialEq, Eq)]
struct CopyTally {
    files: usize,
    bytes: u64,
    /// Symlinks encountered and skipped. They are not followed: a link's
    /// target may sit outside the copied tree, and it would resolve to
    /// something different on the receiving host regardless.
    symlinks_skipped: usize,
    /// Sockets, FIFOs, and device nodes encountered and skipped. Counted for
    /// the same reason symlinks are: an entry that did not travel should be
    /// something the operator hears about, not something they discover.
    others_skipped: usize,
}

/// Skill-bundle content carried into the bundle.
#[derive(Debug, Default, PartialEq, Eq)]
struct SkillCopy {
    tally: CopyTally,
    /// Bundles whose directory was found and copied.
    bundles: usize,
    /// Referenced bundles that contributed no skills: the directory is absent,
    /// or holds nothing this bundle admits. Config load scaffolds an empty
    /// directory for every configured bundle, so "present but empty" is the
    /// ordinary shape of this, not an exotic one. Either way there is no
    /// content for the manifest to advertise.
    without_content: Vec<String>,
}

/// Everything an export copied, for the operator's report.
#[derive(Debug, Default, PartialEq, Eq)]
struct BundleCopy {
    workspace: CopyTally,
    skills: SkillCopy,
}

/// What one copy pass is walking: how to name it in errors, and which entries
/// it carries.
struct CopySpec<'a> {
    /// Path prefix inside the bundle, for operator-facing messages.
    root: String,
    /// Whether an entry, named relative to that root, is carried.
    filter: &'a dyn Fn(&Path) -> bool,
}

pub async fn run(config: &Config, alias: &str, out: &Path, force: bool) -> Result<()> {
    let mut plan = agent_bundle::plan_export(config, alias).map_err(anyhow::Error::new)?;

    let copied = write_bundle(&mut plan, out, force).await?;

    report(&plan, out, &copied);
    Ok(())
}

/// Materialize `plan` at `out`.
///
/// Everything that can refuse the export runs first, against nothing but
/// metadata; only then is a staging directory created, filled, and swapped in.
/// The destination is never partially written and never keeps an entry the new
/// manifest does not describe. On any returned error it is left as it was.
async fn write_bundle(plan: &mut ExportPlan, out: &Path, force: bool) -> Result<BundleCopy> {
    let dest = resolve_path(out)?;
    reject_source_overlap(&dest, plan, out)?;
    check_destination(&dest, out, force).await?;

    let config_toml = agent_bundle::render_config_toml(plan).map_err(anyhow::Error::new)?;

    let Some(parent) = dest.parent() else {
        bail!(
            "{}",
            mta(
                "cli-agent-export-dest-no-parent",
                &[("path", out.display().to_string().as_str())],
                "destination {$path} has no parent directory to stage the bundle beside"
            )
        );
    };
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("failed to create destination parent {}", parent.display()))?;

    // Dropping the staging directory removes it, so every `?` below cleans up
    // after itself and leaves the destination untouched. A crash skips the
    // drop, leaving the staged tree behind for the operator to delete.
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(parent)
        .with_context(|| {
            format!(
                "failed to create a staging directory for the bundle in {}",
                parent.display()
            )
        })?;

    write_file(&staging.path().join(CONFIG_FILE), &config_toml).await?;
    let mut copied = BundleCopy {
        workspace: copy_workspace(plan, &staging.path().join(WORKSPACE_DIR))?,
        skills: SkillCopy::default(),
    };
    copy_skill_bundles(plan, &staging.path().join(SKILLS_DIR), &mut copied)?;

    // The manifest describes the bundle, so it is rendered from what the copy
    // actually carried and written last. Advertising a `skills/<alias>/` tree
    // that is not there would outlive the terminal that said otherwise.
    agent_bundle::record_missing_skill_content(plan, &copied.skills.without_content);
    let manifest_toml = agent_bundle::render_manifest_toml(
        plan,
        &Provenance {
            zeroclaw_version: env!("CARGO_PKG_VERSION").to_string(),
            exported_at: chrono::Utc::now().to_rfc3339(),
        },
    )
    .map_err(anyhow::Error::new)?;
    write_file(&staging.path().join(MANIFEST_FILE), &manifest_toml).await?;

    publish(staging, &dest, parent)?;
    Ok(copied)
}

/// Resolve `path` to an absolute, symlink-free form.
///
/// The destination need not exist yet, so the nearest existing ancestor is
/// canonicalized and the components below it are re-appended. Resolving both
/// sides this way is what makes [`reject_source_overlap`] trustworthy: a
/// symlinked ancestor (`/tmp` → `/private/tmp` on macOS, an operator's
/// symlinked data dir anywhere) would otherwise hide an overlap.
fn resolve_path(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    let mut below: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = absolute.as_path();
    loop {
        if let Ok(canonical) = cursor.canonicalize() {
            let mut resolved = canonical;
            resolved.extend(below.iter().rev());
            return Ok(resolved);
        }
        match (cursor.file_name(), cursor.parent()) {
            (Some(name), Some(parent)) => {
                below.push(name.to_os_string());
                cursor = parent;
            }
            // No ancestor exists at all (an absolute path under a missing
            // root); nothing can be canonicalized, so compare it as written.
            _ => return Ok(absolute),
        }
    }
}

/// Refuse an export whose destination overlaps a tree it reads, in either
/// direction.
///
/// A destination containing a source would have publishing replace the very
/// tree the bundle is reading, destroying it. A destination inside a source
/// would have the copy walk into its own output. Every tree the export reads
/// is checked, the workspace and each skill bundle alike, before anything is
/// created.
fn reject_source_overlap(dest: &Path, plan: &ExportPlan, out: &Path) -> Result<()> {
    reject_overlap(dest, &plan.workspace_source, out, None)?;
    for skills in &plan.skill_sources {
        reject_overlap(dest, &skills.source, out, Some(&skills.alias))?;
    }
    Ok(())
}

/// One source tree against the destination. `bundle` names the skill bundle
/// being read, or `None` for the agent's workspace.
fn reject_overlap(dest: &Path, source: &Path, out: &Path, bundle: Option<&str>) -> Result<()> {
    let resolved = resolve_path(source)?;
    let dest_display = out.display().to_string();
    let source_display = source.display().to_string();
    // Containment is checked first so that a destination equal to the source
    // reports the destructive shape rather than the recursive one.
    if resolved.starts_with(dest) {
        match bundle {
            None => bail!(
                "{}",
                mta(
                    "cli-agent-export-dest-contains-workspace",
                    &[
                        ("path", dest_display.as_str()),
                        ("workspace", source_display.as_str())
                    ],
                    "destination {$path} contains the agent workspace {$workspace} — exporting there would replace the workspace itself"
                )
            ),
            Some(alias) => bail!(
                "{}",
                mta(
                    "cli-agent-export-dest-contains-skills",
                    &[
                        ("path", dest_display.as_str()),
                        ("alias", alias),
                        ("source", source_display.as_str())
                    ],
                    "destination {$path} contains skill bundle `{$alias}` at {$source} — exporting there would replace the skills the bundle carries"
                )
            ),
        }
    }
    if dest.starts_with(&resolved) {
        match bundle {
            None => bail!(
                "{}",
                mta(
                    "cli-agent-export-dest-inside-workspace",
                    &[
                        ("path", dest_display.as_str()),
                        ("workspace", source_display.as_str())
                    ],
                    "destination {$path} is inside the agent workspace {$workspace} — choose a path outside it"
                )
            ),
            Some(alias) => bail!(
                "{}",
                mta(
                    "cli-agent-export-dest-inside-skills",
                    &[
                        ("path", dest_display.as_str()),
                        ("alias", alias),
                        ("source", source_display.as_str())
                    ],
                    "destination {$path} is inside skill bundle `{$alias}` at {$source} — choose a path outside it"
                )
            ),
        }
    }
    Ok(())
}

/// Check that the destination can be published to, without touching it. A
/// non-directory is refused outright; a directory that already holds files
/// needs `--force`, which replaces its contents rather than merging into them.
async fn check_destination(dest: &Path, out: &Path, force: bool) -> Result<()> {
    if !dest.exists() {
        return Ok(());
    }
    if !dest.is_dir() {
        bail!(
            "{}",
            mta(
                "cli-agent-export-dest-not-a-dir",
                &[("path", out.display().to_string().as_str())],
                "destination {$path} exists and is not a directory"
            )
        );
    }
    let mut entries = tokio::fs::read_dir(dest)
        .await
        .with_context(|| format!("failed to read destination {}", dest.display()))?;
    let occupied = entries
        .next_entry()
        .await
        .with_context(|| format!("failed to read destination {}", dest.display()))?
        .is_some();
    if occupied && !force {
        bail!(
            "{}",
            mta(
                "cli-agent-export-dest-not-empty",
                &[("path", out.display().to_string().as_str())],
                "destination {$path} is not empty — pass --force to replace its contents"
            )
        );
    }
    Ok(())
}

/// Swap the staged bundle into place.
fn publish(staging: tempfile::TempDir, dest: &Path, parent: &Path) -> Result<()> {
    // On failure the guard drops and removes the staged tree. On success the
    // rename has already moved it, so the guard is disarmed rather than left to
    // recurse over a path that is now the published bundle.
    swap_into_place(staging.path(), dest, parent)?;
    let _ = staging.keep();
    Ok(())
}

/// Move `staged` onto `dest`, replacing whatever the destination held.
///
/// An existing bundle is moved aside rather than deleted first, so a failed
/// move can put it back. The window between the two renames is real: a crash
/// inside it leaves the destination absent and the previous bundle under the
/// retired name, which is why that name is derived from the staging token
/// rather than being random on its own. Recovery is renaming it back.
fn swap_into_place(staged: &Path, dest: &Path, parent: &Path) -> Result<()> {
    if !dest.exists() {
        return std::fs::rename(staged, dest)
            .with_context(|| format!("failed to move the staged bundle into {}", dest.display()));
    }

    let retired = parent.join(retired_name(staged));
    if retired.exists() {
        // A leftover from a run that died mid-publish, wearing the same token.
        // Refuse rather than consume it: the caller cleans up the staged tree
        // and the destination is still the bundle it was.
        bail!(
            "cannot retire the existing bundle: {} already exists",
            retired.display()
        );
    }
    std::fs::rename(dest, &retired).with_context(|| {
        format!(
            "failed to move the existing bundle at {} aside",
            dest.display()
        )
    })?;
    match std::fs::rename(staged, dest) {
        Ok(()) => {
            // The new bundle is published; the old one is now unreferenced.
            // Failing to reap it is untidy, not a failed export.
            std::fs::remove_dir_all(&retired).ok();
            Ok(())
        }
        Err(err) => {
            if std::fs::rename(&retired, dest).is_ok() {
                return Err(err).with_context(|| {
                    format!("failed to move the staged bundle into {}", dest.display())
                });
            }
            let dest_display = dest.display().to_string();
            let retired_display = retired.display().to_string();
            let error = err.to_string();
            bail!(
                "{}",
                mta(
                    "cli-agent-export-restore-failed",
                    &[
                        ("path", dest_display.as_str()),
                        ("retired", retired_display.as_str()),
                        ("error", error.as_str())
                    ],
                    "failed to publish the bundle to {$path} ({$error}), and the previous bundle could not be moved back — it is at {$retired}"
                )
            );
        }
    }
}

async fn write_file(path: &Path, contents: &str) -> Result<()> {
    tokio::fs::write(path, contents)
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Copy the agent's workspace into the bundle, honoring the plan's memory
/// exclusion. A missing source workspace is not an error: an agent that has
/// never run has nothing on disk yet.
///
/// The workspace is live — the agent that owns it can be writing to it through
/// an ordinary tool call while the export runs — so the walk never re-opens a
/// path by name. The configured root is opened once, and every entry below it
/// is classified *and* read through a handle on the directory that holds it
/// (cap-std, the same binding `deliver_file` uses).
///
/// Both steps refuse symlinks, which is what makes them describe one object.
/// Keeping the read merely *beneath* the root is not enough: this exporter has
/// content boundaries of its own inside that root, so an entry classified as an
/// ordinary file and then swapped for an in-root link to `memory/brain.db`
/// would copy the memory store under the admitted name without ever leaving the
/// workspace. The open therefore refuses to traverse a link at all, and a swap
/// between the two steps fails the export instead of silently redirecting it.
fn copy_workspace(plan: &ExportPlan, dest: &Path) -> Result<CopyTally> {
    let mut copied = CopyTally::default();
    if !plan.workspace_source.is_dir() {
        return Ok(copied);
    }
    let source =
        Dir::open_ambient_dir(&plan.workspace_source, ambient_authority()).with_context(|| {
            format!(
                "failed to open workspace {}",
                plan.workspace_source.display()
            )
        })?;
    let target = open_bundle_dir(dest)?;
    let spec = CopySpec {
        root: WORKSPACE_DIR.to_string(),
        filter: &agent_bundle::workspace_entry_included,
    };
    copy_tree(&source, &target, &spec, &PathBuf::new(), &mut copied)?;
    Ok(copied)
}

/// Copy the content of every skill bundle the agent references.
///
/// Skills live under the install-wide `shared/skills/` tree rather than the
/// agent's workspace, so the config alone would import an agent whose skills
/// are absent. A skill is a directory inside the bundle's directory: the copy
/// carries the child directories the bundle admits and nothing else, so a
/// skill the bundle excludes never travels and loose local state (sync
/// markers and the like) stays behind.
fn copy_skill_bundles(plan: &ExportPlan, dest_root: &Path, into: &mut BundleCopy) -> Result<()> {
    for source in &plan.skill_sources {
        if !source.source.is_dir() {
            into.skills.without_content.push(source.alias.clone());
            continue;
        }
        let bundle = Dir::open_ambient_dir(&source.source, ambient_authority())
            .with_context(|| format!("failed to open skill bundle `{}`", source.alias))?;
        let bundle_dest = dest_root.join(&source.alias);
        let target = open_bundle_dir(&bundle_dest)?;
        let carried = copy_skills(&bundle, &target, source, &mut into.skills.tally)?;
        if carried == 0 {
            // An empty tree is not content. Leave neither the directory nor
            // the manifest claim behind for it.
            drop(target);
            std::fs::remove_dir_all(&bundle_dest).ok();
            into.skills.without_content.push(source.alias.clone());
            continue;
        }
        into.skills.bundles += 1;
    }
    if into.skills.bundles == 0 {
        // Nothing was carried, so the bundle gets no `skills/` at all rather
        // than an empty directory implying otherwise.
        std::fs::remove_dir_all(dest_root).ok();
    }
    Ok(())
}

/// Copy the skills one bundle admits, each through its own directory handle.
///
/// The open is no-follow like the workspace walk's: an admitted skill swapped
/// for a link to a sibling the bundle excludes would otherwise carry the
/// excluded content under the admitted name.
fn copy_skills(
    source: &Dir,
    dest: &Dir,
    bundle: &SkillBundleSource,
    copied: &mut CopyTally,
) -> Result<usize> {
    let mut skills = 0;
    let root = format!("{SKILLS_DIR}/{}", bundle.alias);
    let entries = source
        .entries()
        .with_context(|| format!("failed to read {root}"))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {root}"))?;
        let name = entry.file_name();
        // A skill the config cannot name is not a skill this bundle grants.
        let Some(skill) = name.to_str() else {
            continue;
        };
        if !bundle.filter.admits_skill(skill) {
            continue;
        }
        let file_type = entry
            .metadata()
            .with_context(|| format!("failed to stat {root}/{skill}"))?
            .file_type();
        if file_type.is_symlink() {
            copied.symlinks_skipped += 1;
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        entry_swap_seam(Path::new(skill));

        let child_source = source
            .open_dir_nofollow(&name)
            .with_context(|| format!("failed to open {root}/{skill}"))?;
        dest.create_dir(&name)
            .with_context(|| format!("failed to create {root}/{skill} in the bundle"))?;
        let child_dest = dest
            .open_dir_nofollow(&name)
            .with_context(|| format!("failed to open {root}/{skill} in the bundle"))?;
        let spec = CopySpec {
            root: root.clone(),
            filter: &|_| true,
        };
        copy_tree(&child_source, &child_dest, &spec, Path::new(skill), copied)?;
        skills += 1;
    }
    Ok(skills)
}

/// Name to move the existing bundle aside under.
///
/// The staging directory's name was allocated uniquely in this parent, so
/// reusing its random token keeps the retired name unique too, without the
/// exporter carrying a random-number dependency of its own.
fn retired_name(staged: &Path) -> String {
    let token = staged
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let token = token.strip_prefix(STAGING_PREFIX).unwrap_or(&token);
    format!("{RETIRED_PREFIX}{token}")
}

/// Create a directory inside the staging bundle and open a handle on it.
fn open_bundle_dir(path: &Path) -> Result<Dir> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    Dir::open_ambient_dir(path, ambient_authority())
        .with_context(|| format!("failed to open {}", path.display()))
}

/// Copy one directory's entries, recursing through child handles.
///
/// Writes are bound the same way as reads: the bundle side is a handle too, so
/// a symlink planted in the staging tree cannot redirect a write out of it.
fn copy_tree(
    source: &Dir,
    dest: &Dir,
    spec: &CopySpec<'_>,
    relative: &Path,
    copied: &mut CopyTally,
) -> Result<()> {
    let entries = source
        .entries()
        .with_context(|| format!("failed to read {}", rel(spec, relative)))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", rel(spec, relative)))?;
        let name = entry.file_name();
        let child = relative.join(&name);
        if !(spec.filter)(&child) {
            continue;
        }

        // `DirEntry::metadata` is a no-follow stat through the directory handle,
        // so it describes the object sitting in *this* directory under that
        // name, not whatever a fresh path lookup would resolve to. The opens
        // below are no-follow through the same handle, so the object that is
        // read is the object this classified.
        let file_type = entry
            .metadata()
            .with_context(|| format!("failed to stat {}", rel(spec, &child)))?
            .file_type();
        if file_type.is_symlink() {
            copied.symlinks_skipped += 1;
            continue;
        }
        if !file_type.is_dir() && !file_type.is_file() {
            // Sockets, FIFOs, devices: nothing a bundle can carry.
            copied.others_skipped += 1;
            continue;
        }

        entry_swap_seam(&child);

        if file_type.is_dir() {
            let child_source = source
                .open_dir_nofollow(&name)
                .with_context(|| format!("failed to open {}", rel(spec, &child)))?;
            dest.create_dir(&name)
                .with_context(|| format!("failed to create {} in the bundle", rel(spec, &child)))?;
            let child_dest = dest
                .open_dir_nofollow(&name)
                .with_context(|| format!("failed to open {} in the bundle", rel(spec, &child)))?;
            copy_tree(&child_source, &child_dest, spec, &child, copied)?;
        } else {
            let mut reader = entry
                .open_with(OpenOptions::new().read(true).follow(FollowSymlinks::No))
                .with_context(|| format!("failed to open {}", rel(spec, &child)))?;
            // The opened handle, not the name, decides what gets copied: an
            // entry that is no longer a regular file is left out rather than
            // read through whatever replaced it.
            let source_metadata = reader
                .metadata()
                .with_context(|| format!("failed to stat {}", rel(spec, &child)))?;
            if !source_metadata.is_file() {
                continue;
            }
            let mut writer = dest
                .open_with(&name, OpenOptions::new().write(true).create_new(true))
                .with_context(|| format!("failed to create {} in the bundle", rel(spec, &child)))?;
            let bytes = std::io::copy(&mut reader, &mut writer)
                .with_context(|| format!("failed to copy {} into the bundle", rel(spec, &child)))?;
            // Carry the mode across, so an executable in the workspace is still
            // executable for whoever imports the bundle.
            writer
                .set_permissions(source_metadata.permissions())
                .with_context(|| format!("failed to set permissions on {}", rel(spec, &child)))?;
            copied.files += 1;
            copied.bytes += bytes;
        }
    }
    Ok(())
}

/// Render a path for an error message, named as it sits inside the bundle.
fn rel(spec: &CopySpec<'_>, relative: &Path) -> String {
    let shown = relative.display().to_string();
    if shown.is_empty() {
        spec.root.clone()
    } else {
        format!("{}/{shown}", spec.root)
    }
}

/// Test seam: runs between an entry's no-follow classification and the
/// handle-bound open of that entry, the interleaving at which a path-based copy
/// could be made to follow a symlink out of the workspace. Compiled away
/// outside tests.
#[cfg(not(test))]
#[inline]
fn entry_swap_seam(_relative: &Path) {}

#[cfg(test)]
fn entry_swap_seam(relative: &Path) {
    tests::run_entry_swap_seam(relative);
}

fn report(plan: &ExportPlan, out: &Path, copied: &BundleCopy) {
    let files = copied.workspace.files.to_string();
    let kib = (copied.workspace.bytes / 1024).to_string();
    println!(
        "{}",
        mta(
            "cli-agent-export-written",
            &[
                ("alias", plan.root_alias.as_str()),
                ("path", out.display().to_string().as_str()),
                ("files", files.as_str()),
                ("kib", kib.as_str()),
            ],
            "exported agent `{$alias}` to {$path} ({$files} workspace file(s), {$kib} KiB)"
        )
    );

    if copied.skills.bundles > 0 {
        let files = copied.skills.tally.files.to_string();
        let bundles = copied.skills.bundles.to_string();
        println!(
            "{}",
            mta(
                "cli-agent-export-skills-carried",
                &[("files", files.as_str()), ("bundles", bundles.as_str())],
                "  {$files} skill file(s) carried from {$bundles} skill bundle(s)"
            )
        );
    }
    let others_skipped = copied.workspace.others_skipped + copied.skills.tally.others_skipped;
    if others_skipped > 0 {
        let count = others_skipped.to_string();
        println!(
            "{}",
            mta(
                "cli-agent-export-others-skipped",
                &[("count", count.as_str())],
                "  {$count} special file(s) skipped — sockets, FIFOs, and devices are host \
                 state, not content a bundle can carry"
            )
        );
    }

    let symlinks_skipped = copied.workspace.symlinks_skipped + copied.skills.tally.symlinks_skipped;
    if symlinks_skipped > 0 {
        let count = symlinks_skipped.to_string();
        println!(
            "{}",
            mta(
                "cli-agent-export-symlinks-skipped",
                &[("count", count.as_str())],
                "  {$count} symlink(s) skipped — links are not followed into a bundle"
            )
        );
    }

    if !plan.risk_flags.is_empty() {
        let count = plan.risk_flags.len().to_string();
        println!(
            "\n{}",
            mta(
                "cli-agent-export-risk-header",
                &[("count", count.as_str())],
                "⚠️  {$count} capability grant(s) an importing operator must accept:"
            )
        );
        for flag in &plan.risk_flags {
            println!(
                "{}",
                mta(
                    "cli-agent-export-risk-entry",
                    &[
                        ("kind", flag.kind.as_wire()),
                        ("path", flag.path.as_str()),
                        ("detail", flag.detail.as_str()),
                    ],
                    "  [{$kind}] {$path} — {$detail}"
                )
            );
        }
    }

    if !plan.required_secrets.is_empty() {
        let count = plan.required_secrets.len().to_string();
        println!(
            "\n{}",
            mta(
                "cli-agent-export-secrets-header",
                &[("count", count.as_str())],
                "🔑 {$count} credential(s) were scrubbed and must be supplied on import:"
            )
        );
        for path in &plan.required_secrets {
            println!(
                "{}",
                mta(
                    "cli-agent-export-secrets-entry",
                    &[("path", path.as_str())],
                    "  {$path}"
                )
            );
        }
    }

    if !plan.dropped.is_empty() {
        let count = plan.dropped.len().to_string();
        println!(
            "\n{}",
            mta(
                "cli-agent-export-dropped-header",
                &[("count", count.as_str())],
                "ℹ️  {$count} item(s) could not travel and were left behind:"
            )
        );
        for entry in &plan.dropped {
            println!(
                "{}",
                mta(
                    "cli-agent-export-dropped-entry",
                    &[
                        ("path", entry.path.as_str()),
                        ("reason", entry.reason.as_wire()),
                        ("detail", entry.detail.as_str()),
                    ],
                    "  {$path} ({$reason}) — {$detail}"
                )
            );
        }
    }

    // Scrubbing is a schema-driven pass over the config closure. It does not
    // and cannot reach the files a bundle carries, so the operator is told
    // plainly rather than left to infer it from the scrubbed-credentials list
    // just above.
    // The scrubbed-credentials list above is easy to read as "credentials were
    // detected and removed". It is narrower than that, and the difference is
    // the operator's to act on.
    println!(
        "\n{}",
        mt(
            "cli-agent-export-scrub-scope",
            "⚠️  Scrubbing blanks the fields the schema marks secret. It is not credential \
             detection: other config values travel as written, so a token in an MCP server's \
             url, or a credential in its command or args, is carried and repeated in the \
             manifest's risk flags."
        )
    );

    let carried = copied.workspace.files + copied.skills.tally.files;
    if carried > 0 {
        let count = carried.to_string();
        println!(
            "\n{}",
            mta(
                "cli-agent-export-content-not-scrubbed",
                &[("count", count.as_str())],
                "⚠️  {$count} carried file(s) are copied as-is. Scrubbing covers config.toml \
                 only: workspace and skill content is never scanned for secrets, so a .env \
                 file, a token in a note, or a credential in a git remote will be contained in \
                 the export."
            )
        );
    }

    println!(
        "\n{}",
        mt(
            "cli-agent-export-review-hint",
            "Review config.toml, zeroclaw-agent.toml, and the files the bundle carries before \
             sharing it."
        )
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn plan_for(workspace: &Path) -> ExportPlan {
        ExportPlan {
            root_alias: "researcher".to_string(),
            config: toml::Table::new(),
            required_secrets: Vec::new(),
            dropped: Vec::new(),
            risk_flags: Vec::new(),
            workspace_source: workspace.to_path_buf(),
            skill_sources: Vec::new(),
        }
    }

    /// The published manifest, parsed.
    fn manifest_of(out: &Path) -> toml::Table {
        std::fs::read_to_string(out.join(MANIFEST_FILE))
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Values of a manifest array field, as strings.
    fn manifest_list(manifest: &toml::Table, field: &str) -> Vec<String> {
        manifest
            .get(field)
            .and_then(toml::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    }

    /// Sorted names directly under `dir` — used to assert that an export left
    /// no staging or retired directory behind.
    fn entry_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A swap to perform mid-copy, keyed on the entry's workspace-relative path.
    type Swap = Box<dyn Fn(&Path)>;

    thread_local! {
        /// Swap to perform at [`entry_swap_seam`]. Thread-local, so tests
        /// running in parallel cannot see each other's.
        static ENTRY_SWAP: std::cell::RefCell<Option<Swap>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn run_entry_swap_seam(relative: &Path) {
        ENTRY_SWAP.with_borrow(|swap| {
            if let Some(swap) = swap.as_ref() {
                swap(relative);
            }
        });
    }

    /// Installs a swap at the copy's check-to-read seam for as long as it is
    /// held, so a replacement race can be reproduced at an exact interleaving.
    struct EntrySwap;

    impl EntrySwap {
        fn install(swap: impl Fn(&Path) + 'static) -> Self {
            ENTRY_SWAP.with_borrow_mut(|slot| *slot = Some(Box::new(swap)));
            Self
        }
    }

    impl Drop for EntrySwap {
        fn drop(&mut self) {
            ENTRY_SWAP.with_borrow_mut(|slot| *slot = None);
        }
    }

    /// The guide tells an operator to recover a crashed export by renaming
    /// `.zeroclaw-export-old-<token>` back, and to recognise the pair by their
    /// shared token. That pairing is a promise, so it is pinned here.
    #[test]
    fn the_retired_name_pairs_with_the_staging_directory() {
        assert_eq!(
            retired_name(Path::new("/tmp/bundles/.zeroclaw-export-AbC123")),
            ".zeroclaw-export-old-AbC123"
        );
    }

    #[test]
    fn copy_skips_the_memory_store() {
        let source = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");
        write(&source.path().join("notes/plan.md"), "plan");
        write(&source.path().join("memory/brain.db"), "sqlite");

        let plan = plan_for(source.path());
        let copied = copy_workspace(&plan, dest.path()).unwrap();

        assert_eq!(copied.files, 2);
        assert!(dest.path().join("IDENTITY.md").exists());
        assert!(dest.path().join("notes/plan.md").exists());
        assert!(!dest.path().join("memory").exists());
    }

    /// Every file under `dir`, recursively.
    fn all_files(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut queue = vec![dir.to_path_buf()];
        while let Some(next) = queue.pop() {
            for entry in std::fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    queue.push(path);
                } else {
                    found.push(path);
                }
            }
        }
        found
    }

    /// Format 1 carries no memory at all, so the bundle must contain no
    /// database — under any name, not just the paths the filter knows about.
    /// This is the consistency boundary the format chose: a live WAL database
    /// cannot be copied as files without risking a torn read, so none is
    /// copied, and the export is checked against the published artifact.
    #[tokio::test]
    async fn no_database_reaches_the_published_bundle() {
        const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");
        // A store mid-write: the database plus the WAL sidecars that hold
        // commits not yet folded back into it.
        std::fs::create_dir_all(source.path().join("memory")).unwrap();
        for name in ["brain.db", "brain.db-wal", "brain.db-shm"] {
            let mut bytes = SQLITE_MAGIC.to_vec();
            bytes.extend_from_slice(b"\x00\x01uncommitted");
            std::fs::write(source.path().join("memory").join(name), bytes).unwrap();
        }
        // The snapshot the store re-hydrates from lives at the workspace root,
        // outside `memory/`, and is memory in another form.
        write(
            &source.path().join("MEMORY_SNAPSHOT.md"),
            "# 🧠 ZeroClaw Memory Snapshot\n\n- user's home address\n",
        );

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let copied = write_bundle(&mut plan_for(source.path()), &out, false)
            .await
            .unwrap();

        assert_eq!(copied.workspace.files, 1);
        assert!(!out.join(WORKSPACE_DIR).join("memory").exists());
        assert!(!out.join(WORKSPACE_DIR).join("MEMORY_SNAPSHOT.md").exists());

        for file in all_files(&out) {
            let bytes = std::fs::read(&file).unwrap();
            assert!(
                !bytes.starts_with(SQLITE_MAGIC),
                "{} is a database",
                file.display()
            );
            assert!(
                !String::from_utf8_lossy(&bytes).contains("home address"),
                "{} carries memory content",
                file.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn copy_counts_special_files_it_skips() {
        use std::os::unix::net::UnixListener;

        let source = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");
        // A socket is host state: it cannot travel, and the operator should
        // hear that it did not rather than infer it from a file count.
        let _socket = UnixListener::bind(source.path().join("agent.sock")).unwrap();

        let plan = plan_for(source.path());
        let copied = copy_workspace(&plan, dest.path()).unwrap();

        assert_eq!(copied.files, 1);
        assert_eq!(copied.others_skipped, 1);
        assert_eq!(copied.symlinks_skipped, 0);
        assert!(!dest.path().join("agent.sock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn copy_skips_symlinks_instead_of_following_them_out_of_the_workspace() {
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("secret.txt"), "host secret");

        let source = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            source.path().join("escape.txt"),
        )
        .unwrap();

        let plan = plan_for(source.path());
        let copied = copy_workspace(&plan, dest.path()).unwrap();

        assert_eq!(copied.files, 1);
        assert_eq!(copied.symlinks_skipped, 1);
        assert!(!dest.path().join("escape.txt").exists());
    }

    #[test]
    fn missing_workspace_is_not_an_error() {
        let dest = tempfile::tempdir().unwrap();
        let plan = plan_for(Path::new("/nonexistent/zeroclaw/workspace"));
        let copied = copy_workspace(&plan, dest.path()).unwrap();
        assert_eq!(copied, CopyTally::default());
    }

    /// A plan carrying one skill bundle the way `plan_export` builds one: the
    /// source to copy, plus the `carried_skills` grant that advertises it in
    /// the manifest. The advertisement is what has to track reality.
    fn plan_with_skills(workspace: &Path, dir: &Path, exclude: &[&str]) -> ExportPlan {
        let mut plan = plan_for(workspace);
        plan.skill_sources = vec![skill_source("research_tools", dir, exclude)];
        plan.risk_flags
            .push(zeroclaw_config::agent_bundle::RiskFlag {
                kind: zeroclaw_config::agent_bundle::RiskKind::CarriedSkills,
                path: "skill_bundles.research_tools".to_string(),
                detail: "carries this skill bundle's content".to_string(),
            });
        plan
    }

    fn skill_source(alias: &str, dir: &Path, exclude: &[&str]) -> SkillBundleSource {
        SkillBundleSource {
            alias: alias.to_string(),
            source: dir.to_path_buf(),
            filter: zeroclaw_config::schema::SkillBundleConfig {
                directory: None,
                include: Vec::new(),
                exclude: exclude.iter().map(|s| (*s).to_string()).collect(),
            },
        }
    }

    #[tokio::test]
    async fn skill_bundle_content_travels_next_to_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        // Skills live outside the workspace, under the install-wide tree.
        let skills = tempfile::tempdir().unwrap();
        write(&skills.path().join("web_search/SKILL.md"), "# search");
        write(&skills.path().join("web_search/run.sh"), "#!/bin/sh\n");
        write(&skills.path().join("internal_only/SKILL.md"), "# internal");
        write(&skills.path().join(".sync-marker"), "local state");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        plan.skill_sources = vec![skill_source(
            "research_tools",
            skills.path(),
            &["internal_only"],
        )];

        let copied = write_bundle(&mut plan, &out, false).await.unwrap();

        assert_eq!(copied.skills.bundles, 1);
        assert_eq!(copied.skills.tally.files, 2);
        assert!(copied.skills.without_content.is_empty());

        let carried = out.join(SKILLS_DIR).join("research_tools");
        assert!(carried.join("web_search/SKILL.md").is_file());
        assert!(carried.join("web_search/run.sh").is_file());
        // Excluded by the bundle, so it is not the agent's to carry.
        assert!(!carried.join("internal_only").exists());
        // A loose file is local state, not a skill.
        assert!(!carried.join(".sync-marker").exists());
        assert_eq!(entry_names(&carried), vec!["web_search".to_string()]);
    }

    #[tokio::test]
    async fn a_destination_overlapping_a_skill_bundle_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");
        let skills = tempfile::tempdir().unwrap();
        write(&skills.path().join("web_search/SKILL.md"), "# search");

        let mut plan = plan_for(workspace.path());
        plan.skill_sources = vec![skill_source("research_tools", skills.path(), &[])];

        // Inside the bundle's directory: the copy would consume its own output.
        let err = write_bundle(&mut plan, &skills.path().join("exports/bundle"), true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("inside skill bundle"), "{err}");

        // Containing it: publishing would replace the skills being read.
        let err = write_bundle(&mut plan, skills.path(), true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("contains skill bundle"), "{err}");
        assert!(skills.path().join("web_search/SKILL.md").is_file());
    }

    #[tokio::test]
    async fn a_skill_bundle_with_no_content_is_recorded_in_the_manifest_not_advertised() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(
            workspace.path(),
            Path::new("/nonexistent/shared/skills/research_tools"),
            &[],
        );

        let copied = write_bundle(&mut plan, &out, false).await.unwrap();
        assert_eq!(copied.skills.bundles, 0);

        // The manifest and the tree have to agree: no content on disk, and
        // nothing in the artifact claiming otherwise once the terminal that
        // ran the export is gone.
        assert!(!out.join(SKILLS_DIR).exists());
        let manifest = manifest_of(&out);
        assert_eq!(
            manifest_list(&manifest, "skill_bundles"),
            Vec::<String>::new(),
            "{manifest:?}"
        );
        let flags = manifest
            .get("risk_flags")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            !flags
                .iter()
                .any(|f| f.get("kind").and_then(toml::Value::as_str) == Some("carried_skills")),
            "{flags:?}"
        );

        // And the omission is recorded where an importer will read it.
        let dropped = manifest
            .get("dropped")
            .and_then(toml::Value::as_array)
            .unwrap();
        let entry = dropped
            .iter()
            .find(|d| d.get("path").and_then(toml::Value::as_str) == Some("skills/research_tools"))
            .unwrap_or_else(|| panic!("{dropped:?}"));
        assert_eq!(
            entry.get("reason").and_then(toml::Value::as_str),
            Some("source_missing")
        );
    }

    /// The ordinary shape of "no content": config load scaffolds a directory
    /// for every configured bundle, so a bundle nobody has installed skills
    /// into is an empty directory, not a missing one. It must not be
    /// advertised either, and must not leave an empty tree in the artifact.
    #[tokio::test]
    async fn an_empty_skill_bundle_is_not_advertised_and_leaves_no_tree() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");
        // Scaffolded, never populated. Plus one the bundle excludes, so the
        // "everything here is filtered out" case lands in the same place.
        let skills = tempfile::tempdir().unwrap();
        write(&skills.path().join("internal_only/SKILL.md"), "# internal");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(workspace.path(), skills.path(), &["internal_only"]);
        let copied = write_bundle(&mut plan, &out, false).await.unwrap();

        assert_eq!(copied.skills.bundles, 0);
        assert_eq!(
            copied.skills.without_content,
            vec!["research_tools".to_string()]
        );
        assert!(!out.join(SKILLS_DIR).exists());

        let manifest = manifest_of(&out);
        assert_eq!(
            manifest_list(&manifest, "skill_bundles"),
            Vec::<String>::new(),
            "{manifest:?}"
        );
        let dropped = manifest
            .get("dropped")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            dropped.iter().any(|d| {
                d.get("path").and_then(toml::Value::as_str) == Some("skills/research_tools")
                    && d.get("reason").and_then(toml::Value::as_str) == Some("source_missing")
            }),
            "{dropped:?}"
        );
    }

    #[tokio::test]
    async fn a_carried_skill_bundle_is_advertised_and_present() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");
        let skills = tempfile::tempdir().unwrap();
        write(&skills.path().join("web_search/SKILL.md"), "# search");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(workspace.path(), skills.path(), &[]);

        write_bundle(&mut plan, &out, false).await.unwrap();

        // The same two things checked together, in the case where the claim is
        // true: the manifest advertises the alias and the tree backs it up.
        let manifest = manifest_of(&out);
        assert_eq!(
            manifest_list(&manifest, "skill_bundles"),
            vec!["research_tools".to_string()]
        );
        assert!(
            out.join(SKILLS_DIR)
                .join("research_tools/web_search/SKILL.md")
                .is_file()
        );
        let flags = manifest
            .get("risk_flags")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            flags
                .iter()
                .any(|f| f.get("kind").and_then(toml::Value::as_str) == Some("carried_skills")),
            "{flags:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_skill_is_skipped_rather_than_followed() {
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("secret/SKILL.md"), "host secret");

        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        let skills = tempfile::tempdir().unwrap();
        write(&skills.path().join("web_search/SKILL.md"), "# search");
        std::os::unix::fs::symlink(
            outside.path().join("secret"),
            skills.path().join("borrowed"),
        )
        .unwrap();

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        plan.skill_sources = vec![skill_source("research_tools", skills.path(), &[])];

        let copied = write_bundle(&mut plan, &out, false).await.unwrap();

        assert_eq!(copied.skills.tally.files, 1);
        assert_eq!(copied.skills.tally.symlinks_skipped, 1);
        assert!(
            !out.join(SKILLS_DIR)
                .join("research_tools/borrowed")
                .exists()
        );
    }

    #[test]
    fn copy_preserves_the_executable_bit() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let source = tempfile::tempdir().unwrap();
            let dest = tempfile::tempdir().unwrap();
            let script = source.path().join("run.sh");
            write(&script, "#!/bin/sh\n");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

            copy_workspace(&plan_for(source.path()), dest.path()).unwrap();

            let mode = std::fs::metadata(dest.path().join("run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "mode {mode:o}");
        }
    }

    /// The workspace is writable by the agent being exported, so an entry can be
    /// replaced between the moment the copy classifies it and the moment the
    /// copy reads it. The seam reproduces exactly that interleaving.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_replaced_by_an_escaping_symlink_mid_copy_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        write(&secret, "host secret");

        let source = tempfile::tempdir().unwrap();
        let entry = source.path().join("notes.md");
        write(&entry, "workspace note");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(source.path());
        let result = {
            let _swap = EntrySwap::install(move |relative| {
                if relative == Path::new("notes.md") {
                    std::fs::remove_file(&entry).unwrap();
                    std::os::unix::fs::symlink(&secret, &entry).unwrap();
                    // The name now resolves outside the workspace: a copy that
                    // re-opened it by path would read this.
                    assert_eq!(std::fs::read_to_string(&entry).unwrap(), "host secret");
                }
            });
            write_bundle(&mut plan, &out, false).await
        };

        // Fails closed: the bundle is never published, and the host file the
        // symlink pointed at is nowhere in the output.
        let err = result.unwrap_err();
        assert!(err.to_string().contains("workspace/notes.md"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    /// Staying inside the workspace is not the boundary that matters here: the
    /// bundle has content exclusions of its own, and an in-root link would
    /// carry excluded content under an admitted name without ever escaping.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_replaced_by_an_in_root_symlink_to_memory_is_not_followed() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("notes.md"), "workspace note");
        write(
            &source.path().join("memory/brain.db"),
            "SQLite format 3\0the agent's private history",
        );

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let entry = source.path().join("notes.md");
        let mut plan = plan_for(source.path());
        let result = {
            let _swap = EntrySwap::install(move |relative| {
                if relative == Path::new("notes.md") {
                    std::fs::remove_file(&entry).unwrap();
                    // Relative, and squarely inside the workspace root: cap-std's
                    // beneath check has nothing to object to.
                    std::os::unix::fs::symlink("memory/brain.db", &entry).unwrap();
                    assert!(
                        std::fs::read_to_string(&entry)
                            .unwrap()
                            .contains("private history")
                    );
                }
            });
            write_bundle(&mut plan, &out, false).await
        };

        let err = result.unwrap_err();
        assert!(err.to_string().contains("workspace/notes.md"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    /// The same shape one level down: the bundle's own include/exclude filter
    /// is a content boundary, so an admitted skill must not be able to become a
    /// link to an excluded one.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_skill_replaced_by_a_symlink_to_an_excluded_sibling_is_not_followed() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        let skills = tempfile::tempdir().unwrap();
        write(&skills.path().join("web_search/SKILL.md"), "# search");
        write(
            &skills.path().join("internal_only/SKILL.md"),
            "# internal runbook",
        );

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        plan.skill_sources = vec![skill_source(
            "research_tools",
            skills.path(),
            &["internal_only"],
        )];

        let admitted = skills.path().join("web_search");
        let result = {
            let _swap = EntrySwap::install(move |relative| {
                if relative == Path::new("web_search") {
                    std::fs::remove_dir_all(&admitted).unwrap();
                    std::os::unix::fs::symlink("internal_only", &admitted).unwrap();
                    assert!(admitted.join("SKILL.md").is_file());
                }
            });
            write_bundle(&mut plan, &out, false).await
        };

        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("skills/research_tools/web_search"),
            "{err}"
        );
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_directory_replaced_by_an_escaping_symlink_mid_copy_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("secret.txt"), "host secret");

        let source = tempfile::tempdir().unwrap();
        let entry = source.path().join("notes");
        write(&entry.join("plan.md"), "plan");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(source.path());
        let target = outside.path().to_path_buf();
        let result = {
            let _swap = EntrySwap::install(move |relative| {
                if relative == Path::new("notes") {
                    std::fs::remove_dir_all(&entry).unwrap();
                    std::os::unix::fs::symlink(&target, &entry).unwrap();
                    // A copy that re-walked the name by path would descend here.
                    assert!(entry.join("secret.txt").is_file());
                }
            });
            write_bundle(&mut plan, &out, false).await
        };

        let err = result.unwrap_err();
        assert!(err.to_string().contains("workspace/notes"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn export_writes_the_whole_bundle_to_a_fresh_destination() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let copied = write_bundle(&mut plan_for(source.path()), &out, false)
            .await
            .unwrap();

        assert_eq!(copied.workspace.files, 1);
        assert!(out.join(CONFIG_FILE).is_file());
        assert!(out.join(MANIFEST_FILE).is_file());
        assert!(out.join(WORKSPACE_DIR).join("IDENTITY.md").is_file());
        // The staging directory was published, not left beside the bundle.
        assert_eq!(entry_names(parent.path()), vec!["bundle".to_string()]);
    }

    #[tokio::test]
    async fn non_empty_destination_is_refused_without_force_and_left_alone() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");
        write(&out.join("config.toml"), "# existing");

        let err = write_bundle(&mut plan_for(source.path()), &out, false)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("--force"), "{err}");
        assert_eq!(entry_names(&out), vec!["config.toml".to_string()]);
        assert_eq!(
            std::fs::read_to_string(out.join("config.toml")).unwrap(),
            "# existing"
        );
        assert_eq!(entry_names(parent.path()), vec!["bundle".to_string()]);
    }

    #[tokio::test]
    async fn force_publishes_a_replacement_rather_than_merging_into_the_old_bundle() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");
        // A previous export of a different shape: entries the new bundle's
        // manifest does not describe must not survive the re-export.
        write(&out.join(CONFIG_FILE), "# stale closure");
        write(&out.join("leftover.toml"), "# stale");
        write(
            &out.join(WORKSPACE_DIR).join("notes/gone.md"),
            "# stale workspace file",
        );

        let copied = write_bundle(&mut plan_for(source.path()), &out, true)
            .await
            .unwrap();

        assert_eq!(copied.workspace.files, 1);
        // `leftover.toml` is gone: the bundle was replaced, not merged into.
        assert_eq!(
            entry_names(&out),
            vec![
                CONFIG_FILE.to_string(),
                WORKSPACE_DIR.to_string(),
                MANIFEST_FILE.to_string(),
            ]
        );
        assert!(!out.join(WORKSPACE_DIR).join("notes").exists());
        assert!(out.join(WORKSPACE_DIR).join("IDENTITY.md").is_file());
        assert!(
            std::fs::read_to_string(out.join(CONFIG_FILE))
                .unwrap()
                .contains("agent bundle")
        );
        // Neither the staging nor the retired directory outlived the publish.
        assert_eq!(entry_names(parent.path()), vec!["bundle".to_string()]);
    }

    /// Root ignores mode bits, so probe rather than assume the permission-denied
    /// case is reachable here.
    #[cfg(unix)]
    fn read_permissions_are_enforced(dir: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let probe = dir.join("probe");
        std::fs::write(&probe, "probe").unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = std::fs::read(&probe).is_err();
        std::fs::remove_file(&probe).ok();
        denied
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_export_removes_its_staging_and_leaves_the_previous_bundle() {
        use std::os::unix::fs::PermissionsExt;

        let probe_dir = tempfile::tempdir().unwrap();
        if !read_permissions_are_enforced(probe_dir.path()) {
            return; // running as root: an unreadable file is still readable
        }

        // A workspace file that cannot be read fails the copy after the config
        // and manifest have already been staged.
        let source = tempfile::tempdir().unwrap();
        let locked = source.path().join("locked.md");
        write(&locked, "unreadable");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");
        write(&out.join(CONFIG_FILE), "# previous export");

        let err = write_bundle(&mut plan_for(source.path()), &out, true)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("workspace/locked.md"), "{err}");
        assert_eq!(entry_names(&out), vec![CONFIG_FILE.to_string()]);
        assert_eq!(
            std::fs::read_to_string(out.join(CONFIG_FILE)).unwrap(),
            "# previous export"
        );
        assert_eq!(entry_names(parent.path()), vec!["bundle".to_string()]);
    }

    #[tokio::test]
    async fn destination_inside_the_workspace_is_refused_before_anything_is_written() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");
        let out = source.path().join("exports/bundle");

        let err = write_bundle(&mut plan_for(source.path()), &out, true)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("inside the agent workspace"),
            "{err}"
        );
        assert_eq!(entry_names(source.path()), vec!["IDENTITY.md".to_string()]);
    }

    #[tokio::test]
    async fn destination_containing_the_workspace_is_refused_before_anything_is_written() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("agents/researcher/workspace");
        write(&workspace.join("IDENTITY.md"), "identity");

        let err = write_bundle(&mut plan_for(&workspace), root.path(), true)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("contains the agent workspace"),
            "{err}"
        );
        assert_eq!(entry_names(root.path()), vec!["agents".to_string()]);
        assert!(workspace.join("IDENTITY.md").is_file());
    }

    #[tokio::test]
    async fn destination_equal_to_the_workspace_is_refused() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let err = write_bundle(&mut plan_for(source.path()), source.path(), true)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("contains the agent workspace"),
            "{err}"
        );
        assert!(source.path().join("IDENTITY.md").is_file());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_destination_cannot_hide_an_overlap() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        // `link` resolves into the workspace, so the overlap is only visible
        // once both sides are resolved.
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("link");
        std::os::unix::fs::symlink(source.path(), &link).unwrap();

        let err = write_bundle(&mut plan_for(source.path()), &link.join("bundle"), true)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("inside the agent workspace"),
            "{err}"
        );
        assert_eq!(entry_names(source.path()), vec!["IDENTITY.md".to_string()]);
    }

    #[tokio::test]
    async fn a_file_destination_is_refused() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");
        write(&out, "not a directory");

        let err = write_bundle(&mut plan_for(source.path()), &out, true)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("not a directory"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "not a directory".to_string()
        );
    }
}
