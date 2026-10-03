use std::path::PathBuf;

use bstr::BStr;
use but_error::bail_precondition;
use but_oxidize::{ObjectIdExt, OidExt as _};
use gix::{
    merge::{plumbing::tree::ConflictIndexEntry, tree::TreatAsUnresolved},
    prelude::ObjectIdExt as _,
    refs::Target,
};
use tracing::instrument;

use crate::{RepositoryExt, update_head_reference};

use super::{Options, Outcome};

/// Perform all file operations necessary to turn the *worktree* of `repo` into
/// `new_head_id^{tree}`.
///
/// If `new_head_id` is a *commit*, we will also set `HEAD` (or the ref it points to if symbolic) to the `new_head_id`.
/// We will also update the `.git/index` to match the `new_head_id^{tree}`, while leaving uncommitted
/// worktree content in place, so it stays visible as an unstaged modification as with `git checkout`.
/// Unresolved conflicts keep their stages 1/2/3 in the index.
/// GitButler-conflicted commits are rejected by default before any worktree, index, or ref update.
///
/// We will always handle changes in the worktree safely to avoid loss of uncommitted information. This also means that deletions
/// never cause us to conflict. Conflicted files that would be checked out will cause an error.
///
/// #### Note: No rename tracking
///
/// To keep it simpler, we don't do rename tracking, so deletions and additions are always treated separately.
/// If this changes, then the source sid of a rename could also cause conflicts, maybe? It's a bit unclear what it would mean
/// in practice, but I guess that we bring deleted files back instead of conflicting.
#[instrument(skip(repo), err(Debug))]
pub fn safe_checkout_from_head(
    new_head_id: gix::ObjectId,
    repo: &gix::Repository,
    Options {
        skip_head_update,
        merge_base_override,
        allow_conflicted_commit_checkout,
        allow_uncommitted_changes_to_conflict_with_new_head,
    }: Options,
) -> anyhow::Result<Outcome> {
    let new_object = new_head_id.attach(repo).object()?;
    if !allow_conflicted_commit_checkout
        && new_object.kind.is_commit()
        && crate::Commit::from_id(new_head_id.attach(repo))?.is_conflicted()
    {
        return Err(
            anyhow::anyhow!(but_error::Code::ConflictedCommitCheckout).context(format!(
                "Refusing to check out conflicted commit {new_head_id}"
            )),
        );
    }

    let git2_repo = but_oxidize::open_git2_repo(repo.git_dir())?;
    let head_tree_id = repo.head_tree_id_or_empty()?;
    let head_tree = git2_repo.find_tree(head_tree_id.to_git2())?;
    // The index is written once, at the end of this function, for exactly the paths this checkout
    // changes. Writing it here as well (as this used to) would replace the whole index whenever the
    // incremental apply failed, dropping staged changes and `git rm --cached` state with it.
    let old_tree = if let Some(id) = merge_base_override {
        git2_repo.find_object(id.to_git2(), None)?.peel_to_tree()?
    } else {
        head_tree.clone()
    };

    let new_tree = git2_repo
        .find_object(new_head_id.to_git2(), None)?
        .peel_to_tree()?;
    let mut conflict_occurred = false;
    // Filled in when the checkout resolves conflicts, to place the same conflict stages into the
    // index Git reads.
    let mut conflict_entries = Vec::new();
    // Paths the checkout rewrites in the worktree. The index follows the checkout for them: it holds
    // what was checked out, which is also what `git diff` has to compare the worktree against.
    let mut rewritten_paths = Vec::new();
    if old_tree.id() != new_tree.id() {
        // Reopen to ensure that there is no "object memory" (i.e. all object
        // writes actually happen on disk).
        let mut repo = gix::open(repo.git_dir())?;
        #[allow(deprecated)]
        let wd_tree_id = repo.create_wd_tree(u64::MAX)?;
        let wd_tree = git2_repo
            .find_object(wd_tree_id.to_git2(), None)?
            .peel_to_tree()?;
        repo.config_snapshot_mut()
            .set_value(&gix::config::tree::Merge::RENORMALIZE, "true")?;
        let mut outcome = repo.merge_trees(
            old_tree.id().to_gix(),
            wd_tree_id,
            new_tree.id().to_gix(),
            Default::default(),
            repo.tree_merge_options()?.with_rewrites(None),
        )?;
        let checkout_target_base_id = outcome.tree.write()?.detach();

        let checkout_target_base = git2_repo.find_tree(checkout_target_base_id.to_git2())?;
        rewritten_paths = changed_paths(&git2_repo, &wd_tree, &checkout_target_base)?;
        let mut checkout_target = git2::Index::new()?;
        checkout_target.read_tree(&checkout_target_base)?;

        let unresolved = TreatAsUnresolved::git();
        if outcome.has_unresolved_conflicts(unresolved) {
            conflict_occurred = true;
            if allow_uncommitted_changes_to_conflict_with_new_head {
                for conflict in outcome.conflicts.iter() {
                    if !conflict.is_unresolved(unresolved) {
                        continue;
                    }
                    let [base, ours, theirs] = conflict.entries();
                    if base.is_some_and(|c| c.mode.is_tree())
                        || ours.is_some_and(|c| c.mode.is_tree())
                        || theirs.is_some_and(|c| c.mode.is_tree())
                    {
                        bail_precondition!(
                            "Cannot checkout file-directory conflict: {}",
                            conflict.ours.location()
                        );
                    }
                    let entries = ConflictIndexEntries {
                        base: base.map(|c| to_git2_index_entry(&c, conflict.ours.location())),
                        ours: ours.map(|c| to_git2_index_entry(&c, conflict.ours.location())),
                        theirs: theirs.map(|c| to_git2_index_entry(&c, conflict.ours.location())),
                    };
                    entries.add_to(&mut checkout_target)?;
                    conflict_entries.push(entries);
                }
            } else {
                let mut paths = outcome
                    .conflicts
                    .iter()
                    .filter(|c| c.is_unresolved(unresolved))
                    .map(|c| format!("{:?}", c.ours.location()))
                    .collect::<Vec<_>>();
                paths.sort();
                paths.dedup();
                bail_precondition!(
                    "Uncommitted files would be overwritten by checkout: {}",
                    paths.join(", ")
                );
            }
        }

        let mut checkout_opts = git2::build::CheckoutBuilder::new();
        checkout_opts.baseline(&wd_tree);
        // `.git/index` is written below, scoped to the paths this operation changes. The index that
        // a checkout would write is the merge result, which contains uncommitted worktree content:
        // Git then reports committed work as staged and `git diff` shows nothing to commit.
        checkout_opts.update_index(false);
        git2_repo.checkout_index(Some(&mut checkout_target), Some(&mut checkout_opts))?;
    }

    sync_index_with_head(
        &git2_repo,
        &head_tree,
        &new_tree,
        &rewritten_paths,
        &conflict_entries,
    )?;
    let mut head_update = None;
    if new_object.kind.is_commit() && !skip_head_update {
        let needs_update = repo
            .head()?
            .id()
            .is_none_or(|actual_head_id| actual_head_id != new_head_id);
        if needs_update {
            // We play it loose here, as we assume a repository lock so we won't interfere with ourselves.
            // Git itself enforces no lock either, so we rely on basic locking ref-locking here. Good enough.
            let edits = update_head_reference(
                repo,
                Target::Object(new_head_id),
                true,
                "safe checkout",
                "GitButler".into(),
                new_object.into_commit().parent_ids().count(),
            )?;
            head_update = Some(edits);
        }
    }

    Ok(Outcome {
        head_update,
        conflict_occurred,
    })
}

/// The index entries of one unresolved conflict, at stages 1/2/3 as Git stores them.
struct ConflictIndexEntries {
    base: Option<git2::IndexEntry>,
    ours: Option<git2::IndexEntry>,
    theirs: Option<git2::IndexEntry>,
}

impl ConflictIndexEntries {
    fn add_to(&self, index: &mut git2::Index) -> anyhow::Result<()> {
        index.conflict_add(self.base.as_ref(), self.ours.as_ref(), self.theirs.as_ref())?;
        Ok(())
    }
}

fn to_git2_index_entry(entry: &ConflictIndexEntry, path: &BStr) -> git2::IndexEntry {
    git2::IndexEntry {
        ctime: git2::IndexTime::new(0, 0),
        mtime: git2::IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: entry.mode.value() as u32,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: entry.id.to_git2(),
        flags: 0,
        flags_extended: 0,
        path: path.to_vec(),
    }
}

/// Bring `.git/index` in line with `new_tree` for the paths where `previous_tree` differs from it.
///
/// Operations that rewrite the head tree without checking anything out still owe Git an honest
/// index: without this, the index keeps whatever the previous operation left behind, and `git
/// status` / `git diff` describe changes that are neither staged nor uncommitted in a meaningful
/// way.
pub fn sync_index_with_tree(
    repo: &gix::Repository,
    previous_tree: gix::ObjectId,
    new_tree: gix::ObjectId,
) -> anyhow::Result<()> {
    let git2_repo = but_oxidize::open_git2_repo(repo.git_dir())?;
    let previous = git2_repo
        .find_object(previous_tree.to_git2(), None)?
        .peel_to_tree()?;
    let new = git2_repo
        .find_object(new_tree.to_git2(), None)?
        .peel_to_tree()?;
    sync_index_with_head(&git2_repo, &previous, &new, &[], &[])
}

/// Bring `.git/index` in line with `new_tree`, for the paths where `previous_tree` differs from it,
/// plus `rewritten_paths` (paths the checkout rewrote in the worktree).
///
/// Every other index entry is state Git was left in - staged changes, renames, `git rm --cached` -
/// and survives untouched. Unresolved conflicts keep their stages 1/2/3.
fn sync_index_with_head(
    git2_repo: &git2::Repository,
    previous_tree: &git2::Tree<'_>,
    new_tree: &git2::Tree<'_>,
    rewritten_paths: &[PathBuf],
    conflicts: &[ConflictIndexEntries],
) -> anyhow::Result<()> {
    let mut paths = changed_paths(git2_repo, previous_tree, new_tree)?;
    paths.extend_from_slice(rewritten_paths);
    paths.sort();
    paths.dedup();
    if paths.is_empty() && conflicts.is_empty() {
        return Ok(());
    }
    // Entries are taken from an index built from the tree, so their flags are what Git expects for
    // these paths, and their stat data is empty, which makes Git re-check the content.
    let mut from_tree = git2::Index::new()?;
    from_tree.read_tree(new_tree)?;

    let mut index = git2_repo.index()?;
    for path in &paths {
        match from_tree.get_path(path, 0) {
            Some(entry) => {
                index.add(&entry)?;
            }
            None => {
                // The path is gone from the checked out tree, e.g. a file became a directory.
                if let Err(err) = index.remove_path(path)
                    && err.code() != git2::ErrorCode::NotFound
                {
                    return Err(err.into());
                }
            }
        }
    }
    for entries in conflicts {
        entries.add_to(&mut index)?;
    }
    index.write()?;
    Ok(())
}

/// The paths at which `from` and `to` differ, in either direction.
fn changed_paths(
    git2_repo: &git2::Repository,
    from: &git2::Tree<'_>,
    to: &git2::Tree<'_>,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for delta in git2_repo
        .diff_tree_to_tree(Some(from), Some(to), None)?
        .deltas()
    {
        if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) {
            paths.push(path.to_owned());
        }
    }
    Ok(paths)
}
