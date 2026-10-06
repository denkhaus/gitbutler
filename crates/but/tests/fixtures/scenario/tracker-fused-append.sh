#!/usr/bin/env bash

source "${BASH_SOURCE[0]%/*}/shared.sh"

# A JSONL tracker on the target with two rows, and a lane with a commit of its own.
git-init-frozen
printf 'seq 19\nseq 20\n' >log.jsonl
git add log.jsonl
git commit -m "add log base"
setup_target_to_match_main

git checkout -b A
commit-file A
create_workspace_commit_once A
