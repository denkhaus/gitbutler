//! Print the commit a push would publish for a branch.

use but_api::json::HexHash;
use but_ctx::Context;
use gix::refs::{Category, FullName};
use serde::Serialize;

use crate::{
    CliResult, IdMap,
    args::sha,
    command::legacy::push,
    error::bad_input,
    theme::Theme,
    utils::{CliOutput, CliOutputHuman, WriteWithUtils},
};

pub fn sha(ctx: &mut Context, args: sha::Platform) -> CliResult<ShaOutcome> {
    // Resolve the arguments into an operation.
    let op = resolve(ctx, args)?;

    // Run the operation.
    Ok(run(ctx, op)?)
}

fn resolve(ctx: &mut Context, args: sha::Platform) -> CliResult<ShaOperation> {
    let sha::Platform { branch_id } = args;

    let branch_name = {
        let guard = ctx.shared_worktree_access();
        let id_map = IdMap::new_from_context(ctx, guard.read_permission())?;
        push::resolve_branch_name(ctx, &id_map, &branch_id.0)?
    };
    let branch = Category::LocalBranch.to_full_name(branch_name.as_str())?;

    // Reject a branch that names no segment of the workspace before doing any work: it has no
    // pushable commit, and saying so beats printing a SHA that no remote can resolve.
    if !has_pushable_commit(ctx, branch.as_ref())? {
        return Err(bad_input(format!(
            "Branch '{branch_name}' is not part of the workspace, so it has no pushable commit"
        ))
        .hint("Run `but status` to see the branches of this workspace")
        .into());
    }

    Ok(ShaOperation { branch })
}

fn run(ctx: &mut Context, op: ShaOperation) -> anyhow::Result<ShaOutcome> {
    let ShaOperation { branch } = op;

    let guard = ctx.shared_worktree_access();
    let (_repo, ws, _db) = ctx.workspace_and_db_with_perm(guard.read_permission())?;
    let Some(tip) = but_workspace::legacy::pushable_branch_tip(&ws.graph, branch.as_ref()) else {
        // `resolve` already rejected that case, so the workspace changed in the meantime.
        anyhow::bail!(
            "Branch '{branch}' is not part of the workspace, so it has no pushable commit"
        );
    };

    Ok(ShaOutcome {
        branch: branch.shorten().to_string(),
        sha: tip.id,
    })
}

/// Whether `branch` names a segment of the workspace with a commit a push would publish.
fn has_pushable_commit(ctx: &Context, branch: &gix::refs::FullNameRef) -> anyhow::Result<bool> {
    let guard = ctx.shared_worktree_access();
    let (_repo, ws, _db) = ctx.workspace_and_db_with_perm(guard.read_permission())?;
    Ok(but_workspace::legacy::pushable_branch_tip(&ws.graph, branch).is_some())
}

/// One branch's pushable commit.
struct ShaOperation {
    /// The local branch whose tip a push would publish, like `refs/heads/foo`.
    branch: FullName,
}

/// The pushable commit of one branch.
#[must_use]
pub struct ShaOutcome {
    /// The short name of the branch, like `foo`.
    branch: String,
    /// The commit a push of `branch` publishes.
    sha: gix::ObjectId,
}

impl CliOutputHuman for ShaOutcome {
    fn on_human(
        self,
        out: &mut dyn WriteWithUtils,
        _agent: bool,
        _theme: &Theme,
    ) -> anyhow::Result<()> {
        let Self { sha, .. } = self;

        // Deliberately the bare, full, uncoloured SHA on its own line: release tooling consumes
        // this value as `$(but sha <branch>)`. `theme::Commit` shortens and colours it.
        writeln!(out, "{sha}")?;

        Ok(())
    }
}

impl CliOutput for ShaOutcome {
    fn on_json(self) -> impl serde::Serialize {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Output {
            branch: String,
            sha: HexHash,
        }

        let Self { branch, sha } = self;

        Output {
            branch,
            sha: sha.into(),
        }
    }
}
