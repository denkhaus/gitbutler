use snapbox::IntoData;
use snapbox::str;

use crate::command::util;
use crate::utils::{CommandExt, Sandbox};

fn pretty_status(env: &Sandbox) -> String {
    serde_json::to_string_pretty(&util::status_json(env)).unwrap()
}

fn raw_json_status(env: &Sandbox) -> String {
    let output = env.but("--json status").allow_json().output().unwrap();
    format!(
        "status={}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn install_editor_script(env: &Sandbox, script: &str) {
    env.file("editor.sh", script);
}

#[test]
fn integrate_pull_rebase_applies_and_snapshots_before_and_after() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-diverged");

    snapbox::assert_data_eq!(
        env.git_log(),
        snapbox::str![[r#"
*   a952a0b (HEAD -> gitbutler/workspace) GitButler Workspace Commit
|\  
| * 643ade3 (A) add only-on-local
|/  
| * 28baf9a (origin/A) add only-on-remote
|/  
* 0dc3733 (origin/main, origin/HEAD, main) add M

"#]]
        .raw()
    );
    snapbox::assert_data_eq!(
        pretty_status(&env),
        snapbox::str![[r#"
{
  "uncommittedChanges": [],
  "stacks": [],
  "mergeBase": {
    "cliId": "",
    "commitId": "0dc37334a458df421bf67ea806103bf5004845dd",
    "createdAt": "2000-01-01T00:00:00+00:00",
    "message": "add M\n",
    "authorName": "author",
    "authorEmail": "author@example.com",
    "conflicted": null,
    "reviewId": null,
    "changes": null
  },
  "upstreamState": {
    "behind": 0,
    "latestCommit": {
      "cliId": "",
      "commitId": "0dc37334a458df421bf67ea806103bf5004845dd",
      "createdAt": "2000-01-01T00:00:00+00:00",
      "message": "add M\n",
      "authorName": "author",
      "authorEmail": "author@example.com",
      "conflicted": null,
      "reviewId": null,
      "changes": null
    },
    "lastFetched": null
  }
}
"#]]
        .raw()
    );

    env.but("branch update A")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Updated branch A.

"#]]);

    snapbox::assert_data_eq!(
        env.git_log(),
        snapbox::str![[r#"
*   6a3496e (HEAD -> gitbutler/workspace) GitButler Workspace Commit
|\  
| * 74faa12 (A) add only-on-local
| * 28baf9a (origin/A) add only-on-remote
|/  
* 0dc3733 (origin/main, origin/HEAD, main, gitbutler/target) add M

"#]]
        .raw()
    );
    snapbox::assert_data_eq!(
        pretty_status(&env),
        snapbox::str![[r#"
{
  "uncommittedChanges": [],
  "stacks": [],
  "mergeBase": {
    "cliId": "",
    "commitId": "0dc37334a458df421bf67ea806103bf5004845dd",
    "createdAt": "2000-01-01T00:00:00+00:00",
    "message": "add M\n",
    "authorName": "author",
    "authorEmail": "author@example.com",
    "conflicted": null,
    "reviewId": null,
    "changes": null
  },
  "upstreamState": {
    "behind": 0,
    "latestCommit": {
      "cliId": "",
      "commitId": "0dc37334a458df421bf67ea806103bf5004845dd",
      "createdAt": "2000-01-01T00:00:00+00:00",
      "message": "add M\n",
      "authorName": "author",
      "authorEmail": "author@example.com",
      "conflicted": null,
      "reviewId": null,
      "changes": null
    },
    "lastFetched": null
  }
}
"#]]
        .raw()
    );
}

#[test]
fn integrate_smart_squash_applies_matching_change_ids() {
    let env =
        Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-smart-squash");

    snapbox::assert_data_eq!(
        env.git_log(),
        snapbox::str![[r#"
* 2662ee8 (HEAD -> gitbutler/workspace, A) add only-on-local
| * c42227a (origin/A) add only-on-remote
|/  
* 0dc3733 (origin/main, origin/HEAD, main) add M

"#]]
    );
    snapbox::assert_data_eq!(
        raw_json_status(&env),
        snapbox::str![[r#"
status=exit status: 1
stdout:

stderr:
Error: GitButler mode exit required: please run `but teardown` to preserve your work.

"#]]
    );

    env.but("branch update A --strategy smart-squash")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Updated branch A.

"#]]);

    snapbox::assert_data_eq!(
        env.git_log(),
        snapbox::str![[r#"
* bf02b24 (HEAD -> gitbutler/workspace, A) add only-on-remote
| * c42227a (origin/A) add only-on-remote
|/  
* 0dc3733 (origin/main, origin/HEAD, main, gitbutler/target) add M

"#]]
    );
    snapbox::assert_data_eq!(
        raw_json_status(&env),
        snapbox::str![[r#"
status=exit status: 1
stdout:

stderr:
Error: GitButler mode exit required: please run `but teardown` to preserve your work.

"#]]
    );
}

#[test]
fn integrate_dry_run_shows_preview_without_changing_repo() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-diverged");
    let before_log = env.git_log();
    let before_status = pretty_status(&env);

    env.but("branch update A --dry-run")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Preview

* A
● sm 74faa12 add only-on-local
● __ 28baf9a add only-on-remote
o 0dc3733

"#]]);

    snapbox::assert_data_eq!(
        &before_log,
        snapbox::str![[r#"
*   a952a0b (HEAD -> gitbutler/workspace) GitButler Workspace Commit
|\  
| * 643ade3 (A) add only-on-local
|/  
| * 28baf9a (origin/A) add only-on-remote
|/  
* 0dc3733 (origin/main, origin/HEAD, main) add M

"#]]
        .raw()
    );
    snapbox::assert_data_eq!(
        &before_status,
        snapbox::str![[r#"
{
  "uncommittedChanges": [],
  "stacks": [],
  "mergeBase": {
    "cliId": "",
    "commitId": "0dc37334a458df421bf67ea806103bf5004845dd",
    "createdAt": "2000-01-01T00:00:00+00:00",
    "message": "add M\n",
    "authorName": "author",
    "authorEmail": "author@example.com",
    "conflicted": null,
    "reviewId": null,
    "changes": null
  },
  "upstreamState": {
    "behind": 0,
    "latestCommit": {
      "cliId": "",
      "commitId": "0dc37334a458df421bf67ea806103bf5004845dd",
      "createdAt": "2000-01-01T00:00:00+00:00",
      "message": "add M\n",
      "authorName": "author",
      "authorEmail": "author@example.com",
      "conflicted": null,
      "reviewId": null,
      "changes": null
    },
    "lastFetched": null
  }
}
"#]]
        .raw()
    );
    assert_eq!(env.git_log(), before_log, "dry-run must not rewrite refs");
    assert_eq!(
        pretty_status(&env),
        before_status,
        "dry-run must not change workspace status"
    );
}

#[test]
fn integrate_dry_run_verbose_shows_divergence_before_preview() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-diverged");
    let before_log = env.git_log();
    let before_status = pretty_status(&env);

    env.but("branch update A --dry-run --verbose")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Current state: A <- origin/A

● __ 643ade3 (A) add only-on-local
┊● __ 28baf9a (origin/A) add only-on-remote
├╯
o 0dc3733 add M

----------------------------

Preview

* A
● sm 74faa12 add only-on-local
● __ 28baf9a add only-on-remote
o 0dc3733

"#]]);

    assert_eq!(
        env.git_log(),
        before_log,
        "verbose dry-run must not rewrite refs"
    );
    assert_eq!(
        pretty_status(&env),
        before_status,
        "verbose dry-run must not change workspace status"
    );
}

#[test]
fn integrate_merge_dry_run_marks_conflicted_preview_commits() {
    let env = Sandbox::init_scenario_with_target_and_default_settings_slow(
        "branch-integrate-conflicting",
    );

    env.but("branch update A -s merge --dry-run")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Preview

* A
● uk d1f9d65 Merge dbf2a866824eab2a4c485b30bcfba70af8502900 into previous commit {conflicted}
● __ 57ca948 local change in A
o 6a997fd

"#]]);
}

#[test]
fn integrate_interactive_unchanged_script_applies_generated_plan() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-diverged");
    install_editor_script(&env, "#!/usr/bin/env bash\n: \"$1\"\n");

    env.but("branch update A --interactive")
        .env("GIT_EDITOR", "bash editor.sh")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Updated branch A.

"#]]);

    snapbox::assert_data_eq!(
        env.git_log(),
        snapbox::str![[r#"
*   6a3496e (HEAD -> gitbutler/workspace) GitButler Workspace Commit
|\  
| * 74faa12 (A) add only-on-local
| * 28baf9a (origin/A) add only-on-remote
|/  
* 0dc3733 (origin/main, origin/HEAD, main, gitbutler/target) add M

"#]]
        .raw()
    );
}

#[test]
fn integrate_interactive_dry_run_keeps_repo_unchanged() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-diverged");
    install_editor_script(&env, "#!/usr/bin/env bash\n: \"$1\"\n");
    let before_log = env.git_log();
    let before_status = pretty_status(&env);

    env.but("branch update A --interactive --dry-run")
        .env("GIT_EDITOR", "bash editor.sh")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Preview

* A
● sm 74faa12 add only-on-local
● __ 28baf9a add only-on-remote
o 0dc3733

"#]]);

    assert_eq!(
        env.git_log(),
        before_log,
        "interactive dry-run must not rewrite refs"
    );
    assert_eq!(
        pretty_status(&env),
        before_status,
        "interactive dry-run must not change workspace status"
    );
}

#[test]
fn integrate_interactive_applies_edited_merge_plan() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-diverged");
    install_editor_script(
        &env,
        r#"#!/usr/bin/env bash
cat > "$1" <<'EOF'
pick 643ade3
merge 28baf9a
EOF
"#,
    );

    env.but("branch update A --interactive")
        .env("GIT_EDITOR", "bash editor.sh")
        .assert()
        .success()
        .stderr_eq(str![]);

    snapbox::assert_data_eq!(
        env.git_log(),
        snapbox::str![[r#"
*   646aabb (HEAD -> gitbutler/workspace) GitButler Workspace Commit
|\  
| *   d1f1cff (A) Merge 28baf9a2794d7722ceff84f2967b5186545b8a48 into previous commit
| |\  
| | * 28baf9a (origin/A) add only-on-remote
| |/  
|/|   
| * 643ade3 add only-on-local
|/  
* 0dc3733 (origin/main, origin/HEAD, main, gitbutler/target) add M

"#]]
        .raw()
    );
}

#[test]
fn integrate_interactive_fails_on_parse_error() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-diverged");
    install_editor_script(
        &env,
        r#"#!/usr/bin/env bash
printf 'drop 643ade3\n' > "$1"
"#,
    );
    let before_log = env.git_log();

    env.but("branch update A --interactive")
        .env("GIT_EDITOR", "bash editor.sh")
        .assert()
        .failure()
        .stdout_eq(str![""])
        .stderr_eq(str![[r#"
Error: line 1: unknown command 'drop'

"#]]);

    assert_eq!(
        env.git_log(),
        before_log,
        "parse failures must not rewrite refs"
    );
}

#[test]
fn integrate_interactive_fails_on_out_of_scope_commit() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-diverged");
    install_editor_script(
        &env,
        r#"#!/usr/bin/env bash
printf 'pick 0dc3733\n' > "$1"
"#,
    );
    let before_log = env.git_log();

    env.but("branch update A --interactive")
        .env("GIT_EDITOR", "bash editor.sh")
        .assert()
        .failure()
        .stdout_eq(str![""])
        .stderr_eq(str![[r#"
Error: line 1: invalid pick commit: commit '0dc3733' is not part of the editable divergence

"#]]);

    assert_eq!(
        env.git_log(),
        before_log,
        "validation failures must not rewrite refs"
    );
}

#[test]
fn integrate_errors_cleanly_without_tracking_branch() {
    let env =
        Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-no-tracking");
    snapbox::assert_data_eq!(
        env.git_log(),
        snapbox::str![[r#"
* edd3eb7 (HEAD -> gitbutler/workspace) GitButler Workspace Commit
* 9477ae7 (A) add A
* 0dc3733 (origin/main, origin/HEAD, main) add M

"#]]
    );
    snapbox::assert_data_eq!(
        pretty_status(&env),
        snapbox::str![[r#"
{
  "uncommittedChanges": [],
  "stacks": [],
  "mergeBase": {
    "cliId": "",
    "commitId": "0dc37334a458df421bf67ea806103bf5004845dd",
    "createdAt": "2000-01-01T00:00:00+00:00",
    "message": "add M\n",
    "authorName": "author",
    "authorEmail": "author@example.com",
    "conflicted": null,
    "reviewId": null,
    "changes": null
  },
  "upstreamState": {
    "behind": 0,
    "latestCommit": {
      "cliId": "",
      "commitId": "0dc37334a458df421bf67ea806103bf5004845dd",
      "createdAt": "2000-01-01T00:00:00+00:00",
      "message": "add M\n",
      "authorName": "author",
      "authorEmail": "author@example.com",
      "conflicted": null,
      "reviewId": null,
      "changes": null
    },
    "lastFetched": null
  }
}
"#]]
        .raw()
    );

    env.but("branch update A")
        .assert()
        .failure()
        .stdout_eq(str![""])
        .stderr_eq(str![[r#"
Error: Branch 'refs/heads/A' has no tracking branch

"#]]);
}

#[test]
fn integrate_level_branch_is_a_noop_with_a_clear_message() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-level");
    let before_log = env.git_log();

    env.but("branch update A")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
A is already up to date with origin/A.

"#]]);

    assert_eq!(
        env.git_log(),
        before_log,
        "an integration with nothing to do must not rewrite refs"
    );

    env.but("branch update A")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
A is already up to date with origin/A.

"#]]);
}

#[test]
fn integrate_takes_a_remote_merge_commit_verbatim() {
    let env =
        Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-merge-commit");

    let remote_tip = env.invoke_git("rev-parse refs/remotes/origin/A");
    assert_ne!(
        env.invoke_git("rev-parse refs/heads/A"),
        remote_tip,
        "the lane starts behind the remote's merge commit"
    );
    assert_eq!(
        env.invoke_git("rev-list --count refs/heads/A..refs/remotes/origin/A"),
        "2",
        "the divergence is the merge commit and its merged side"
    );
    assert_eq!(
        env.invoke_git("rev-list --parents -n 1 refs/remotes/origin/A")
            .split_whitespace()
            .count(),
        3,
        "the remote tip is a merge commit"
    );

    env.but("branch update A")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Updated branch A.

"#]]);

    assert_eq!(
        env.invoke_git("rev-parse refs/heads/A"),
        remote_tip,
        "the lane takes the remote's merge commit itself, merged side included"
    );
    assert_eq!(
        env.invoke_git("rev-list --count HEAD..refs/remotes/origin/A"),
        "0",
        "the workspace contains the remote tip"
    );
    assert!(
        env.read_file("merged-side").is_ok(),
        "the merged side is materialized in the worktree"
    );

    env.but("branch update A")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
A is already up to date with origin/A.

"#]]);
}

#[test]
fn integrate_refreshes_a_workspace_left_behind_by_a_plain_git_fast_forward() {
    let env =
        Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-stale-worktree");
    env.setup_metadata(&["A"]);

    assert_eq!(
        env.invoke_git("rev-parse refs/heads/A"),
        env.invoke_git("rev-parse refs/remotes/origin/A"),
        "the branch is level with its remote"
    );
    assert!(
        !env.projects_root().join("only-on-plain-git").exists(),
        "the file is in the branch ref, but the worktree lags behind it"
    );

    env.but("branch update A")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
A is already up to date with origin/A.

"#]]);

    assert!(
        env.projects_root().join("only-on-plain-git").exists(),
        "the workspace was refreshed to the branch tip, so the file is materialized"
    );
}

#[test]
fn integrate_takes_a_remote_merge_commit_verbatim_when_diverged() {
    let env = Sandbox::init_scenario_with_target_and_default_settings(
        "branch-integrate-diverged-merge-commit",
    );

    assert_eq!(
        env.invoke_git("rev-list --count refs/heads/A..refs/remotes/origin/A"),
        "2",
        "the lane is behind the remote's merge commit and its merged side"
    );
    assert_eq!(
        env.invoke_git("rev-list --count refs/remotes/origin/A..refs/heads/A"),
        "1",
        "the lane carries one local commit the remote does not have"
    );
    assert_eq!(
        env.invoke_git("rev-list --parents -n 1 refs/remotes/origin/A")
            .split_whitespace()
            .count(),
        3,
        "the remote tip is a merge commit"
    );

    env.but("branch update A")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Updated branch A.

"#]]);

    assert_eq!(
        env.invoke_git("rev-list --count refs/heads/A..refs/remotes/origin/A"),
        "0",
        "the remote's merge commit is part of the lane, so the lane is not behind anymore"
    );
    assert!(
        env.read_file("merged-side").is_ok(),
        "the merged side is materialized in the worktree"
    );
    assert!(
        env.read_file("only-on-local").is_ok(),
        "the local commit stays materialized in the worktree"
    );

    // A second run is a no-op: like any lane that is only ahead of its remote, the update
    // rebuilds the local commit in place, stacks nothing, and stays level with the remote.
    let lane_tip = env.invoke_git("rev-parse refs/heads/A");
    env.but("branch update A")
        .assert()
        .success()
        .stderr_eq(str![])
        .stdout_eq(str![[r#"
Updated branch A.

"#]]);
    assert_eq!(
        env.invoke_git("rev-parse refs/heads/A"),
        lane_tip,
        "the second run stacks no further commits"
    );
    assert_eq!(
        env.invoke_git("rev-list --count refs/heads/A..refs/remotes/origin/A"),
        "0",
        "the lane stays level with the remote"
    );
}

#[test]
fn integrate_level_branch_reports_the_unchanged_workspace_as_json() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("branch-integrate-level");
    let before_status = pretty_status(&env);

    let output = env
        .but("--json branch update A")
        .allow_json()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("machine output is JSON");
    assert!(
        value.get("workspace").is_some(),
        "an integration with nothing to do reports the workspace it left alone: {value}"
    );

    assert_eq!(
        pretty_status(&env),
        before_status,
        "an integration with nothing to do must not change the workspace"
    );
}
