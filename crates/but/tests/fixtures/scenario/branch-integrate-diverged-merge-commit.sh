#!/usr/bin/env bash

source "${BASH_SOURCE[0]%/*}/shared.sh"

git-init-frozen
commit-file M
setup_target_to_match_main

git checkout -b A
commit-file only-on-remote
remote_tracking_caught_up A

# A forge-style "Create a merge commit" merge on the remote of A: the merged side is
# only reachable through the merge commit, and its content is only in the merge's tree.
git checkout -b merged-side
commit-file merged-side
git checkout A
git merge --no-ff -m "Merge pull request #1 from merged-side" merged-side
remote_tracking_caught_up A
git branch -D merged-side

# The local lane diverges: back at the pre-merge commit, then a local commit of its own.
git reset --hard A~1
commit-file only-on-local
create_workspace_commit_once A
