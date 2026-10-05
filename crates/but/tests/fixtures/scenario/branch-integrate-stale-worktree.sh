#!/usr/bin/env bash

source "${BASH_SOURCE[0]%/*}/shared.sh"

git-init-frozen
commit-file M
setup_target_to_match_main

git checkout -b A
commit-file only-on-remote
remote_tracking_caught_up A
create_workspace_commit_once A

# A plain-git fast-forward of the lane: the branch ref and its remote move while the GitButler
# workspace commit stays behind, so the new file is in both refs but not in the worktree.
git checkout -b plain-git A
commit-file only-on-plain-git
git update-ref refs/remotes/origin/A plain-git
git update-ref refs/heads/A plain-git
git checkout gitbutler/workspace
git branch -D plain-git
