//! This module makes an attempt to build a push implementation that is as
//! non-legacy as possible without doing some larger re-plumbing around the
//! gitbutler-git crate.
//!
//! This module should avoid legacy data structures where possible.

use std::str::FromStr as _;

use anyhow::{Context as _, Result};
use bstr::{BStr, BString, ByteSlice, ByteVec};
use but_core::extract_remote_name_and_short_name;
use but_db::DbHandle;
use gitbutler_git::{PushResult, push_with_askpass};
use gitbutler_reference::RemoteRefname;
use gitbutler_repo::hooks;
use gix::refs::Category;
use indexmap::IndexMap;

use crate::{RefInfo, ref_info::Segment, ui::PushStatus};

/// How much of a branch's lane chain a push transfers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PushScope {
    /// Push the selected branch and every segment beneath it, the default for stacks.
    #[default]
    BranchAndAncestors,
    /// Push only the selected branch itself, leaving its ancestors untouched. This is the
    /// `but pr new --single` path for agents and automation that must not publish the
    /// branches their work happens to rest on.
    BranchOnly,
}

/// Push a given branch and its ancestors
#[allow(clippy::too_many_arguments)]
pub fn workspace_branch_and_ancestors_push(
    repo: &gix::Repository,
    ws: &but_graph::Workspace,
    ref_info: &RefInfo,
    db: &mut DbHandle,
    gerrit_mode: bool,
    with_force: bool,
    skip_force_push_protection: bool,
    force_push_protection: bool,
    branch: &gix::refs::FullNameRef,
    push_scope: PushScope,
    run_hooks: bool,
    run_husky_hooks: bool,
    push_opts: Vec<but_gerrit::PushFlag>,
) -> Result<PushResult> {
    let graph = &ws.graph;
    let to_push = match push_scope {
        PushScope::BranchAndAncestors => branch_and_ancestor_segments(ref_info, branch),
        PushScope::BranchOnly => branch_only_segment(ref_info, branch),
    };

    let remote_names = repo.remote_names();
    let target_ref_name = ws
        .target_ref_name()
        .context("failed to get target reference name")?
        .to_owned();
    let push_remote = ws
        .push_remote_name()
        .context("failed to get target push remote name")?;
    let target_branch_name =
        target_branch_name_from_ref_name(target_ref_name.as_ref(), &remote_names)?;
    let mut result = PushResult {
        remote: push_remote.clone(),
        branch_to_remote: vec![],
        branch_sha_updates: vec![],
    };

    for (sidx, segment) in to_push.iter().rev() {
        let Some(ref_name) = segment
            .ref_name()
            .filter(|name| name.category() == Some(Category::LocalBranch))
        else {
            continue;
        };

        if matches!(
            segment.push_status,
            PushStatus::Integrated | PushStatus::NothingToPush
        ) {
            continue;
        }

        let Some(local_sha) = graph.tip_skip_empty(*sidx) else {
            continue;
        };

        let remote_refname = match &segment.remote_tracking_ref_name {
            Some(r) => r.clone(),
            None => format_remote_refname(ref_name, &push_remote)?,
        };
        let (remote_name, remote_branch_name) =
            extract_remote_name_and_short_name(remote_refname.as_ref(), &remote_names)
                .with_context(|| {
                    format!("failed to determine remote name for `{remote_refname}`")
                })?;
        let before_sha = remote_before_sha(repo, remote_refname.as_ref())?;
        let remote = repo.find_remote(remote_name.as_str())?;
        let remote_url = remote
            .url(gix::remote::Direction::Push)
            .or_else(|| remote.url(gix::remote::Direction::Fetch))
            .with_context(|| format!("Remote named {remote_name} didn't have a URL"))?;

        if run_hooks {
            match hooks::pre_push(
                repo,
                &remote_name,
                &remote_url.to_bstring().to_str_lossy(),
                local_sha.id,
                &RemoteRefname::from_str(&remote_refname.as_bstr().to_str_lossy())?,
                run_husky_hooks,
            )? {
                hooks::HookResult::Success | hooks::HookResult::NotConfigured => Ok(()),
                hooks::HookResult::Failure(error_data) => Err(anyhow::anyhow!(
                    "pre-push hook failed: {}",
                    error_data.error
                )),
            }?;
        }

        let gerrit_push_args = gerrit_push_args(
            gerrit_mode,
            local_sha.id,
            target_branch_name.as_bstr(),
            &push_opts,
        );
        let push_output = push_with_askpass(
            repo,
            local_sha.id,
            remote_refname.as_ref(),
            with_force,
            force_push_protection && !skip_force_push_protection,
            gerrit_push_args.refspec,
            // Historically we have tried to pass a stackId to the frontend when
            // doing askpass... but as far as I can tell, it's never used in a
            // meaningful way and doesn't seem to actually be required.
            Some(None),
            gerrit_push_args.push_opts,
        )?;

        maybe_record_gerrit_push_metadata(repo, db, gerrit_mode, segment, &push_output)?;
        let branch_name = ref_name.shorten().to_str_lossy().to_string();
        result.branch_to_remote.push((
            branch_name.clone(),
            remote_refname,
            remote_branch_name.to_str_lossy().to_string(),
        ));
        result.branch_sha_updates.push((
            branch_name,
            before_sha.to_string(),
            local_sha.id.to_string(),
        ));
    }

    Ok(result)
}

/// The commit `but push <branch>` publishes for `branch`, or `None` if the branch names no segment
/// of the workspace at all.
///
/// Resolution uses [`but_graph::Graph::tip_skip_empty()`] on the branch's own segment, the same step
/// the push loop performs for the ref it sends, so it cannot drift from what a push publishes.
pub fn pushable_branch_tip<'graph>(
    graph: &'graph but_graph::Graph,
    branch: &gix::refs::FullNameRef,
) -> Option<&'graph but_graph::Commit> {
    let segment = graph.segment_by_ref_name(branch)?;
    graph.tip_skip_empty(segment.id)
}

/// Return the segment of the selected local branch and every segment beneath it in top-to-base
/// order, crossing into the lane a worktree rests on.
///
/// This is the logical scope of a push operation: every commit the push transfers sits in one of
/// these segments, whether or not a branch names it. Only segments named by a local branch are
/// pushed as refs, and [`workspace_branch_and_ancestors_push()`] skips those already current on
/// the remote.
pub fn branch_and_ancestor_segments<'a>(
    ref_info: &'a RefInfo,
    branch: &gix::refs::FullNameRef,
) -> IndexMap<but_graph::SegmentIndex, &'a Segment> {
    ref_info
        .lane_chain(branch)
        .into_iter()
        .flat_map(|(lane, index)| lane.segments_from(index))
        .map(|segment| (segment.id, segment))
        .collect()
}

/// Return only the segment the selected local branch names, without the segments beneath it.
///
/// This is the single-branch push scope ([`PushScope::BranchOnly`]): only the selected
/// branch's ref is pushed, and the branches it rests on stay untouched.
pub fn branch_only_segment<'a>(
    ref_info: &'a RefInfo,
    branch: &gix::refs::FullNameRef,
) -> IndexMap<but_graph::SegmentIndex, &'a Segment> {
    ref_info
        .lane_chain(branch)
        .into_iter()
        .take(1)
        .flat_map(|(lane, index)| {
            lane.segments
                .get(index..index + 1)
                .unwrap_or_default()
                .iter()
        })
        .map(|segment| (segment.id, segment))
        .collect()
}

struct GerritPushArgs {
    refspec: Option<String>,
    push_opts: Vec<String>,
}

fn gerrit_push_args(
    gerrit_mode: bool,
    head: gix::ObjectId,
    target_branch_name: &BStr,
    push_flags: &[but_gerrit::PushFlag],
) -> GerritPushArgs {
    if gerrit_mode {
        GerritPushArgs {
            refspec: Some(format!("{head}:refs/for/{target_branch_name}")),
            push_opts: push_flags.iter().map(|flag| flag.to_string()).collect(),
        }
    } else {
        GerritPushArgs {
            refspec: None,
            push_opts: vec![],
        }
    }
}

/// Derive the target branch name while tolerating stale target remotes.
fn target_branch_name_from_ref_name(
    target_ref_name: &gix::refs::FullNameRef,
    remote_names: &gix::remote::Names,
) -> Result<BString> {
    let (category, shorthand_name) = target_ref_name
        .category_and_short_name()
        .context("Target branch could not be categorized")?;
    if matches!(category, Category::RemoteBranch) {
        if let Some((_remote, short_name)) =
            extract_remote_name_and_short_name(target_ref_name, remote_names)
        {
            return Ok(short_name);
        }
        let remote_ref: RemoteRefname = target_ref_name.to_string().parse()?;
        return Ok(remote_ref.branch().into());
    }
    Ok(shorthand_name.to_owned())
}

fn maybe_record_gerrit_push_metadata(
    repo: &gix::Repository,
    db: &mut DbHandle,
    gerrit_mode: bool,
    segment: &Segment,
    push_output: &str,
) -> Result<()> {
    if !gerrit_mode {
        return Ok(());
    }

    let push_output = but_gerrit::parse::push_output(push_output)?;
    but_gerrit::record_push_metadata(
        repo,
        db,
        segment.commits.iter().map(|c| c.id).collect::<Vec<_>>(),
        push_output,
    )
}

fn format_remote_refname(
    reference: &gix::refs::FullNameRef,
    remote_name: &str,
) -> Result<gix::refs::FullName> {
    let mut out: BString = b"refs/remotes/".into();
    out.push_str(format!("{remote_name}/"));
    out.push_str(reference.shorten());

    Ok(out.try_into()?)
}

fn remote_before_sha(
    repo: &gix::Repository,
    remote_refname: &gix::refs::FullNameRef,
) -> Result<gix::ObjectId> {
    Ok(repo
        .try_find_reference(remote_refname)?
        .map(|mut reference| reference.peel_to_commit())
        .transpose()?
        .map(|commit| commit.id)
        .unwrap_or(repo.object_hash().null()))
}
