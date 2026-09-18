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

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
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

/// Install subtree that owns skill-bundle content. Matches the directory
/// contract `zeroclaw_config::skill_bundles::validate_directory` enforces.
const SKILLS_FAMILY: &str = "shared";

/// Install subtree that owns default-location agent workspaces.
const WORKSPACE_FAMILY: &str = "agents";

/// One filesystem object's identity.
///
/// `dev`/`ino` are the portable identity pair `cap-fs-ext` exposes: on Windows
/// they are the volume serial and file index, and cap-primitives builds every
/// view from an opened handle, so neither side of a comparison is a by-name
/// guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectId {
    dev: u64,
    ino: u64,
}

impl ObjectId {
    fn of(metadata: &cap_std::fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
}

/// The destination state the operator's `--force` decision was made against.
///
/// The decision and the publication are separated by the whole copy, which is
/// as long as the export takes. Carrying the decision forward as a value —
/// including the identity of the object it was made against — is what stops
/// publication from re-deciding against whatever the destination holds by
/// then. A directory that appears mid-copy was admitted by nobody, and a
/// `--force` given for one tree is not a `--force` for the tree that replaced
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    /// Nothing was at the destination. Publication may create it, and may
    /// replace nothing.
    Vacant,
    /// An empty directory was there. Replacing it needed no `--force` because
    /// there was nothing to lose, which stays true only while it is that same
    /// object and still empty.
    Empty(ObjectId),
    /// A non-empty directory the operator admitted for replacement with
    /// `--force`. Publication retires that object and no other.
    Forced(ObjectId),
}

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
    /// Files carrying more than one name, skipped rather than copied. A hard
    /// link is a second name for an object that may live anywhere on the host,
    /// so its bytes are not this tree's to carry.
    hard_links_skipped: usize,
    /// Entries whose object changed between being classified and being opened.
    /// The copy carries what it inspected or nothing at all.
    replaced_skipped: usize,
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

    let (Some(parent_path), Some(name)) = (dest.parent(), dest.file_name()) else {
        bail!(
            "{}",
            mta(
                "cli-agent-export-dest-no-parent",
                &[("path", out.display().to_string().as_str())],
                "destination {$path} has no parent directory to stage the bundle beside"
            )
        );
    };
    // Creating the parent before the destination is admitted costs nothing a
    // refusal would have to undo: every refusal in `check_destination` needs
    // an object *at* the destination, which needs the parent to exist already.
    tokio::fs::create_dir_all(parent_path)
        .await
        .with_context(|| {
            format!(
                "failed to create destination parent {}",
                parent_path.display()
            )
        })?;
    // Opened once, and every later step that touches the destination goes
    // through it: the occupancy decision, both publishing renames, and the
    // reap. A parent replaced mid-export therefore cannot redirect the
    // publish to a directory the operator never named — the handle still
    // refers to the directory that was admitted, whatever now wears its name.
    let parent = Dir::open_ambient_dir(parent_path, ambient_authority())
        .with_context(|| format!("failed to open {}", parent_path.display()))?;

    let admission = check_destination(&parent, name, out, force)?;

    // Dropping the staging directory removes it, so every `?` below cleans up
    // after itself and leaves the destination untouched. A crash skips the
    // drop, leaving the staged tree behind for the operator to delete.
    //
    // Created by path rather than through the handle above, which is the one
    // step that cannot be: a parent replaced in the instant between the two
    // leaves the staged tree invisible to the handle, and publication then
    // fails to find it. That is a failed export, not a misdirected one.
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(parent_path)
        .with_context(|| {
            format!(
                "failed to create a staging directory for the bundle in {}",
                parent_path.display()
            )
        })?;

    // Identities of the trees the copy opened. Publication may retire the
    // object at the destination, and must never retire one of these:
    // `reject_source_overlap` answers that question by path before the copy,
    // where a rename during the copy can make the answer stale.
    let mut sources = Vec::new();
    let workspace_dest = staging.path().join(WORKSPACE_DIR);
    let mut copied = BundleCopy {
        workspace: copy_workspace(plan, &workspace_dest, &mut sources)?,
        skills: SkillCopy::default(),
    };
    copy_skill_bundles(
        plan,
        &staging.path().join(SKILLS_DIR),
        &mut copied,
        &mut sources,
    )?;

    // The closure may reference an identity document inside the workspace. The
    // planner proved the path stays in the tree; only the copy knows whether
    // the file was actually there to carry.
    if let Some(relative) = plan.identity_document.as_deref()
        && !workspace_dest.join(relative).is_file()
    {
        agent_bundle::record_missing_identity_document(plan);
    }

    // Both files describe the bundle, so both are rendered from what the copy
    // carried and written once it is done.
    let config_toml = agent_bundle::render_config_toml(plan).map_err(anyhow::Error::new)?;
    write_file(&staging.path().join(CONFIG_FILE), &config_toml).await?;

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

    publish(staging, &parent, name, &dest, admission, &sources)?;
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
            // A `..` below the nearest existing ancestor: `file_name` is
            // `None` while ancestors remain. The filesystem will resolve it
            // only after `create_dir_all` brings the missing component into
            // being, so no overlap or occupancy check made now describes the
            // path that would actually be written. Refused: a destination
            // like `missing/../<workspace>` would otherwise slip past
            // [`reject_source_overlap`] and publish over the workspace it
            // reads.
            (None, Some(_)) => bail!(
                "{}",
                mta(
                    "cli-agent-export-path-unresolvable",
                    &[("path", path.display().to_string().as_str())],
                    "{$path} reaches through `..` inside a directory that does not exist \
                     yet, so what it names cannot be checked before the export writes; \
                     write the path without `..`"
                )
            ),
            // No ancestor exists at all (an absolute path under a missing
            // root); nothing can be canonicalized, so compare it as written.
            (_, None) => return Ok(absolute),
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

/// Decide whether the bundle may be published over what is at the
/// destination, and bind that decision to the object it was made against.
///
/// A non-directory is refused outright; a directory that already holds files
/// needs `--force`, which replaces its contents rather than merging into them.
///
/// The destination is inspected through a handle on its parent, so the answer
/// describes one object rather than one name. A symlink is refused rather
/// than followed — publishing through one would replace whatever it points
/// at, which is not what the operator named — and an object swapped between
/// the classification and the open fails the export instead of being admitted
/// on the strength of a decision made about something else.
fn check_destination(parent: &Dir, name: &OsStr, out: &Path, force: bool) -> Result<Admission> {
    let metadata = match parent.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Admission::Vacant);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect destination {}", out.display()));
        }
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "{}",
            mta(
                "cli-agent-export-dest-symlink",
                &[("path", out.display().to_string().as_str())],
                "destination {$path} is a symlink; publishing would replace whatever it \
                 points at rather than the path you named, so name the directory itself"
            )
        );
    }
    if !metadata.is_dir() {
        bail!(
            "{}",
            mta(
                "cli-agent-export-dest-not-a-dir",
                &[("path", out.display().to_string().as_str())],
                "destination {$path} exists and is not a directory"
            )
        );
    }
    let opened = parent
        .open_dir_nofollow(name)
        .with_context(|| format!("failed to open destination {}", out.display()))?;
    let opened_metadata = opened
        .dir_metadata()
        .with_context(|| format!("failed to inspect destination {}", out.display()))?;
    if !is_same_object(&metadata, &opened_metadata) {
        bail!("{}", destination_changed(out));
    }
    let admitted = ObjectId::of(&opened_metadata);
    let occupied = opened
        .entries()
        .with_context(|| format!("failed to read destination {}", out.display()))?
        .next()
        .is_some();
    if !occupied {
        return Ok(Admission::Empty(admitted));
    }
    if !force {
        bail!(
            "{}",
            mta(
                "cli-agent-export-dest-not-empty",
                &[("path", out.display().to_string().as_str())],
                "destination {$path} is not empty — pass --force to replace its contents"
            )
        );
    }
    Ok(Admission::Forced(admitted))
}

/// The refusal for a destination that is no longer the object the export
/// checked. Shared by the admission and by every publication step, because
/// they are all saying the same thing: nothing was replaced.
fn destination_changed(out: &Path) -> String {
    mta(
        "cli-agent-export-dest-changed",
        &[("path", out.display().to_string().as_str())],
        "destination {$path} is not the directory this export checked before copying; \
         nothing was replaced, so look at what is there and export again",
    )
}

/// The identity of the directory at `name`, opened through `parent` without
/// following a link.
///
/// `None` covers every way the name can fail to be a directory this export
/// could have admitted: absent, a symlink, not a directory, or replaced
/// between being classified and being opened. Callers compare against what
/// they admitted, so all of those are one answer — not this object.
fn destination_identity(parent: &Dir, name: &OsStr, out: &Path) -> Result<Option<ObjectId>> {
    let metadata = match parent.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", out.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(None);
    }
    let opened = match parent.open_dir_nofollow(name) {
        Ok(opened) => opened,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open {}", out.display()));
        }
    };
    let opened = opened
        .dir_metadata()
        .with_context(|| format!("failed to inspect {}", out.display()))?;
    if !is_same_object(&metadata, &opened) {
        return Ok(None);
    }
    Ok(Some(ObjectId::of(&opened)))
}

/// The identity of an opened source root, recorded so publication can refuse
/// to retire a tree this export read.
fn source_identity(dir: &Dir, path: &Path) -> Result<ObjectId> {
    let metadata = dir
        .dir_metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    Ok(ObjectId::of(&metadata))
}

/// Swap the staged bundle into place, bound to what the operator admitted.
fn publish(
    staging: tempfile::TempDir,
    parent: &Dir,
    name: &OsStr,
    dest: &Path,
    admission: Admission,
    sources: &[ObjectId],
) -> Result<()> {
    let Some(staged) = staging.path().file_name() else {
        bail!("the staging directory has no name to publish from");
    };
    let staged = staged.to_os_string();
    // On failure the guard drops and removes the staged tree. On success the
    // rename has already moved it, so the guard is disarmed rather than left to
    // recurse over a path that is now the published bundle.
    swap_into_place(parent, &staged, name, dest, admission, sources)?;
    let _ = staging.keep();
    Ok(())
}

/// Move the staged bundle onto the destination, replacing only the object the
/// operator admitted for replacement.
///
/// Every step runs through `parent`, the handle the admission was made
/// through, so neither a replaced parent nor a replaced destination can move
/// the publish somewhere the operator did not name.
///
/// An admitted bundle is moved aside rather than deleted first, so a failed
/// move can put it back. The window between the two renames is real: a crash
/// inside it leaves the destination absent and the previous bundle under the
/// retired name, which is why that name is derived from the staging token
/// rather than being random on its own. Recovery is renaming it back.
///
/// The recursive delete at the end is the one irreversible step here, so it
/// runs only against an object proven twice to be the admitted one: once
/// before the retiring rename, and once through the retired name afterwards.
/// A rename moves a name, not necessarily the object that wore it when the
/// decision to move it was taken.
fn swap_into_place(
    parent: &Dir,
    staged: &OsStr,
    name: &OsStr,
    dest: &Path,
    admission: Admission,
    sources: &[ObjectId],
) -> Result<()> {
    publish_seam(dest);
    let admitted = match admission {
        Admission::Vacant => {
            // Nothing was admitted for replacement, so publication only
            // creates. The probe is advisory; the rename is the enforcement,
            // and it cannot replace a non-empty directory or a non-directory
            // on any supported platform. What it can still replace on Unix is
            // an empty directory that appeared meanwhile, which loses nothing
            // and retires nothing.
            if parent.symlink_metadata(name).is_ok() {
                bail!(
                    "{}",
                    mta(
                        "cli-agent-export-dest-appeared",
                        &[("path", dest.display().to_string().as_str())],
                        "destination {$path} did not exist when the export started and does \
                         now; replacing it was never admitted, so nothing was written"
                    )
                );
            }
            return parent.rename(staged, parent, name).with_context(|| {
                format!("failed to move the staged bundle into {}", dest.display())
            });
        }
        Admission::Empty(admitted) | Admission::Forced(admitted) => admitted,
    };

    let current = destination_identity(parent, name, dest)?;
    if current.is_some_and(|current| sources.contains(&current)) {
        // Named before the identity comparison below, which would refuse this
        // too but could only say the destination changed. It changed into a
        // tree this export just read, and publishing would delete it.
        bail!(
            "{}",
            mta(
                "cli-agent-export-dest-is-source",
                &[("path", dest.display().to_string().as_str())],
                "destination {$path} is now one of the trees this export read; publishing \
                 would replace the source it just copied, so nothing was written"
            )
        );
    }
    if current != Some(admitted) {
        bail!("{}", destination_changed(dest));
    }

    if matches!(admission, Admission::Empty(_)) {
        // An empty destination was replaceable without `--force` because
        // there was nothing to lose. `remove_dir` is what keeps that true:
        // the kernel refuses a directory that has gained entries since, and
        // no `--force` was ever given for them.
        parent
            .remove_dir(name)
            .with_context(|| format!("failed to clear the empty destination {}", dest.display()))?;
        return match parent.rename(staged, parent, name) {
            Ok(()) => Ok(()),
            Err(error) => {
                // Put the empty directory back, so a failed export leaves the
                // destination as it found it.
                parent.create_dir(name).ok();
                Err(error).with_context(|| {
                    format!("failed to move the staged bundle into {}", dest.display())
                })
            }
        };
    }

    let retired = retired_name(staged);
    if parent.symlink_metadata(&retired).is_ok() {
        // A leftover from a run that died mid-publish, wearing the same token.
        // Refuse rather than consume it: the caller cleans up the staged tree
        // and the destination is still the bundle it was.
        bail!("cannot retire the existing bundle: {retired} already exists beside the bundle");
    }
    parent.rename(name, parent, &retired).with_context(|| {
        format!(
            "failed to move the existing bundle at {} aside",
            dest.display()
        )
    })?;
    if destination_identity(parent, OsStr::new(&retired), dest)? != Some(admitted) {
        // The name moved, but not the object the admission was made against.
        // Put it back and refuse rather than point a recursive delete at
        // something nobody admitted.
        parent.rename(&retired, parent, name).ok();
        bail!("{}", destination_changed(dest));
    }
    match parent.rename(staged, parent, name) {
        Ok(()) => {
            // The new bundle is published; the old one is now unreferenced.
            // Failing to reap it is untidy, not a failed export.
            parent.remove_dir_all(&retired).ok();
            Ok(())
        }
        Err(err) => {
            if parent.rename(&retired, parent, name).is_ok() {
                return Err(err).with_context(|| {
                    format!("failed to move the staged bundle into {}", dest.display())
                });
            }
            let dest_display = dest.display().to_string();
            let error = err.to_string();
            bail!(
                "{}",
                mta(
                    "cli-agent-export-restore-failed",
                    &[
                        ("path", dest_display.as_str()),
                        ("retired", retired.as_str()),
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

/// What opening a copy source's root found.
enum SourceRoot {
    /// A real directory, opened without traversing a link at its final
    /// component.
    Opened(Dir),
    /// Nothing to copy: the path is absent, or is not a directory.
    Nothing,
    /// The final component is a symlink. Refused rather than followed.
    Symlinked,
    /// A component between the trusted install root and the source is a
    /// symlink, or the declared path leaves the subtree its family owns.
    /// Refused: the source cannot be reached through real directories inside
    /// the tree its contract names.
    Escaped { at: PathBuf },
}

/// Open an anchored source root by walking `relative` from the install root,
/// opening every component without following a link at any of them.
///
/// The walk is the boundary check. A canonicalize-then-compare answer proves
/// nothing about the object a later by-path open finds, and comparing against
/// the install root alone admits the in-install redirect: a symlinked
/// `shared/skills` or `agents` that points elsewhere *inside* the install
/// passes the comparison while moving the source out of the subtree its
/// family's contract names. Opening `shared`, then `skills`, then the bundle
/// directory — each through the previous handle, each refusing a link — is a
/// proof about the handle the copy then reads.
///
/// The install root itself is the operator's configured location, so symlinks
/// above it (a relocated home directory, `/tmp` on macOS) are the operator's
/// own and are followed, exactly like the config file that names the install.
///
/// Each component's `symlink_metadata` only picks which answer to give. The
/// no-follow open is what enforces it, and the opened handle must then be the
/// object that was classified, compared by filesystem identity: a component
/// replaced between the two steps — by a link or by another directory — fails
/// the export instead of redirecting it.
///
/// Mount manipulation is outside this defense. A bind mount planted inside
/// the subtree reads as an ordinary directory here; creating one takes mount
/// privileges, which are beyond what an export boundary can defend against.
fn open_anchored_root(install_root: &Path, relative: &Path, family: &str) -> Result<SourceRoot> {
    if !relative.starts_with(family) {
        // The planner binds each family to its subtree (`shared/` for skill
        // bundles, `agents/` for default workspaces); a path outside it never
        // reaches an open. This is the boundary that does not depend on the
        // planner having done so.
        return Ok(SourceRoot::Escaped {
            at: install_root.join(relative),
        });
    }
    let root = install_root
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", install_root.display()))?;
    let mut dir = Dir::open_ambient_dir(&root, ambient_authority())
        .with_context(|| format!("failed to open {}", root.display()))?;
    let mut walked = install_root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let std::path::Component::Normal(name) = component else {
            // `..`, `.`, or a new root would re-anchor the walk; the planner
            // normalizes these away, and one that appears anyway is refused.
            return Ok(SourceRoot::Escaped {
                at: install_root.join(relative),
            });
        };
        walked.push(name);
        let metadata = match dir.symlink_metadata(name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Only an absent component means there is no source. Any
                // other failure aborts: publishing a bundle that silently
                // lacks content the operator could not read is a success the
                // manifest cannot stand behind.
                return Ok(SourceRoot::Nothing);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", walked.display()));
            }
        };
        let last = components.peek().is_none();
        if metadata.file_type().is_symlink() {
            return Ok(if last {
                SourceRoot::Symlinked
            } else {
                SourceRoot::Escaped { at: walked }
            });
        }
        if !metadata.is_dir() {
            // Exists but is not a directory. Only absence means there is no
            // source: a file squatting a component of the install's own tree
            // is a broken install, and exporting past it would publish a
            // bundle silently missing what the manifest claims to describe.
            bail!(
                "{}",
                mta(
                    "cli-agent-export-source-not-a-directory",
                    &[("path", walked.display().to_string().as_str())],
                    "{$path} exists but is not a directory; the export refuses to publish a \
                     bundle that silently lacks the source it names"
                )
            );
        }
        source_root_swap_seam(&walked);
        let opened = dir
            .open_dir_nofollow(name)
            .with_context(|| format!("failed to open {}", walked.display()))?;
        let opened_metadata = opened
            .dir_metadata()
            .with_context(|| format!("failed to inspect {}", walked.display()))?;
        if !is_same_object(&metadata, &opened_metadata) {
            bail!(
                "{}",
                mta(
                    "cli-agent-export-source-root-replaced",
                    &[("path", walked.display().to_string().as_str())],
                    "{$path} was replaced while the export was opening it; the copy carries \
                     the tree it inspected or nothing at all, so run the export again"
                )
            );
        }
        dir = opened;
    }
    Ok(SourceRoot::Opened(dir))
}

/// Open an operator-configured source root, refusing a symlink at its final
/// component.
///
/// A configured `workspace.path` is the operator's own boundary, so its
/// ancestors are followed as configured — the same trust the config file's
/// own location gets. The last component never is: it is classified and
/// opened through a handle on its canonicalized parent, where a link is an
/// error instead of a redirect. Canonicalizing the *full* path here would
/// undo the classification — a root swapped for a link between the two steps
/// would be resolved silently, and the no-follow open would then vouch for
/// the link's target as if it were the configured tree.
fn open_configured_root(path: &Path) -> Result<SourceRoot> {
    // Judged on the configured path as written, before any resolution: on
    // Windows `std::path::absolute` collapses a trailing `..` lexically, so
    // checking afterwards would let such a path export whatever the collapse
    // lands on. A filesystem root, or a path ending in `..`, has no final
    // component to classify and open. Refused rather than treated as absent:
    // the runtime may resolve such a path to a real workspace, and an export
    // that silently publishes an empty one instead is a success the manifest
    // cannot stand behind.
    if !matches!(
        path.components().next_back(),
        Some(std::path::Component::Normal(_))
    ) {
        bail!(
            "{}",
            mta(
                "cli-agent-export-workspace-path-unresolvable",
                &[("path", path.display().to_string().as_str())],
                "the configured workspace path {$path} does not end in a plain directory \
                 name, so the export cannot bind what it copies to what it checked; set \
                 `workspace.path` to the resolved directory and export again"
            )
        );
    }
    let absolute = std::path::absolute(path)
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
        // Unreachable after the check above; kept as the same refusal so a
        // platform surprise fails closed rather than publishing.
        bail!(
            "{}",
            mta(
                "cli-agent-export-workspace-path-unresolvable",
                &[("path", path.display().to_string().as_str())],
                "the configured workspace path {$path} does not end in a plain directory \
                 name, so the export cannot bind what it copies to what it checked; set \
                 `workspace.path` to the resolved directory and export again"
            )
        );
    };
    let parent = match parent.canonicalize() {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SourceRoot::Nothing);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to resolve {}", parent.display()));
        }
    };
    let parent = Dir::open_ambient_dir(&parent, ambient_authority())
        .with_context(|| format!("failed to open {}", parent.display()))?;
    let metadata = match parent.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SourceRoot::Nothing);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() {
        return Ok(SourceRoot::Symlinked);
    }
    if !metadata.is_dir() {
        // Same rule as the anchored walk: only absence means "no source".
        bail!(
            "{}",
            mta(
                "cli-agent-export-source-not-a-directory",
                &[("path", path.display().to_string().as_str())],
                "{$path} exists but is not a directory; the export refuses to publish a \
                 bundle that silently lacks the source it names"
            )
        );
    }
    source_root_swap_seam(&absolute);
    let opened = parent
        .open_dir_nofollow(name)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let opened_metadata = opened
        .dir_metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !is_same_object(&metadata, &opened_metadata) {
        bail!(
            "{}",
            mta(
                "cli-agent-export-source-root-replaced",
                &[("path", path.display().to_string().as_str())],
                "{$path} was replaced while the export was opening it; the copy carries \
                 the tree it inspected or nothing at all, so run the export again"
            )
        );
    }
    Ok(SourceRoot::Opened(opened))
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
fn copy_workspace(
    plan: &ExportPlan,
    dest: &Path,
    sources: &mut Vec<ObjectId>,
) -> Result<CopyTally> {
    let mut copied = CopyTally::default();
    let opened = match plan.source_boundaries.workspace.as_deref() {
        Some(relative) => open_anchored_root(
            &plan.source_boundaries.install_root,
            relative,
            WORKSPACE_FAMILY,
        )?,
        None => open_configured_root(&plan.workspace_source)?,
    };
    let source = match opened {
        SourceRoot::Opened(dir) => dir,
        SourceRoot::Nothing => return Ok(copied),
        SourceRoot::Escaped { at } => bail!(
            "{}",
            mta(
                "cli-agent-export-workspace-root-escape",
                &[
                    ("path", plan.workspace_source.display().to_string().as_str()),
                    ("at", at.display().to_string().as_str())
                ],
                "the agent workspace {$path} is not reachable through real directories under \
                 the install's agents tree: {$at} is a symlink or leaves that tree, so the \
                 copy cannot prove what it would carry"
            )
        ),
        SourceRoot::Symlinked => bail!(
            "{}",
            mta(
                "cli-agent-export-workspace-root-symlink",
                &[("path", plan.workspace_source.display().to_string().as_str())],
                "the agent workspace {$path} is a symlink; the bundle would carry whatever it \
                 points at as the agent's own tree, so set `workspace.path` to the real \
                 directory and export again"
            )
        ),
    };
    sources.push(source_identity(&source, &plan.workspace_source)?);
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
fn copy_skill_bundles(
    plan: &ExportPlan,
    dest_root: &Path,
    into: &mut BundleCopy,
    sources: &mut Vec<ObjectId>,
) -> Result<()> {
    if plan.skill_sources.is_empty() {
        return Ok(());
    }
    // Every bundle directory is created and opened *through* this handle, so an
    // alias that is not a single component cannot place content outside the
    // staging tree even if it reaches here. The planner rejects such aliases;
    // this is the boundary that does not depend on it having done so.
    let skills_root = open_bundle_dir(dest_root)?;
    for source in &plan.skill_sources {
        let bundle = match open_anchored_root(
            &plan.source_boundaries.install_root,
            &source.relative,
            SKILLS_FAMILY,
        )? {
            SourceRoot::Opened(dir) => dir,
            SourceRoot::Nothing => {
                into.skills.without_content.push(source.alias.clone());
                continue;
            }
            SourceRoot::Escaped { at } => bail!(
                "{}",
                mta(
                    "cli-agent-export-skill-root-escape",
                    &[
                        ("alias", source.alias.as_str()),
                        ("path", source.source.display().to_string().as_str()),
                        ("at", at.display().to_string().as_str())
                    ],
                    "skill bundle `{$alias}` at {$path} is not reachable through real \
                     directories under the install's shared tree: {$at} is a symlink or \
                     leaves that tree, so the copy cannot prove what it would carry"
                )
            ),
            SourceRoot::Symlinked => bail!(
                "{}",
                mta(
                    "cli-agent-export-skill-root-symlink",
                    &[
                        ("alias", source.alias.as_str()),
                        ("path", source.source.display().to_string().as_str())
                    ],
                    "skill bundle `{$alias}` resolves to the symlink {$path}; a bundle \
                     directory must be a real directory inside the install's shared tree"
                )
            ),
        };
        sources.push(source_identity(&bundle, &source.source)?);
        skills_root.create_dir(&source.alias).with_context(|| {
            format!(
                "failed to create {SKILLS_DIR}/{} in the bundle",
                source.alias
            )
        })?;
        let target = skills_root
            .open_dir_nofollow(&source.alias)
            .with_context(|| {
                format!("failed to open {SKILLS_DIR}/{} in the bundle", source.alias)
            })?;
        let carried = copy_skills(&bundle, &target, source, &mut into.skills.tally)?;
        if carried == 0 {
            // An empty tree is not content. Leave neither the directory nor
            // the manifest claim behind for it.
            drop(target);
            skills_root.remove_dir_all(&source.alias).ok();
            into.skills.without_content.push(source.alias.clone());
            continue;
        }
        into.skills.bundles += 1;
    }
    if into.skills.bundles == 0 {
        // Nothing was carried, so the bundle gets no `skills/` at all rather
        // than an empty directory implying otherwise.
        drop(skills_root);
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
        let classified = entry
            .metadata()
            .with_context(|| format!("failed to stat {root}/{skill}"))?;
        let file_type = classified.file_type();
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
        // The no-follow open proves the name is not a link; it does not prove
        // *which* directory now wears it. An admitted skill renamed away and
        // an excluded sibling renamed into its place is still a real
        // directory, so the opened handle must be the object that was
        // classified — the same rule the per-entry copy applies below.
        let opened = child_source
            .dir_metadata()
            .with_context(|| format!("failed to stat {root}/{skill}"))?;
        if !is_same_object(&classified, &opened) {
            copied.replaced_skipped += 1;
            continue;
        }
        dest.create_dir(&name)
            .with_context(|| format!("failed to create {root}/{skill} in the bundle"))?;
        let child_dest = dest
            .open_dir_nofollow(&name)
            .with_context(|| format!("failed to open {root}/{skill} in the bundle"))?;
        let spec = CopySpec {
            root: root.clone(),
            filter: &|_| true,
        };
        // Count skills by what the copy actually wrote, not by how many
        // directories were admitted. A skill whose every entry is skipped
        // (symlinks, special files, hard links) leaves nothing behind, and a
        // bundle must not advertise a capability it has no content for.
        let before = copied.files;
        copy_tree(&child_source, &child_dest, &spec, Path::new(skill), copied)?;
        if copied.files > before {
            skills += 1;
        } else {
            dest.remove_dir_all(&name).with_context(|| {
                format!("failed to remove the empty {root}/{skill} from the bundle")
            })?;
        }
    }
    Ok(skills)
}

/// Name to move the existing bundle aside under.
///
/// The staging directory's name was allocated uniquely in this parent, so
/// reusing its random token keeps the retired name unique too, without the
/// exporter carrying a random-number dependency of its own.
fn retired_name(staged: &OsStr) -> String {
    let token = staged.to_string_lossy();
    let token = token.strip_prefix(STAGING_PREFIX).unwrap_or(&token);
    format!("{RETIRED_PREFIX}{token}")
}

/// Whether two metadata views describe one filesystem object.
fn is_same_object(classified: &cap_std::fs::Metadata, opened: &cap_std::fs::Metadata) -> bool {
    ObjectId::of(classified) == ObjectId::of(opened)
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
        let classified = entry
            .metadata()
            .with_context(|| format!("failed to stat {}", rel(spec, &child)))?;
        let file_type = classified.file_type();
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
            let opened = child_source
                .dir_metadata()
                .with_context(|| format!("failed to stat {}", rel(spec, &child)))?;
            if !is_same_object(&classified, &opened) {
                copied.replaced_skipped += 1;
                continue;
            }
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
            // No-follow proves the name is not a symlink. It does not prove the
            // bytes behind it belong to this tree: a hard link is a second name
            // for one object, so an entry swapped for a link to a host file
            // still classifies and opens as an ordinary regular file. A link
            // count above one is that second name, checked on the handle the
            // copy is about to read.
            if source_metadata.nlink() > 1 {
                copied.hard_links_skipped += 1;
                continue;
            }
            // Refusing to follow a name says nothing about *which* object now
            // wears it. Between the classification above and this open, the
            // entry can be unlinked and a host file renamed into its place:
            // still a regular file, still one link, still inside the directory
            // handle. The copy carries the object it inspected, so the two
            // views have to name one object.
            if !is_same_object(&classified, &source_metadata) {
                copied.replaced_skipped += 1;
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

/// Test seam: runs between a walked component's no-follow classification and
/// the handle-bound open of that component — the interleaving at which an
/// ancestor replacement could try to turn an admitted directory into a
/// redirect. Compiled away outside tests.
#[cfg(not(test))]
#[inline]
fn source_root_swap_seam(_walked: &Path) {}

#[cfg(test)]
fn source_root_swap_seam(walked: &Path) {
    tests::run_source_root_swap_seam(walked);
}

/// Test seam: runs once the staged tree is complete, immediately before
/// publication binds itself to the admitted destination — the interleaving at
/// which the destination can be created, replaced, or moved out from under
/// the admission that was made about it. Compiled away outside tests.
#[cfg(not(test))]
#[inline]
fn publish_seam(_dest: &Path) {}

#[cfg(test)]
fn publish_seam(dest: &Path) {
    tests::run_publish_seam(dest);
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
    let replaced_skipped = copied.workspace.replaced_skipped + copied.skills.tally.replaced_skipped;
    if replaced_skipped > 0 {
        let count = replaced_skipped.to_string();
        println!(
            "{}",
            mta(
                "cli-agent-export-replaced-skipped",
                &[("count", count.as_str())],
                "  {$count} entry/entries were replaced while the export ran and were skipped — \
                 the bundle carries the objects it inspected"
            )
        );
    }

    let hard_links_skipped =
        copied.workspace.hard_links_skipped + copied.skills.tally.hard_links_skipped;
    if hard_links_skipped > 0 {
        let count = hard_links_skipped.to_string();
        println!(
            "{}",
            mta(
                "cli-agent-export-hard-links-skipped",
                &[("count", count.as_str())],
                "  {$count} hard-linked file(s) skipped — a second name for a file that may \
                 live anywhere on this host is not this workspace's content to carry"
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

    /// A closure whose agent entry names an identity document.
    ///
    /// Minimal, but not *below* minimal: the rendered config is proved to
    /// validate on a clean install before it is written, and an agent entry
    /// with no model provider or risk profile is not a config anyone could
    /// import. Both are carried here so these fixtures exercise the identity
    /// handling rather than the closure gate.
    fn agent_identity_config(path: &str) -> toml::Table {
        let mut identity = toml::Table::new();
        identity.insert(
            "aieos_path".to_string(),
            toml::Value::String(path.to_string()),
        );
        let mut agent = toml::Table::new();
        agent.insert("identity".to_string(), toml::Value::Table(identity));
        agent.insert(
            "model_provider".to_string(),
            toml::Value::String("anthropic.main".to_string()),
        );
        agent.insert(
            "risk_profile".to_string(),
            toml::Value::String("guarded".to_string()),
        );
        let mut agents = toml::Table::new();
        agents.insert("researcher".to_string(), toml::Value::Table(agent));

        let mut anthropic = toml::Table::new();
        anthropic.insert("main".to_string(), toml::Value::Table(toml::Table::new()));
        let mut models = toml::Table::new();
        models.insert("anthropic".to_string(), toml::Value::Table(anthropic));
        let mut providers = toml::Table::new();
        providers.insert("models".to_string(), toml::Value::Table(models));

        let mut risk_profiles = toml::Table::new();
        risk_profiles.insert(
            "guarded".to_string(),
            toml::Value::Table(toml::Table::new()),
        );

        let mut root = toml::Table::new();
        root.insert("agents".to_string(), toml::Value::Table(agents));
        root.insert("providers".to_string(), toml::Value::Table(providers));
        root.insert(
            "risk_profiles".to_string(),
            toml::Value::Table(risk_profiles),
        );
        root
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
            // Tests point the workspace at a temporary directory, which is
            // the "operator chose this location" shape: no install to anchor
            // to. Skill fixtures re-anchor via `bound_skills_to`.
            source_boundaries: zeroclaw_config::agent_bundle::SourceBoundaries {
                install_root: workspace.to_path_buf(),
                workspace: None,
            },
            identity_document: None,
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

    thread_local! {
        /// Swap to perform at [`source_root_swap_seam`], keyed on the walked
        /// absolute path. Thread-local like [`ENTRY_SWAP`].
        static SOURCE_ROOT_SWAP: std::cell::RefCell<Option<Swap>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn run_source_root_swap_seam(walked: &Path) {
        SOURCE_ROOT_SWAP.with_borrow(|swap| {
            if let Some(swap) = swap.as_ref() {
                swap(walked);
            }
        });
    }

    thread_local! {
        /// Swap to perform at [`publish_seam`], keyed on the destination.
        /// Thread-local like [`ENTRY_SWAP`].
        static PUBLISH_SWAP: std::cell::RefCell<Option<Swap>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn run_publish_seam(dest: &Path) {
        PUBLISH_SWAP.with_borrow(|swap| {
            if let Some(swap) = swap.as_ref() {
                swap(dest);
            }
        });
    }

    /// Installs a swap at the publish seam for as long as it is held, so a
    /// destination that changes between admission and publication can be
    /// reproduced at an exact interleaving.
    struct PublishSwap;

    impl PublishSwap {
        fn install(swap: impl Fn(&Path) + 'static) -> Self {
            PUBLISH_SWAP.with_borrow_mut(|slot| *slot = Some(Box::new(swap)));
            Self
        }
    }

    impl Drop for PublishSwap {
        fn drop(&mut self) {
            PUBLISH_SWAP.with_borrow_mut(|slot| *slot = None);
        }
    }

    /// Installs a swap at the source-root walk's check-to-open seam for as
    /// long as it is held, so an ancestor replacement race can be reproduced
    /// at an exact interleaving.
    struct SourceRootSwap;

    impl SourceRootSwap {
        fn install(swap: impl Fn(&Path) + 'static) -> Self {
            SOURCE_ROOT_SWAP.with_borrow_mut(|slot| *slot = Some(Box::new(swap)));
            Self
        }
    }

    impl Drop for SourceRootSwap {
        fn drop(&mut self) {
            SOURCE_ROOT_SWAP.with_borrow_mut(|slot| *slot = None);
        }
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
            retired_name(OsStr::new(".zeroclaw-export-AbC123")),
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
        let copied = copy_workspace(&plan, dest.path(), &mut Vec::new()).unwrap();

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
        let copied = copy_workspace(&plan, dest.path(), &mut Vec::new()).unwrap();

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
        let copied = copy_workspace(&plan, dest.path(), &mut Vec::new()).unwrap();

        assert_eq!(copied.files, 1);
        assert_eq!(copied.symlinks_skipped, 1);
        assert!(!dest.path().join("escape.txt").exists());
    }

    #[test]
    fn missing_workspace_is_not_an_error() {
        let dest = tempfile::tempdir().unwrap();
        let plan = plan_for(Path::new("/nonexistent/zeroclaw/workspace"));
        let copied = copy_workspace(&plan, dest.path(), &mut Vec::new()).unwrap();
        assert_eq!(copied, CopyTally::default());
    }

    /// An install-shaped skills fixture: the returned bundle directory is
    /// `<install>/shared/skills/<alias>`, the only shape the anchored walk
    /// opens. The `TempDir` is the install root; keep it alive.
    fn install_with_skills(alias: &str) -> (tempfile::TempDir, PathBuf) {
        let install = tempfile::tempdir().unwrap();
        let dir = install.path().join("shared").join("skills").join(alias);
        std::fs::create_dir_all(&dir).unwrap();
        (install, dir)
    }

    /// Anchor the plan at the install root three components above `dir`,
    /// which must be `<install>/shared/skills/<alias>`.
    fn bound_skills_to(plan: &mut ExportPlan, dir: &Path) {
        let install = dir
            .ancestors()
            .nth(3)
            .map_or_else(|| dir.to_path_buf(), Path::to_path_buf);
        plan.source_boundaries.install_root = install;
    }

    /// A plan carrying one skill bundle the way `plan_export` builds one: the
    /// source to copy, plus the `carried_skills` grant that advertises it in
    /// the manifest. The advertisement is what has to track reality.
    fn plan_with_skills(workspace: &Path, dir: &Path, exclude: &[&str]) -> ExportPlan {
        let mut plan = plan_for(workspace);
        bound_skills_to(&mut plan, dir);
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
        let name = dir.file_name().expect("bundle directory has a name");
        SkillBundleSource {
            alias: alias.to_string(),
            source: dir.to_path_buf(),
            relative: PathBuf::from("shared").join("skills").join(name),
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
        let (_install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");
        write(&skills.join("web_search/run.sh"), "#!/bin/sh\n");
        write(&skills.join("internal_only/SKILL.md"), "# internal");
        write(&skills.join(".sync-marker"), "local state");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        bound_skills_to(&mut plan, &skills);
        plan.skill_sources = vec![skill_source("research_tools", &skills, &["internal_only"])];

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
        let (_install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");

        let mut plan = plan_for(workspace.path());
        bound_skills_to(&mut plan, &skills);
        plan.skill_sources = vec![skill_source("research_tools", &skills, &[])];

        // Inside the bundle's directory: the copy would consume its own output.
        let err = write_bundle(&mut plan, &skills.join("exports/bundle"), true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("inside skill bundle"), "{err}");

        // Containing it: publishing would replace the skills being read.
        let err = write_bundle(&mut plan, &skills, true).await.unwrap_err();
        assert!(err.to_string().contains("contains skill bundle"), "{err}");
        assert!(skills.join("web_search/SKILL.md").is_file());
    }

    #[tokio::test]
    async fn a_skill_bundle_with_no_content_is_recorded_in_the_manifest_not_advertised() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        // The install exists; the bundle's directory under it does not.
        let install = tempfile::tempdir().unwrap();
        let mut plan = plan_with_skills(
            workspace.path(),
            &install.path().join("shared/skills/research_tools"),
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

    /// An admitted skill directory is not carried content: if everything in it
    /// is skipped, the bundle has no skill file, and the manifest must not say
    /// otherwise.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_skill_whose_entries_are_all_skipped_is_not_advertised() {
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("host.md"), "host content");

        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        // Admitted by the bundle's filter, but holding nothing that travels.
        let (_install, skills) = install_with_skills("research_tools");
        std::fs::create_dir_all(skills.join("web_search")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("host.md"),
            skills.join("web_search/SKILL.md"),
        )
        .unwrap();

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(workspace.path(), &skills, &[]);
        let copied = write_bundle(&mut plan, &out, false).await.unwrap();

        assert_eq!(copied.skills.tally.files, 0);
        assert_eq!(copied.skills.bundles, 0);
        assert_eq!(
            copied.skills.without_content,
            vec!["research_tools".to_string()]
        );

        // Manifest and tree agree that nothing was carried.
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
    }

    /// Defence in depth for the planner's alias check: even handed a hostile
    /// alias, materialization goes through a handle on `skills/`, so nothing
    /// can be written outside the staging tree.
    #[tokio::test]
    async fn a_traversing_skill_alias_writes_nothing_outside_the_staging_tree() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");
        let (_install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        bound_skills_to(&mut plan, &skills);
        plan.skill_sources = vec![SkillBundleSource {
            alias: "../../outside".to_string(),
            relative: skill_source("x", &skills, &[]).relative,
            source: skills.clone(),
            filter: zeroclaw_config::schema::SkillBundleConfig::default(),
        }];

        let result = write_bundle(&mut plan, &out, false).await;

        // Whether it errors or carries nothing, the one thing that must hold is
        // that no file appeared outside the requested destination.
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        assert!(!parent.path().join("outside").exists());
        assert!(!parent.path().parent().unwrap().join("outside").exists());
        if let Ok(copied) = result {
            assert_eq!(copied.skills.tally.files, 0);
        }
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
        let (_install, skills) = install_with_skills("research_tools");
        write(&skills.join("internal_only/SKILL.md"), "# internal");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(workspace.path(), &skills, &["internal_only"]);
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
        let (_install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(workspace.path(), &skills, &[]);

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

        let (_install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");
        std::os::unix::fs::symlink(outside.path().join("secret"), skills.join("borrowed")).unwrap();

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        bound_skills_to(&mut plan, &skills);
        plan.skill_sources = vec![skill_source("research_tools", &skills, &[])];

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

            copy_workspace(&plan_for(source.path()), dest.path(), &mut Vec::new()).unwrap();

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

    /// The planner keeps a workspace-relative identity path; only the copy
    /// knows whether the file was there. A reference the bundle cannot satisfy
    /// is dropped from the published config rather than shipped dangling.
    #[tokio::test]
    async fn an_identity_document_the_copy_did_not_carry_is_dropped() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(source.path());
        plan.identity_document = Some("identity/aieos.json".to_string());
        plan.config = agent_identity_config("identity/aieos.json");

        write_bundle(&mut plan, &out, false).await.unwrap();

        let published = std::fs::read_to_string(out.join(CONFIG_FILE)).unwrap();
        assert!(!published.contains("aieos.json"), "{published}");
        assert!(plan.identity_document.is_none());
    }

    #[tokio::test]
    async fn an_identity_document_the_copy_carried_is_kept() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("identity/aieos.json"), "{}");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(source.path());
        plan.identity_document = Some("identity/aieos.json".to_string());
        plan.config = agent_identity_config("identity/aieos.json");

        write_bundle(&mut plan, &out, false).await.unwrap();

        let published = std::fs::read_to_string(out.join(CONFIG_FILE)).unwrap();
        assert!(published.contains("identity/aieos.json"), "{published}");
        assert!(
            out.join(WORKSPACE_DIR)
                .join("identity/aieos.json")
                .is_file()
        );
    }

    /// Refusing a symlink at the final component leaves every component above
    /// it followed. A symlinked `shared` or `shared/skills` hands the copy an
    /// outside tree while the bundle directory itself looks ordinary — the
    /// walk refuses the link at the component where it sits.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_skill_root_reached_through_a_symlinked_ancestor_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        write(
            &outside.path().join("research_tools/web_search/SKILL.md"),
            "host skill content",
        );

        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        // `<install>/shared/skills` is a link out of the install; the bundle
        // directory below it is a real directory.
        let install = tempfile::tempdir().unwrap();
        let shared = install.path().join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::os::unix::fs::symlink(outside.path(), shared.join("skills")).unwrap();
        let bundle_root = shared.join("skills/research_tools");
        assert!(bundle_root.is_dir());
        assert!(
            !std::fs::symlink_metadata(&bundle_root)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        plan.source_boundaries.install_root = install.path().to_path_buf();
        plan.skill_sources = vec![skill_source("research_tools", &bundle_root, &[])];

        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        assert!(
            outside
                .path()
                .join("research_tools/web_search/SKILL.md")
                .is_file()
        );
    }

    /// The in-install redirect: `shared/skills` points at another directory
    /// *inside* the install. A boundary compared against the install root
    /// alone admits this; the no-follow walk refuses the link regardless of
    /// where it points, because skill content is only ever opened through
    /// real directories under `shared/`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_skill_root_redirected_inside_the_install_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        // The content lives in the install, but not in the shared tree.
        let install = tempfile::tempdir().unwrap();
        write(
            &install
                .path()
                .join("private-skills/research_tools/ops/SKILL.md"),
            "not shared content",
        );
        let shared = install.path().join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::os::unix::fs::symlink(install.path().join("private-skills"), shared.join("skills"))
            .unwrap();
        let bundle_root = shared.join("skills/research_tools");
        assert!(bundle_root.is_dir());

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        plan.source_boundaries.install_root = install.path().to_path_buf();
        plan.skill_sources = vec![skill_source("research_tools", &bundle_root, &[])];

        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_workspace_reached_through_a_symlinked_ancestor_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        write(
            &outside.path().join("researcher/workspace/host.md"),
            "host content",
        );

        // `<install>/agents` is a link out of the install.
        let install = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(install.path()).unwrap();
        std::os::unix::fs::symlink(outside.path(), install.path().join("agents")).unwrap();
        let workspace = install.path().join("agents/researcher/workspace");
        assert!(workspace.is_dir());

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&workspace);
        plan.source_boundaries.install_root = install.path().to_path_buf();
        plan.source_boundaries.workspace = Some(PathBuf::from("agents/researcher/workspace"));

        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        // And the host content the ancestor pointed at is untouched.
        assert!(
            outside
                .path()
                .join("researcher/workspace/host.md")
                .is_file()
        );
    }

    /// The workspace flavor of the in-install redirect: `agents` points at a
    /// sibling directory inside the install, so an install-root comparison
    /// passes while the copy walks a tree the agents contract does not own.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_default_workspace_redirected_inside_the_install_is_refused() {
        let install = tempfile::tempdir().unwrap();
        write(
            &install
                .path()
                .join("elsewhere/researcher/workspace/host.md"),
            "not the agents tree",
        );
        std::os::unix::fs::symlink(
            install.path().join("elsewhere"),
            install.path().join("agents"),
        )
        .unwrap();
        let workspace = install.path().join("agents/researcher/workspace");
        assert!(workspace.is_dir());

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&workspace);
        plan.source_boundaries.install_root = install.path().to_path_buf();
        plan.source_boundaries.workspace = Some(PathBuf::from("agents/researcher/workspace"));

        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    /// Deterministic ancestor replacement: the walk classifies `agents` as a
    /// real directory, and it becomes a symlink before the open. The open is
    /// the enforcement, so the export fails instead of following the link —
    /// the race cannot turn a refusal into a traversal.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_workspace_ancestor_replaced_mid_walk_fails_the_export() {
        let install = tempfile::tempdir().unwrap();
        let workspace = install.path().join("agents/researcher/workspace");
        write(&workspace.join("notes.md"), "workspace note");
        let outside = tempfile::tempdir().unwrap();
        write(
            &outside.path().join("researcher/workspace/host.md"),
            "host content",
        );

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&workspace);
        plan.source_boundaries.install_root = install.path().to_path_buf();
        plan.source_boundaries.workspace = Some(PathBuf::from("agents/researcher/workspace"));

        let agents = install.path().join("agents");
        let retired = install.path().join("agents-retired");
        let target = outside.path().to_path_buf();
        let err = {
            let _swap = SourceRootSwap::install(move |walked| {
                if walked.file_name().is_some_and(|name| name == "agents") {
                    std::fs::rename(&agents, &retired).unwrap();
                    std::os::unix::fs::symlink(&target, &agents).unwrap();
                }
            });
            write_bundle(&mut plan, &out, false).await.unwrap_err()
        };

        assert!(err.to_string().contains("failed to open"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        assert!(
            outside
                .path()
                .join("researcher/workspace/host.md")
                .is_file()
        );
    }

    /// The same replacement race against a skill root's `shared/skills`
    /// ancestor.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_skill_ancestor_replaced_mid_walk_fails_the_export() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        let (install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");
        let outside = tempfile::tempdir().unwrap();
        write(
            &outside.path().join("research_tools/ops/SKILL.md"),
            "host skill content",
        );

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(workspace.path(), &skills, &[]);

        let skills_dir = install.path().join("shared/skills");
        let retired = install.path().join("shared/skills-retired");
        let target = outside.path().to_path_buf();
        let err = {
            let _swap = SourceRootSwap::install(move |walked| {
                if walked.file_name().is_some_and(|name| name == "skills") {
                    std::fs::rename(&skills_dir, &retired).unwrap();
                    std::os::unix::fs::symlink(&target, &skills_dir).unwrap();
                }
            });
            write_bundle(&mut plan, &out, false).await.unwrap_err()
        };

        assert!(err.to_string().contains("failed to open"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        assert!(outside.path().join("research_tools/ops/SKILL.md").is_file());
    }

    /// Only an absent source means there is nothing to copy. A source the
    /// export cannot *read* is a failure: succeeding would publish a bundle
    /// whose manifest stands behind content nobody inspected.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_workspace_ancestor_fails_the_export_instead_of_publishing() {
        use std::os::unix::fs::PermissionsExt;

        let install = tempfile::tempdir().unwrap();
        let workspace = install.path().join("agents/researcher/workspace");
        write(&workspace.join("notes.md"), "workspace note");

        let agents = install.path().join("agents");
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::metadata(agents.join("researcher")).is_ok() {
            // Running with CAP_DAC_OVERRIDE (root): the environment cannot
            // produce the permission failure this test pins.
            std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&workspace);
        plan.source_boundaries.install_root = install.path().to_path_buf();
        plan.source_boundaries.workspace = Some(PathBuf::from("agents/researcher/workspace"));

        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        // Restore before asserting so the tempdir can clean up either way.
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        let shown = format!("{err:#}");
        assert!(
            shown.contains("agents"),
            "the failure should name the component: {shown}"
        );
    }

    /// A component replaced by another *real directory* between classification
    /// and open passes every symlink test; only filesystem identity separates
    /// it from the tree that was classified. The copy carries what it
    /// inspected or nothing at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_walk_component_replaced_by_another_directory_fails_the_export() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        let (install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");
        // A sibling tree inside the install, same filesystem, swapped whole.
        write(
            &install
                .path()
                .join("staging/skills/research_tools/ops/SKILL.md"),
            "staged content the copy never classified",
        );

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(workspace.path(), &skills, &[]);

        let skills_dir = install.path().join("shared/skills");
        let retired = install.path().join("shared/skills-retired");
        let staged = install.path().join("staging/skills");
        let err = {
            let _swap = SourceRootSwap::install(move |walked| {
                if walked.file_name().is_some_and(|name| name == "skills") {
                    std::fs::rename(&skills_dir, &retired).unwrap();
                    std::fs::rename(&staged, &skills_dir).unwrap();
                }
            });
            write_bundle(&mut plan, &out, false).await.unwrap_err()
        };

        assert!(err.to_string().contains("replaced"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    /// The configured-workspace flavor of the replacement race: the leaf is
    /// classified as a real directory and becomes a symlink before the open.
    /// The no-follow open on the parent handle is the enforcement.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_configured_workspace_swapped_for_a_link_mid_open_fails_the_export() {
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("host-secret.txt"), "not the agent's");

        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join("workspace");
        write(&workspace.join("notes.md"), "workspace note");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&workspace);
        let swap_workspace = workspace.clone();
        let target = outside.path().to_path_buf();
        let err = {
            let _swap = SourceRootSwap::install(move |walked| {
                if walked.file_name().is_some_and(|name| name == "workspace") {
                    std::fs::remove_dir_all(&swap_workspace).unwrap();
                    std::os::unix::fs::symlink(&target, &swap_workspace).unwrap();
                }
            });
            write_bundle(&mut plan, &out, false).await.unwrap_err()
        };

        assert!(err.to_string().contains("failed to open"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        assert!(outside.path().join("host-secret.txt").is_file());
    }

    /// A destination that reaches through `..` inside a not-yet-existing
    /// directory resolves to nothing at check time and to a real tree at
    /// publish time — `missing/../<workspace>` would slip past the overlap
    /// check and publish over the workspace being read.
    #[tokio::test]
    async fn a_destination_through_a_missing_parent_dir_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join("workspace");
        write(&workspace.join("notes.md"), "workspace note");

        let mut plan = plan_for(&workspace);
        let out = home.path().join("missing/../workspace");
        let err = write_bundle(&mut plan, &out, true).await.unwrap_err();

        assert!(err.to_string().contains(".."), "{err}");
        // The workspace is untouched: not retired, not replaced.
        assert!(workspace.join("notes.md").is_file());
        assert!(!home.path().join("missing").exists());
        assert_eq!(
            entry_names(home.path()),
            vec!["workspace".to_string()],
            "nothing was staged or retired"
        );
    }

    /// The replacement that defeats every symlink test: an admitted skill
    /// directory renamed away and the excluded sibling renamed into its
    /// place, both real directories. Only filesystem identity separates the
    /// opened handle from the classified entry.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_skill_replaced_by_an_excluded_sibling_directory_is_not_copied() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        let (_install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");
        write(&skills.join("internal_only/SKILL.md"), "# internal runbook");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        bound_skills_to(&mut plan, &skills);
        plan.skill_sources = vec![skill_source("research_tools", &skills, &["internal_only"])];

        let admitted = skills.join("web_search");
        // Retired OUTSIDE the bundle directory so the walk never sees it.
        let retired = skills.parent().unwrap().join("web_search-retired");
        let excluded = skills.join("internal_only");
        let copied = {
            let _swap = EntrySwap::install(move |relative| {
                if relative == Path::new("web_search") {
                    std::fs::rename(&admitted, &retired).unwrap();
                    std::fs::rename(&excluded, &admitted).unwrap();
                }
            });
            write_bundle(&mut plan, &out, false).await.unwrap()
        };

        assert_eq!(copied.skills.tally.replaced_skipped, 1);
        for file in all_files(&out) {
            let bytes = std::fs::read(&file).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("internal runbook"),
                "{} carries the excluded skill",
                file.display()
            );
        }
    }

    /// A file squatting the source's place is a broken install, not an
    /// absent source: publishing an empty bundle over it would be a success
    /// the manifest cannot stand behind.
    #[tokio::test]
    async fn a_file_where_the_workspace_should_be_fails_the_export() {
        let install = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(install.path().join("agents/researcher")).unwrap();
        write(
            &install.path().join("agents/researcher/workspace"),
            "a file, not a directory",
        );
        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&install.path().join("agents/researcher/workspace"));
        plan.source_boundaries.install_root = install.path().to_path_buf();
        plan.source_boundaries.workspace = Some(PathBuf::from("agents/researcher/workspace"));

        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    /// The source flavor of the unresolvable `..`: a configured workspace
    /// reaching through a directory that does not exist yet cannot be
    /// overlap-checked, so the export refuses rather than guessing.
    #[tokio::test]
    async fn a_configured_workspace_through_a_missing_parent_is_refused() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("workspace/notes.md"), "workspace note");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let configured = home.path().join("missing/../workspace");
        let mut plan = plan_for(&configured);
        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        assert!(err.to_string().contains(".."), "{err}");
        assert!(!out.exists());
        assert!(!home.path().join("missing").exists());
    }

    /// A configured path with no final component to bind to (it ends in `..`)
    /// must refuse, not publish an empty workspace: the runtime may resolve
    /// it to a real tree the bundle then silently lacks.
    #[tokio::test]
    async fn a_configured_workspace_path_ending_in_parent_dir_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let real = home.path().join("workspace");
        write(&real.join("notes.md"), "workspace note");
        // `inner` exists, so the `..` resolves and the walk reaches the leaf
        // check; the missing-parent case is covered separately below.
        std::fs::create_dir_all(real.join("inner")).unwrap();
        let configured = home.path().join("workspace/inner/..");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&configured);
        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        assert!(err.to_string().contains("plain directory name"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
    }

    /// The configured-workspace flavor of "unreadable is not absent": a parent
    /// the export cannot search must abort the export, not publish a bundle
    /// with an empty workspace.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_configured_workspace_parent_fails_the_export() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let holder = home.path().join("holder");
        let workspace = holder.join("workspace");
        write(&workspace.join("notes.md"), "workspace note");

        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::metadata(&workspace).is_ok() {
            // Running with CAP_DAC_OVERRIDE (root): the environment cannot
            // produce the permission failure this test pins.
            std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&workspace);
        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        let shown = format!("{err:#}");
        assert!(
            shown.contains("holder") || shown.contains("workspace"),
            "the failure should name the path: {shown}"
        );
    }

    /// The no-follow descent starts *below* the root, so the root itself is a
    /// separate boundary: a symlinked workspace puts the whole walk outside the
    /// tree the bundle claims to carry before any of it runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_workspace_root_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("host-secret.txt"), "not the agent's");

        // The configured workspace path is a link to somewhere else entirely.
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join("workspace");
        std::os::unix::fs::symlink(outside.path(), &workspace).unwrap();

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let err = write_bundle(&mut plan_for(&workspace), &out, false)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("is a symlink"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        assert!(outside.path().join("host-secret.txt").is_file());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_skill_bundle_root_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        write(
            &outside.path().join("host-skill/SKILL.md"),
            "not the agent's",
        );

        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        // `<install>/shared/skills/<alias>` is a link out of the shared tree.
        let install = tempfile::tempdir().unwrap();
        let skills = install.path().join("shared/skills");
        std::fs::create_dir_all(&skills).unwrap();
        let bundle_root = skills.join("research_tools");
        std::os::unix::fs::symlink(outside.path(), &bundle_root).unwrap();

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_with_skills(workspace.path(), &bundle_root, &[]);
        let err = write_bundle(&mut plan, &out, false).await.unwrap_err();

        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!out.exists());
        assert_eq!(entry_names(parent.path()), Vec::<String>::new());
        assert!(outside.path().join("host-skill/SKILL.md").is_file());
    }

    /// The replacement that passes every shape test: a different regular file,
    /// one link, no symlink anywhere. Only identity separates it from the entry
    /// the copy classified.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_renamed_over_an_admitted_entry_is_not_copied() {
        // Same filesystem as the workspace, so this is a rename, not a copy.
        let root = tempfile::tempdir().unwrap();
        let host = root.path().join("host-secret.txt");
        write(&host, "host secret bytes");

        let source = root.path().join("workspace");
        let entry = source.join("notes.md");
        write(&entry, "workspace note");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(&source);
        let copied = {
            let _swap = EntrySwap::install(move |relative| {
                if relative == Path::new("notes.md") {
                    std::fs::rename(&host, &entry).unwrap();
                    let meta = std::fs::symlink_metadata(&entry).unwrap();
                    assert!(meta.file_type().is_file());
                    assert_eq!(std::os::unix::fs::MetadataExt::nlink(&meta), 1);
                }
            });
            write_bundle(&mut plan, &out, false).await.unwrap()
        };

        assert_eq!(copied.workspace.replaced_skipped, 1);
        assert_eq!(copied.workspace.files, 0);
        for file in all_files(&out) {
            let bytes = std::fs::read(&file).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("host secret bytes"),
                "{} carries the host file",
                file.display()
            );
        }
    }

    /// The same shape one level down, inside carried skill content.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_skill_file_renamed_over_an_admitted_entry_is_not_copied() {
        let root = tempfile::tempdir().unwrap();
        let host = root.path().join("host-secret.md");
        write(&host, "host secret bytes");

        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join("IDENTITY.md"), "identity");

        // Same filesystem as `host`, so the swap below is a rename.
        let skills = root.path().join("shared/skills/research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");
        write(&skills.join("web_search/notes.md"), "skill note");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let target = skills.join("web_search/notes.md");
        let mut plan = plan_with_skills(workspace.path(), &skills, &[]);
        let copied = {
            let _swap = EntrySwap::install(move |relative| {
                if relative == Path::new("web_search/notes.md") {
                    std::fs::rename(&host, &target).unwrap();
                }
            });
            write_bundle(&mut plan, &out, false).await.unwrap()
        };

        assert_eq!(copied.skills.tally.replaced_skipped, 1);
        for file in all_files(&out) {
            let bytes = std::fs::read(&file).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("host secret bytes"),
                "{} carries the host file",
                file.display()
            );
        }
    }

    /// No-follow proves a name is not a symlink; it says nothing about whether
    /// the bytes belong to this tree. A hard link is the same object under a
    /// second name, so it classifies and opens as an ordinary regular file.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_replaced_by_a_hard_link_to_a_host_file_is_not_copied() {
        let outside = tempfile::tempdir().unwrap();
        let host = outside.path().join("host-secret.txt");
        write(&host, "host secret bytes");

        let source = tempfile::tempdir().unwrap();
        let entry = source.path().join("notes.md");
        write(&entry, "workspace note");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(source.path());
        let copied = {
            let _swap = EntrySwap::install(move |relative| {
                if relative == Path::new("notes.md") {
                    std::fs::remove_file(&entry).unwrap();
                    std::fs::hard_link(&host, &entry).unwrap();
                    // Still a regular file, still not a symlink, and the name
                    // never leaves the workspace directory.
                    let meta = std::fs::symlink_metadata(&entry).unwrap();
                    assert!(meta.file_type().is_file());
                    assert_eq!(
                        std::fs::read_to_string(&entry).unwrap(),
                        "host secret bytes"
                    );
                }
            });
            write_bundle(&mut plan, &out, false).await.unwrap()
        };

        assert_eq!(copied.workspace.hard_links_skipped, 1);
        assert_eq!(copied.workspace.files, 0);
        assert!(!out.join(WORKSPACE_DIR).join("notes.md").exists());
        for file in all_files(&out) {
            let bytes = std::fs::read(&file).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("host secret bytes"),
                "{} carries the host file",
                file.display()
            );
        }
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

        let (_install, skills) = install_with_skills("research_tools");
        write(&skills.join("web_search/SKILL.md"), "# search");
        write(&skills.join("internal_only/SKILL.md"), "# internal runbook");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let mut plan = plan_for(workspace.path());
        bound_skills_to(&mut plan, &skills);
        plan.skill_sources = vec![skill_source("research_tools", &skills, &["internal_only"])];

        let admitted = skills.join("web_search");
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

    /// The destination was absent when the occupancy decision was made, so
    /// nothing needed `--force` and nothing was given it. A directory that
    /// appears during the copy was admitted by nobody: publishing over it
    /// would retire and then recursively delete a tree the operator was never
    /// shown.
    #[tokio::test]
    async fn a_destination_that_appears_after_admission_is_not_retired() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");

        let planted = out.clone();
        let _swap = PublishSwap::install(move |_| {
            write(
                &planted.join("someone-elses.md"),
                "not the export's to delete",
            );
        });

        let err = write_bundle(&mut plan_for(source.path()), &out, false)
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("did not exist when the export started"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("someone-elses.md")).unwrap(),
            "not the export's to delete"
        );
        // Nothing was retired, and the staging tree cleaned up after itself.
        assert_eq!(entry_names(parent.path()), vec!["bundle".to_string()]);
    }

    /// An empty destination is replaceable without `--force` because there is
    /// nothing to lose. Content that appears in it during the copy was never
    /// covered by that, and `remove_dir` is what keeps the promise honest:
    /// the kernel refuses a directory that has gained entries since.
    #[tokio::test]
    async fn an_empty_destination_filled_after_admission_is_not_replaced() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");
        std::fs::create_dir(&out).unwrap();

        let filled = out.clone();
        let _swap = PublishSwap::install(move |_| {
            write(&filled.join("arrived.md"), "written after the check");
        });

        let err = write_bundle(&mut plan_for(source.path()), &out, false)
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("failed to clear the empty destination"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("arrived.md")).unwrap(),
            "written after the check"
        );
        assert_eq!(entry_names(parent.path()), vec!["bundle".to_string()]);
    }

    /// `--force` admits one directory for replacement. A different directory
    /// renamed into its place during the copy is not the one the operator
    /// looked at, so the retire-then-delete path must not be pointed at it.
    #[tokio::test]
    async fn a_forced_destination_replaced_after_admission_is_not_retired() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");
        write(&out.join("old.md"), "the bundle the operator forced over");

        let impostor = parent.path().join("impostor");
        write(&impostor.join("kept.md"), "never admitted for replacement");

        let admitted = out.clone();
        let moved_aside = parent.path().join("moved-aside");
        let aside = moved_aside.clone();
        let _swap = PublishSwap::install(move |_| {
            std::fs::rename(&admitted, &aside).unwrap();
            std::fs::rename(aside.with_file_name("impostor"), &admitted).unwrap();
        });

        let err = write_bundle(&mut plan_for(source.path()), &out, true)
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("not the directory this export checked"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("kept.md")).unwrap(),
            "never admitted for replacement"
        );
        assert!(moved_aside.join("old.md").is_file());
    }

    /// A tree the export read, renamed onto the destination during the copy.
    /// The identity check would refuse this anyway, but only to say the
    /// destination changed; publication names it for what it is, because the
    /// object it would delete is the workspace the bundle just carried.
    #[tokio::test]
    async fn a_source_tree_moved_onto_the_destination_is_not_retired() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("workspace");
        write(&source.join("IDENTITY.md"), "identity");

        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");
        write(&out.join("old.md"), "the bundle the operator forced over");

        let admitted = out.clone();
        let workspace = source.clone();
        let _swap = PublishSwap::install(move |_| {
            std::fs::remove_dir_all(&admitted).unwrap();
            std::fs::rename(&workspace, &admitted).unwrap();
        });

        let err = write_bundle(&mut plan_for(&source), &out, true)
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("one of the trees this export read"),
            "{err}"
        );
        assert!(out.join("IDENTITY.md").is_file());
    }

    /// The parent is opened once and every publishing step goes through that
    /// handle, so a parent replaced during the copy cannot redirect the
    /// publish into a directory the operator never named: the bundle lands in
    /// the object that was admitted, whatever now wears its name. The report
    /// still prints the path as given, which under this race no longer names
    /// the published bundle.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_replaced_parent_cannot_redirect_the_publish() {
        let source = tempfile::tempdir().unwrap();
        write(&source.path().join("IDENTITY.md"), "identity");

        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("out");
        std::fs::create_dir(&parent).unwrap();
        let out = parent.join("bundle");

        let admitted = parent.clone();
        let moved = root.path().join("moved");
        let aside = moved.clone();
        let _swap = PublishSwap::install(move |_| {
            std::fs::rename(&admitted, &aside).unwrap();
            std::fs::create_dir(&admitted).unwrap();
        });

        write_bundle(&mut plan_for(source.path()), &out, false)
            .await
            .unwrap();

        assert!(moved.join("bundle").join(MANIFEST_FILE).is_file());
        // The impostor that took the admitted parent's name is untouched.
        assert!(entry_names(&parent).is_empty());
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
