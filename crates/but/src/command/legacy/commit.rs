use anyhow::Context as _;
use but_api::{
    WorkspaceState,
    json::{ChangeIdString, HexHash},
};
use but_core::{
    DiffSpec, DryRun, RefMetadata,
    ref_metadata::StackId,
    sync::{RepoExclusive, RepoExclusiveGuard},
};
use but_ctx::Context;
use but_rebase::graph_rebase::mutate::{InsertSide, RelativeTo};
use but_transaction::{IntermediateCommitCreateResult, Transaction};
use but_workspace::{RefInfo, commit::ChangeSource};
use gitbutler_oplog::entry::{OperationKind, SnapshotDetails};
use gix::refs::FullName;
use nonempty::NonEmpty;
use serde::Serialize;

use crate::{
    CliError, CliId, CliResult, CliResultExt, IdMap,
    args::{
        atoms::{BranchArg, BranchOrCommit, CliIdArg, Purpose, ResolvedCliIdArg},
        commit::Platform,
    },
    bad_input,
    command::legacy::{
        reword2::CommitMessageSource,
        status::{Selectable, TuiOutcome, TuiRunOptions, tui_with_options},
    },
    error::BadInput,
    id::{CommitId, IdAndHunk, UncommittedHunkOrFile},
    theme::{self, Theme},
    utils::{
        CliOutput, CliOutputHuman, IntermediateChannel, OutputChannel, WriteWithUtils,
        change_source::{ChangeSourceId, ChangeSourceRepo, InvokedFrom, UncommittedSelection},
        diff_rendering,
        diff_specs::DiffSpecBuilder,
        merged_upstream::MergedUpstream,
        patch_selection, rejection,
        single_branch_mode::{
            HowToCreateStackedReference, HowToCreateUnstackedReference, SingleBranchMode,
        },
        targeting::Side,
        worktrees::{worktree_branch, worktree_tip_target},
    },
};

#[derive(Debug, Clone)]
#[must_use]
pub struct CommitOutcome {
    pub new_commit: CommitId,
    pub branch_name: Option<BranchNameTarget>,
    pub(crate) changed_path_count: usize,
}

/// `--json` should only include newly created things. So if the branch already existed it
/// wont be included in the JSON output.
#[derive(Debug, Clone)]
pub enum BranchNameTarget {
    Existing(FullName),
    New(FullName),
}

impl CliOutputHuman for CommitOutcome {
    fn on_human(
        self,
        out: &mut dyn WriteWithUtils,
        _agent: bool,
        _theme: &Theme,
    ) -> anyhow::Result<()> {
        let Self {
            new_commit,
            branch_name,
            changed_path_count: _,
        } = self;

        match branch_name {
            Some(BranchNameTarget::New(branch_name)) => writeln!(
                out,
                "Created commit {} on new branch {}",
                theme::Commit(new_commit),
                theme::Branch(branch_name),
            )?,
            Some(BranchNameTarget::Existing(branch_name)) => writeln!(
                out,
                "Created commit {} on branch {}",
                theme::Commit(new_commit),
                theme::Branch(branch_name),
            )?,
            None => writeln!(out, "Created commit {}", theme::Commit(new_commit))?,
        }

        Ok(())
    }
}

impl CliOutput for CommitOutcome {
    fn on_json(self) -> impl serde::Serialize {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Output {
            commit_id: HexHash,
            #[serde(skip_serializing_if = "Option::is_none")]
            change_id: Option<ChangeIdString>,
            #[serde(skip_serializing_if = "Option::is_none")]
            branch: Option<String>,
        }

        let Self {
            new_commit,
            branch_name,
            changed_path_count: _,
        } = self;

        let branch_name = match branch_name {
            Some(BranchNameTarget::New(branch_name)) => Some(branch_name.shorten().to_string()),
            _ => None,
        };

        Output {
            commit_id: new_commit.commit_id.into(),
            change_id: new_commit.change_id.map(Into::into),
            branch: branch_name,
        }
    }
}

pub fn commit(
    ctx: &mut Context,
    mut out: IntermediateChannel<'_>,
    args: Platform,
    invoked_from: &InvokedFrom,
) -> CliResult<(CommitOutcome, WorkspaceState)> {
    // Read before anything else: a missing patch file or unreadable stdin is bad input, not a
    // repository error.
    let patch = args
        .patch
        .as_ref()
        .map(|path| read_patch(path))
        .transpose()?;
    if args.hunks {
        // A listing is the command's whole result: it is written here, and the command ends
        // instead of returning the commit outcome the dispatcher would print next. Bad input
        // returns above, so it still takes the regular error path.
        list_hunks(
            ctx,
            out.output_channel(),
            &args,
            patch.as_deref(),
            invoked_from,
        )?;
        std::io::Write::flush(&mut std::io::stdout()).ok();
        std::process::exit(0);
    }

    let guard = ctx.exclusive_worktree_access();
    let mut meta = ctx.meta()?;
    let id_map = IdMap::new_from_context(ctx, guard.read_permission())?;

    let (mut guard, commit_op, commit_selection, reword_op) = {
        let head_info = but_api::legacy::workspace::head_info(ctx)?;
        resolve(
            guard,
            ctx,
            args,
            patch,
            &mut out,
            &head_info,
            &id_map,
            invoked_from,
        )?
    };
    Ok(run(
        ctx,
        &mut meta,
        guard.write_permission(),
        commit_op,
        commit_selection,
        reword_op,
    )?)
}

/// Read a `--patch` argument: `-` is stdin, anything else a file path.
fn read_patch(path: &std::path::Path) -> CliResult<Vec<u8>> {
    if path == std::path::Path::new("-") {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)
            .context("Failed to read the patch from stdin")?;
        Ok(buf)
    } else {
        std::fs::read(path)
            .with_context(|| format!("Failed to read the patch file: {}", path.display()))
            .map_err(Into::into)
    }
}

/// `--patch`: select exactly the hunks the patch shows, matched against the current
/// uncommitted changes of the checkout the command runs in.
fn select_patch_hunks(
    patch: Vec<u8>,
    id_map: &IdMap,
    invoked_from: &InvokedFrom,
) -> CliResult<NonEmpty<UncommittedHunkOrFile>> {
    let files = patch_selection::parse(&patch)?;
    let source = invoked_from.managed_source(id_map)?;
    let current = id_map.uncommitted_files_in(&source);
    let selected = patch_selection::match_current_hunks(files, &current)?;
    NonEmpty::from_vec(selected).ok_or_else(|| bad_input("the patch selects no hunks").into())
}

#[expect(clippy::too_many_arguments)]
fn resolve(
    guard: RepoExclusiveGuard,
    ctx: &mut Context,
    args: Platform,
    patch: Option<Vec<u8>>,
    out: &mut IntermediateChannel<'_>,
    head_info: &RefInfo,
    id_map: &IdMap,
    invoked_from: &InvokedFrom,
) -> CliResult<(
    RepoExclusiveGuard,
    CommitOperation,
    CommitSelection,
    CommitMessageSource,
)> {
    let Platform {
        no_message,
        message,
        branch,
        empty,
        above,
        below,
        interactive,
        mut changes,
        file,
        hunks: _,
        patch: _,
        allow_merged,
        switch,
    } = args;
    // `--file` is a path-anchored selector for the same selection `CHANGES` names.
    changes.extend(file);

    if switch && !ctx.settings.feature_flags.single_branch {
        return Err(
            bad_input("`--switch` requires the `single-branch` feature to be enabled")
                .hint("Enable the feature with `but config feature single-branch enable`")
                .into(),
        );
    }

    let merged = MergedUpstream::new(&*ctx.repo.get()?, head_info, allow_merged);

    let target_ish = CommitOperationTargetIsh::resolve(branch, above, below)?;

    let (guard, commit_selection) = if let Some(patch) = patch {
        let selection = select_patch_hunks(patch, id_map, invoked_from)?;
        (
            guard,
            CommitSelection::Changes(Box::new(UncommittedSelection::new(selection)?)),
        )
    } else if !changes.is_empty() {
        let changes = changes
            .into_iter()
            .map(|change| {
                let repo = ctx.repo.get()?;
                match change.try_resolve_uncommitted(&repo, id_map)? {
                    Some(resolved) => Ok(resolved),
                    None => Err(unresolved_change_error(&change, &repo, id_map)),
                }
            })
            .collect::<CliResult<Vec<Vec<UncommittedHunkOrFile>>>>()?;
        let changes = changes.into_iter().flatten().collect();
        let Some(changes) = NonEmpty::from_vec(changes) else {
            return Err(bad_input("No changes to commit")
                .hint("Run `but status` to show applicable targets")
                .into());
        };
        (
            guard,
            CommitSelection::Changes(Box::new(UncommittedSelection::new(changes)?)),
        )
    } else if interactive {
        let Some(mut inout) = out.prepare_for_terminal_input() else {
            return Err(bad_input("Terminal doesn't support interactivity").into());
        };
        let (guard, outcome) = tui_with_options(
            ctx,
            guard,
            &mut inout,
            TuiRunOptions::PickChanges,
            invoked_from,
        )?;
        let ids = match outcome {
            TuiOutcome::Selection(ids) => ids,
            TuiOutcome::None => {
                return Err(bad_input("No changes to commit")
                    .hint("Pick changes by pressing space. Confirm with enter.")
                    .into());
            }
        };
        let changes = ids
            .into_iter()
            .map(|change| {
                match change {
                    Selectable::UncommittedHunkOrFile(id) => Ok(id),
                    _ => {
                        Err(anyhow::anyhow!("BUG: tui should only return uncommitted changes in PickChanges mode but got {change:?}"))
                    }
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some(changes) = NonEmpty::from_vec(changes) else {
            return Err(bad_input("No changes to commit")
                .hint("Pick changes by pressing space. Confirm with enter.")
                .into());
        };
        (
            guard,
            CommitSelection::Changes(Box::new(UncommittedSelection::new(changes)?)),
        )
    } else if empty {
        (guard, CommitSelection::Nothing)
    } else {
        (
            guard,
            CommitSelection::AllChanges(invoked_from.managed_source(id_map)?),
        )
    };

    let commit_op = {
        let (repo, ws, _db) = ctx.workspace_and_db_with_perm(guard.read_permission())?;
        let default_lane = || match &commit_selection {
            CommitSelection::AllChanges(_) | CommitSelection::Changes(_) => {
                Ok(commit_selection.source())
            }
            CommitSelection::Nothing => invoked_from.managed_source(id_map),
        };
        route_commit_operation(
            &repo,
            &ws,
            head_info,
            out,
            id_map,
            target_ish,
            default_lane,
            &merged,
            switch,
        )
        .map_err(|err| match err {
            RouteCommitOperationError::NoStackToCommitTo => {
                bad_input("Found no stack that could be committed to").into()
            }
            RouteCommitOperationError::UnclearTargetCantPrompt => {
                bad_input("Unclear where to commit. Found more than one stack")
                    .hint("You can specify where to commit with `--branch [<BRANCH>]`")
                    .into()
            }
            RouteCommitOperationError::Other(cli_error) => cli_error,
        })?
    };

    let reword_op = CommitMessageSource::from_args(no_message, message, out.format())?;

    Ok((guard, commit_op, commit_selection, reword_op))
}

/// The retired syntax put the target branch in positional position
/// (`but commit <branch> -m "message"`), which the modern grammar reads as a
/// change. When a change fails to resolve but names an applied branch,
/// suggest `-b` targeting instead of the generic missing-change hint.
fn unresolved_change_error(change: &CliIdArg, repo: &gix::Repository, id_map: &IdMap) -> CliError {
    let names_branch = change
        .parse(repo, id_map)
        .ok()
        .into_iter()
        .flatten()
        .any(|id| matches!(id, CliId::Branch(..)));
    let err = bad_input(format!("Could not find uncommitted change: '{change}'"));
    if names_branch {
        err.hint(format!(
            "'{change}' is a branch. To commit onto it, run `but commit -b {change} -m \"message\" [<change>...]`"
        ))
        .into()
    } else {
        err.hint(CliIdArg::CHANGE_MISSING_HINT).into()
    }
}

/// One entry of a `--hunks` listing: something `but diff` knows how to render.
enum HunkListing {
    /// Every uncommitted hunk of one checkout, as a bare `but diff` shows them.
    Checkout(ChangeSourceId),
    /// Every uncommitted hunk below a directory, as `but diff <dir>/` shows them.
    PathPrefix {
        id: String,
        hunks: NonEmpty<IdAndHunk>,
    },
    /// One file or hunk, as `but diff <file-or-hunk-id>` shows it.
    HunkOrFile(Box<UncommittedHunkOrFile>),
}

/// `--hunks`: render the selected changes exactly as `but diff` renders them, and don't commit.
///
/// The listing is the whole result of the command, so it goes through the output channel rather
/// than through a [`CommitOutcome`], which would have to describe a commit that was never made.
/// Selection errors are returned so the caller can report them like any other bad input.
fn list_hunks(
    ctx: &mut Context,
    out: &mut OutputChannel,
    args: &Platform,
    patch: Option<&[u8]>,
    invoked_from: &InvokedFrom,
) -> CliResult<()> {
    if out.is_json() {
        // Reporting would print nothing at all, which a machine caller cannot tell apart from an
        // empty selection.
        return Err(
            bad_input("`--hunks` prints a diff listing, which has no JSON form")
                .hint("Run `but diff --json` to read hunk IDs as JSON")
                .into(),
        );
    }
    let id_map = {
        let guard = ctx.shared_worktree_access();
        IdMap::new_from_context(ctx, guard.read_permission())?
    };

    if let Some(patch) = patch {
        // The patch listing previews the same selection a commit would take: exactly the
        // matched hunks, with their current IDs.
        let selected = select_patch_hunks(patch.to_vec(), &id_map, invoked_from)?;
        let options = diff_rendering::Options {
            skip_commit_header: true,
            skip_line_stats: true,
        };
        let theme = theme::get();
        let Some(human) = out.for_human_or_shell() else {
            return Ok(());
        };
        diff_rendering::with_human_diff_writer(human, theme, |id_gen, writer| {
            for hunk in selected {
                diff_rendering::render_uncommitted_hunk(
                    UncommittedHunkOrFile {
                        id: hunk.id,
                        hunks: hunk.hunks,
                        is_entire_file: hunk.is_entire_file,
                        source: hunk.source,
                    },
                    theme,
                    id_gen,
                    options,
                    writer,
                )?;
            }
            Ok(())
        })?;
        return Ok(());
    }

    // `--file` extends the change selection, exactly like naming the path as `CHANGES`.
    let selection = args.changes.iter().chain(&args.file).collect::<Vec<_>>();
    let mut listing = Vec::new();
    if selection.is_empty() {
        listing.push(HunkListing::Checkout(invoked_from.managed_source(&id_map)?));
    }
    for arg in selection {
        let repo = ctx.repo.get()?;
        match arg.try_resolve(&repo, &id_map, Purpose::Uncommitted, None)? {
            Some(ResolvedCliIdArg::UncommittedHunkOrFile(hunk)) => {
                listing.push(HunkListing::HunkOrFile(hunk));
            }
            Some(ResolvedCliIdArg::PathPrefix { id, hunks }) => {
                listing.push(HunkListing::PathPrefix { id, hunks });
            }
            Some(ResolvedCliIdArg::Uncommitted(source)) => {
                listing.push(HunkListing::Checkout(source));
            }
            // What is left has no hunks to list, so it is reported the way a commit reports a
            // selection it cannot take.
            Some(_) | None => return Err(unresolved_change_error(arg, &repo, &id_map)),
        }
    }

    let options = diff_rendering::Options {
        skip_commit_header: true,
        skip_line_stats: true,
    };
    let theme = theme::get();
    let Some(human) = out.for_human_or_shell() else {
        return Ok(());
    };
    diff_rendering::with_human_diff_writer(human, theme, |id_gen, writer| {
        for item in listing {
            match item {
                HunkListing::Checkout(source) => diff_rendering::render_uncommitted_source(
                    ctx, source, theme, id_gen, options, writer,
                )?,
                HunkListing::PathPrefix { id, hunks } => diff_rendering::render_path_prefix(
                    &id, hunks, ctx, theme, id_gen, options, writer,
                )?,
                HunkListing::HunkOrFile(hunk) => {
                    diff_rendering::render_uncommitted_hunk(*hunk, theme, id_gen, options, writer)?
                }
            }
        }
        Ok(())
    })?;

    Ok(())
}

pub fn run(
    ctx: &mut Context,
    meta: &mut impl RefMetadata,
    perm: &mut RepoExclusive,
    commit_op: CommitOperation,
    commit_selection: CommitSelection,
    reword_op: CommitMessageSource,
) -> anyhow::Result<(CommitOutcome, WorkspaceState)> {
    // Owned for the whole operation: the `ChangeSource` handed to the transaction
    // below borrows from it.
    let source_repo = ChangeSourceRepo::open(ctx, &commit_selection.source())?;

    let sbm = (commit_op.will_create_reference() || commit_op.switch())
        .then(|| SingleBranchMode::new(ctx, perm.read_permission(), commit_op.switch()))
        .transpose()?;

    let changes = {
        let context_lines = ctx.settings.context_lines;
        let (repo, ..) = ctx.workspace_and_db_mut_with_perm(perm.read_permission())?;

        // One repo per builder, which is also what keeps `reconcile_worktree_diff_specs`
        // from seeing a spec whose path is not among that checkout's changes.
        let mut builder = DiffSpecBuilder::for_change_source(&source_repo, &repo, context_lines);

        match commit_selection {
            CommitSelection::AllChanges(_) => {
                builder.push_changes_from_uncommitted_area()?;
            }
            CommitSelection::Changes(selection) => {
                for change in selection.into_changes() {
                    builder.push_changes_from_uncommitted(&change)?;
                }

                builder.reconcile_worktree_diff_specs()?;
            }
            CommitSelection::Nothing => {}
        }

        builder.into_diff_specs()
    };

    let changed_path_count = changes.len();
    let rejection_target = commit_op.rejection_target();
    let snapshot_details = SnapshotDetails::new(OperationKind::CreateCommit);

    let ((new_commit, branch_name), ws) = if let Some(sbm) = sbm {
        sbm.transaction_with_workspace_setup(
            ctx,
            meta,
            snapshot_details,
            perm,
            commit_op.will_create_unstacked_reference(),
            |tx| {
                commit_with_transaction(
                    tx,
                    commit_op,
                    changes,
                    source_repo.as_change_source(),
                    Some(&sbm),
                    reword_op,
                )
            },
        )
    } else {
        but_transaction::with_transaction_with_perm(
            ctx,
            meta,
            perm,
            snapshot_details,
            DryRun::No,
            |tx| {
                commit_with_transaction(
                    tx,
                    commit_op,
                    changes,
                    source_repo.as_change_source(),
                    sbm.as_ref(),
                    reword_op,
                )
            },
        )
    }
    .map_err(|err| rejection::explain_after_rollback(ctx, perm, "commit", rejection_target, err))?;

    let outcome = CommitOutcome {
        new_commit,
        branch_name,
        changed_path_count,
    };

    Ok((outcome, ws))
}

fn commit_with_transaction(
    mut tx: Transaction<'_, '_, impl RefMetadata>,
    commit_op: CommitOperation,
    changes: Vec<DiffSpec>,
    source: ChangeSource<'_>,
    sbm: Option<&SingleBranchMode>,
    reword_op: CommitMessageSource,
) -> anyhow::Result<but_transaction::Commit<(CommitId, Option<BranchNameTarget>)>> {
    let (
        IntermediateCommitCreateResult {
            new_commit,
            rejected_specs,
        },
        branch_name,
    ) = commit_op.execute(&mut tx, changes, source, sbm)?;

    if !rejected_specs.is_empty() {
        return Err(rejection::RejectedChanges(rejected_specs).into());
    }

    let new_commit =
        new_commit.context("BUG: rejected_specs is empty yet nothing was committed")?;

    let reworded_commit = reword_op.execute(new_commit.into(), &mut tx)?;

    Ok(but_transaction::Commit((reworded_commit, branch_name)))
}

/// Targeting modes for committing.
pub enum CommitOperationTargetIsh {
    /// Target the branch if it exists, or create it at the newest base if it does not.
    Branch(CliIdArg),
    /// Target newest base with a new canned branch name.
    UnstackedCannedBranch,
    /// Targets above the [`CliIdArg`], which must denote either a commit or a branch.
    Above {
        target: CliIdArg,
        /// If target is a branch, this specifies the name of the new branch to create.
        new_branch_name: Option<Option<CliIdArg>>,
    },
    /// Targets below the [`CliIdArg`], which must denote either a commit or a branch. For commits,
    /// this is directly below. For branches, this is below the segment.
    Below {
        target: CliIdArg,
        /// If target is a branch, this specifies the name of the new branch to create.
        new_branch_name: Option<Option<CliIdArg>>,
    },
    /// The default target, makes a sensible choice about where to put the commit, creating a branch
    /// if necessary. This should be used if there is no input from the user about where to put the
    /// commit.
    Default,
}

impl CommitOperationTargetIsh {
    pub fn resolve(
        branch: Option<Option<CliIdArg>>,
        above: Option<CliIdArg>,
        below: Option<CliIdArg>,
    ) -> CliResult<Self> {
        Ok(match (branch, above, below) {
            (Some(Some(branch)), None, None) => CommitOperationTargetIsh::Branch(branch),
            (Some(None), None, None) => CommitOperationTargetIsh::UnstackedCannedBranch,
            (branch, Some(cli_id), None) => CommitOperationTargetIsh::Above {
                target: cli_id,
                new_branch_name: branch,
            },
            (branch, None, Some(cli_id)) => CommitOperationTargetIsh::Below {
                target: cli_id,
                new_branch_name: branch,
            },
            (None, None, None) => CommitOperationTargetIsh::Default,
            _ => {
                return Err(anyhow::anyhow!(
                    "BUG: Should not be able to supply more than one of above, below or branch"
                )
                .into());
            }
        })
    }
}

/// `default_lane` names the checkout whose lane a commit without a target goes to. It is only
/// asked for then, so a checkout that can't be named fails just the commits relying on it.
#[expect(clippy::too_many_arguments)]
pub fn route_commit_operation(
    repo: &gix::Repository,
    ws: &but_graph::Workspace,
    head_info: &RefInfo,
    out: &mut IntermediateChannel<'_>,
    id_map: &IdMap,
    target: CommitOperationTargetIsh,
    default_lane: impl FnOnce() -> CliResult<ChangeSourceId>,
    merged: &MergedUpstream,
    switch: bool,
) -> Result<CommitOperation, RouteCommitOperationError> {
    match target {
        CommitOperationTargetIsh::Above {
            target,
            new_branch_name,
        } => {
            let side = Side::Above;
            Ok(route_commit_above_or_below(
                repo,
                ws,
                id_map,
                target,
                side,
                merged,
                new_branch_name,
                switch,
            )?)
        }
        CommitOperationTargetIsh::Below {
            target,
            new_branch_name,
        } => {
            let side = Side::Below;
            Ok(route_commit_above_or_below(
                repo,
                ws,
                id_map,
                target,
                side,
                merged,
                new_branch_name,
                switch,
            )?)
        }
        CommitOperationTargetIsh::Branch(cli_id) => {
            if let Some(branch) = cli_id.try_resolve_branch(repo, id_map)? {
                let segment = branch.resolve_segment(head_info)?;
                let ref_info = segment.ref_info.with_context(|| {
                    format!("BUG: Segment resolved from branch name {branch} has no ref info")
                })?;
                merged.ensure_branch_not_merged(ref_info.ref_name.as_ref())?;

                let target = CommitRelativeToTarget::BranchTip {
                    name: ref_info.ref_name,
                    switch,
                };

                Ok(CommitOperation::CommitAt(CommitAtOperation { target }))
            } else {
                let branch = BranchArg(cli_id.0);
                let branch_name = branch
                    .resolve_for_creation(repo, ws)
                    .with_hint(|| format!("Run `but apply {branch}` to apply the branch first"))?;
                Ok(CommitOperation::CommitToNewBranch(
                    CommitToNewBranchOperation {
                        branch_name: Some(branch_name),
                        switch,
                    },
                ))
            }
        }
        CommitOperationTargetIsh::UnstackedCannedBranch => Ok(CommitOperation::CommitToNewBranch(
            CommitToNewBranchOperation {
                branch_name: None,
                switch,
            },
        )),
        CommitOperationTargetIsh::Default => {
            // A worktree's lane defaults to where the TUI's heading gesture puts changes:
            // the tip of the branch checked out there, not a workspace stack.
            if let ChangeSourceId::Worktree(name) = default_lane()? {
                // The user didn't pick this target, but a detached or otherwise
                // branchless worktree is still their input to fix.
                let name = worktree_branch(repo, name.as_ref())
                    .map_err(|err| CliError::from(bad_input(err.to_string())))?;
                merged.ensure_branch_not_merged(name.as_ref())?;
                return Ok(CommitOperation::CommitAt(CommitAtOperation {
                    target: CommitRelativeToTarget::BranchTip { name, switch },
                }));
            }
            // Branches that have landed upstream are not sensible default targets;
            // skip them so new work goes to a live branch or a fresh one.
            let stacks = head_info
                .stacks
                .iter()
                .filter(|stack| {
                    !stack
                        .segments
                        .first()
                        .is_some_and(|segment| merged.contains_segment(segment))
                })
                .collect::<Vec<_>>();

            match &stacks[..] {
                [] => Ok(CommitOperation::CommitToNewBranch(
                    CommitToNewBranchOperation {
                        branch_name: None,
                        switch,
                    },
                )),
                [stack] => {
                    let ref_info = stack
                        .segments
                        .first()
                        .and_then(|segment| segment.ref_info.as_ref())
                        .context("Head stack has no ref")?;
                    Ok(CommitOperation::CommitAt(CommitAtOperation {
                        target: CommitRelativeToTarget::BranchTip {
                            name: ref_info.ref_name.clone(),
                            switch,
                        },
                    }))
                }
                stacks => {
                    let stack_heads = stacks
                        .iter()
                        .flat_map(|stack| &stack.segments)
                        .filter_map(|segment| segment.ref_info.as_ref())
                        .filter(|ref_info| {
                            merged
                                .ensure_branch_not_merged(ref_info.ref_name.as_ref())
                                .is_ok()
                        })
                        .map(|ref_info| (ref_info.ref_name.shorten(), &ref_info.ref_name))
                        .collect::<Vec<_>>();

                    let Some(stack_heads) = NonEmpty::from_vec(stack_heads) else {
                        return Err(RouteCommitOperationError::NoStackToCommitTo);
                    };

                    let Some(mut input) = out.prepare_for_terminal_input() else {
                        return Err(RouteCommitOperationError::UnclearTargetCantPrompt);
                    };

                    let mut stack_heads =
                        stack_heads.map(|(name, branch)| (name, PickerItem::Branch(branch)));
                    stack_heads.push(("Create new stack".into(), PickerItem::NewStack));

                    let Some(selection) = input.prompt_select(
                        "Multiple stacks found. Choose one to commit to",
                        &stack_heads,
                    )?
                    else {
                        return Err(bad_input("No stack picked").into());
                    };

                    match selection {
                        PickerItem::Branch(full_name) => {
                            Ok(CommitOperation::CommitAt(CommitAtOperation {
                                target: CommitRelativeToTarget::BranchTip {
                                    name: (*full_name).clone(),
                                    switch,
                                },
                            }))
                        }
                        PickerItem::NewStack => Ok(CommitOperation::CommitToNewBranch(
                            CommitToNewBranchOperation {
                                branch_name: None,
                                switch,
                            },
                        )),
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum RouteCommitOperationError {
    NoStackToCommitTo,
    UnclearTargetCantPrompt,
    Other(CliError),
}

impl From<anyhow::Error> for RouteCommitOperationError {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err.into())
    }
}

impl From<CliError> for RouteCommitOperationError {
    fn from(value: CliError) -> Self {
        Self::Other(value)
    }
}

impl From<BadInput> for RouteCommitOperationError {
    fn from(value: BadInput) -> Self {
        Self::Other(value.into())
    }
}

enum PickerItem<'a> {
    Branch(&'a FullName),
    NewStack,
}

#[expect(clippy::too_many_arguments)]
fn route_commit_above_or_below(
    repo: &gix::Repository,
    ws: &but_graph::Workspace,
    id_map: &IdMap,
    target: CliIdArg,
    side: Side,
    merged: &MergedUpstream,
    new_branch_name: Option<Option<CliIdArg>>,
    switch: bool,
) -> CliResult<CommitOperation> {
    if let Some(name) = target.try_resolve_worktree(repo, id_map)? {
        let name = worktree_tip_target(repo, name.as_ref(), side, &target)?;
        merged.ensure_branch_not_merged(name.as_ref())?;
        if new_branch_name.is_some() {
            return Err(bad_input(
                "Cannot use `-b/--branch` when committing relative to worktrees",
            )
            .into());
        }
        return Ok(CommitOperation::CommitAt(CommitAtOperation {
            target: CommitRelativeToTarget::BranchTip { name, switch },
        }));
    }
    let resolved = target
        .resolve_in_workspace(repo, id_map, Purpose::Target, None)
        .hint(
            "Target must be an applied branch or commit. Run `but status` for applicable targets.",
        )?;
    let target = match resolved {
        ResolvedCliIdArg::AnonymousSegment(segment) => {
            return Err(crate::args::atoms::anonymous_segment_error(&segment.id));
        }
        resolved => match resolved
            .into_branch_or_commit()
            .hint("Run `but status` to show applicable targets")?
        {
            BranchOrCommit::Commit(commit) => {
                merged.ensure_commit_not_merged(commit.commit_id)?;

                if new_branch_name.is_some() {
                    return Err(bad_input(
                        "Cannot use `-b/--branch` when committing relative to commits",
                    )
                    .into());
                }

                CommitRelativeToTarget::Commit { commit, side }
            }
            BranchOrCommit::Branch(arg) => {
                let name = arg.resolve_local_branch_name()?;

                let new_branch_name = new_branch_name
                    .flatten()
                    .map(|branch| BranchArg(branch.0).resolve_for_creation(repo, ws))
                    .transpose()?;

                merged.ensure_branch_not_merged(name.as_ref())?;
                CommitRelativeToTarget::BranchBucket {
                    name,
                    side,
                    new_branch_name,
                    switch,
                }
            }
        },
    };
    Ok(CommitOperation::CommitAt(CommitAtOperation { target }))
}

pub enum CommitSelection {
    /// Every uncommitted change of the named checkout, which is what a bare `but commit` and a
    /// worktree's own uncommitted area both mean.
    AllChanges(ChangeSourceId),
    Changes(Box<UncommittedSelection>),
    Nothing,
}

impl CommitSelection {
    /// The checkout these changes are read from, validated to be a single one
    /// at construction, see [`UncommittedSelection`].
    fn source(&self) -> ChangeSourceId {
        match self {
            CommitSelection::AllChanges(source) => source.clone(),
            // An empty commit reads no changes at all, so the main worktree stands in.
            CommitSelection::Nothing => ChangeSourceId::Head,
            CommitSelection::Changes(selection) => selection.source().clone(),
        }
    }
}

#[derive(Debug)]
pub enum CommitOperation {
    CommitToNewBranch(CommitToNewBranchOperation),
    CommitAt(CommitAtOperation),
}

impl CommitOperation {
    pub fn will_create_reference(&self) -> bool {
        match self {
            CommitOperation::CommitToNewBranch(op) => op.will_create_reference(),
            CommitOperation::CommitAt(op) => op.will_create_reference(),
        }
    }

    pub fn will_create_unstacked_reference(&self) -> bool {
        match self {
            CommitOperation::CommitToNewBranch(op) => op.will_create_unstacked_reference(),
            CommitOperation::CommitAt(op) => op.will_create_unstacked_reference(),
        }
    }

    pub fn switch(&self) -> bool {
        match self {
            CommitOperation::CommitToNewBranch(op) => op.switch,
            CommitOperation::CommitAt(op) => op.switch(),
        }
    }

    /// What the operation targets, for explaining rejected changes after a
    /// rollback.
    fn rejection_target(&self) -> rejection::Target {
        match self {
            CommitOperation::CommitToNewBranch(op) => rejection::Target::NewBranch(
                op.branch_name
                    .as_ref()
                    .map(|name| name.shorten().to_string()),
            ),
            CommitOperation::CommitAt(op) => match &op.target {
                CommitRelativeToTarget::Commit { commit, .. } => {
                    rejection::Target::Commit(commit.clone())
                }
                CommitRelativeToTarget::BranchTip { name, switch: _ } => {
                    rejection::Target::Branch(name.shorten().to_string())
                }
                CommitRelativeToTarget::BranchBucket {
                    name: _,
                    new_branch_name,
                    side: _,
                    switch: _,
                } => rejection::Target::NewBranch(
                    new_branch_name
                        .as_ref()
                        .map(|name| name.shorten().to_string()),
                ),
            },
        }
    }

    fn execute(
        self,
        tx: &mut Transaction<'_, '_, impl RefMetadata>,
        changes: Vec<DiffSpec>,
        source: ChangeSource<'_>,
        sbm: Option<&SingleBranchMode>,
    ) -> anyhow::Result<(IntermediateCommitCreateResult, Option<BranchNameTarget>)> {
        match self {
            CommitOperation::CommitToNewBranch(op) => op.execute(tx, changes, source, sbm),
            CommitOperation::CommitAt(op) => op.execute(tx, changes, source, sbm),
        }
    }
}

#[derive(Debug)]
pub struct CommitToNewBranchOperation {
    pub branch_name: Option<FullName>,
    pub switch: bool,
}

impl CommitToNewBranchOperation {
    pub fn will_create_reference(&self) -> bool {
        true
    }

    pub fn will_create_unstacked_reference(&self) -> bool {
        true
    }

    fn execute(
        self,
        tx: &mut Transaction<'_, '_, impl RefMetadata>,
        changes: Vec<DiffSpec>,
        source: ChangeSource<'_>,
        sbm: Option<&SingleBranchMode>,
    ) -> anyhow::Result<(IntermediateCommitCreateResult, Option<BranchNameTarget>)> {
        let branch_name = self.create_reference(tx, sbm)?;

        let commit_create_result = tx
            .create_commit(
                RelativeTo::Reference(branch_name.clone()),
                InsertSide::Below,
                changes,
                String::new(),
                source,
            )
            .context("failed to create commit")?;

        Ok((
            commit_create_result,
            Some(BranchNameTarget::New(branch_name)),
        ))
    }

    pub(crate) fn create_reference(
        self,
        tx: &mut Transaction<'_, '_, impl RefMetadata>,
        sbm: Option<&SingleBranchMode>,
    ) -> anyhow::Result<FullName> {
        let Self {
            branch_name,
            switch: _,
        } = self;

        let new_ref = if let Some(branch_name) = branch_name {
            branch_name
        } else {
            but_core::branch::unique_canned_refname(tx.repo())?
        };

        match sbm
            .context("BUG: sbm must always be set when committing to a new unstacked branch")?
            .how_to_create_unstacked_reference()
        {
            HowToCreateUnstackedReference::Normally => {
                tx.create_reference(new_ref.as_ref(), None, |_| StackId::generate(), Some(0))?;
            }
            HowToCreateUnstackedReference::CreateRefAtAnchorThenCheckout(anchor) => {
                tx.create_reference(new_ref.as_ref(), anchor, |_| StackId::generate(), Some(0))?;
                tx.checkout(new_ref.as_ref())?;
            }
            HowToCreateUnstackedReference::CreateRefAtCommitThenCheckout { target_commit_id } => {
                tx.create_reference_at_commit(new_ref.as_ref(), target_commit_id)?;

                tx.checkout(new_ref.as_ref())?;
            }
        }

        Ok(new_ref)
    }
}

#[derive(Debug)]
pub struct CommitAtOperation {
    pub target: CommitRelativeToTarget,
}

impl CommitAtOperation {
    fn execute(
        self,
        tx: &mut Transaction<'_, '_, impl RefMetadata>,
        changes: Vec<DiffSpec>,
        source: ChangeSource<'_>,
        sbm: Option<&SingleBranchMode>,
    ) -> anyhow::Result<(IntermediateCommitCreateResult, Option<BranchNameTarget>)> {
        let (relative_to, side, branch_name_target) = self.create_target(tx, sbm)?;

        let commit_create_result =
            tx.create_commit(relative_to.clone(), side, changes, String::new(), source)?;

        Ok((commit_create_result, branch_name_target))
    }

    pub fn will_create_reference(&self) -> bool {
        match &self.target {
            CommitRelativeToTarget::Commit { .. } | CommitRelativeToTarget::BranchTip { .. } => {
                false
            }
            CommitRelativeToTarget::BranchBucket { .. } => true,
        }
    }

    pub fn will_create_unstacked_reference(&self) -> bool {
        match &self.target {
            CommitRelativeToTarget::Commit { .. }
            | CommitRelativeToTarget::BranchTip { .. }
            | CommitRelativeToTarget::BranchBucket { .. } => false,
        }
    }

    pub fn switch(&self) -> bool {
        match &self.target {
            CommitRelativeToTarget::Commit { .. } => false,
            CommitRelativeToTarget::BranchTip { switch, .. }
            | CommitRelativeToTarget::BranchBucket { switch, .. } => *switch,
        }
    }

    pub fn create_target(
        &self,
        tx: &mut Transaction<'_, '_, impl RefMetadata>,
        sbm: Option<&SingleBranchMode>,
    ) -> anyhow::Result<(RelativeTo, InsertSide, Option<BranchNameTarget>)> {
        Ok(match &self.target {
            CommitRelativeToTarget::Commit { commit, side } => {
                (RelativeTo::Commit(commit.commit_id), (*side).into(), None)
            }
            CommitRelativeToTarget::BranchBucket {
                name,
                side,
                new_branch_name,
                switch: _,
            } => {
                let new_branch_name = if let Some(new_branch_name) = new_branch_name {
                    new_branch_name.clone()
                } else {
                    but_core::branch::unique_canned_refname(tx.repo())?
                };

                match sbm
                    .context("BUG: sbm must always be set when committing to a new stacked branch")?
                    .how_to_create_stacked_reference(name.as_ref(), *side)
                {
                    HowToCreateStackedReference::Normally(anchor) => {
                        tx.create_reference(
                            new_branch_name.as_ref(),
                            Some(anchor),
                            |_| StackId::generate(),
                            Some(0),
                        )?;
                    }
                    HowToCreateStackedReference::CreateRefAtAnchorThenCheckout(anchor) => {
                        tx.create_reference(
                            new_branch_name.as_ref(),
                            Some(anchor),
                            |_| StackId::generate(),
                            Some(0),
                        )?;
                        tx.checkout(new_branch_name.as_ref())?;
                    }
                }

                let where_to_create_commit = InsertSide::Below;
                (
                    RelativeTo::Reference(new_branch_name.clone()),
                    where_to_create_commit,
                    Some(BranchNameTarget::New(new_branch_name)),
                )
            }
            CommitRelativeToTarget::BranchTip { name, switch } => {
                if *switch {
                    tx.checkout(name.as_ref())?;
                }

                let where_to_create_commit = InsertSide::Below;
                (
                    RelativeTo::Reference(name.clone()),
                    where_to_create_commit,
                    Some(BranchNameTarget::Existing(name.clone())),
                )
            }
        })
    }
}

/// Place a commit relative to something in the workspace.
#[derive(Clone, Debug)]
pub enum CommitRelativeToTarget {
    /// Place the commit relative to this commit, within the same branch.
    Commit { commit: CommitId, side: Side },
    /// Place the commit at the tip of the branch denoted by this reference, moving the reference to
    /// the new commit. This is effectively the same as committing to a branch.
    BranchTip { name: FullName, switch: bool },
    /// Place the commit relative to this branch, treating the branch as a bucket.
    ///
    /// The commit is always inserted on a new branch. If `new_branch_name` is `None` a canned name
    /// will be generated.
    BranchBucket {
        name: FullName,
        new_branch_name: Option<FullName>,
        side: Side,
        switch: bool,
    },
}
