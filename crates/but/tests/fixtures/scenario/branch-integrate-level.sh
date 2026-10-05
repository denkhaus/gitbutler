#!/usr/bin/env bash

source "${BASH_SOURCE[0]%/*}/shared.sh"

git-init-frozen
commit-file M
setup_target_to_match_main

git checkout -b A
commit-file A
remote_tracking_caught_up A
create_workspace_commit_once A
