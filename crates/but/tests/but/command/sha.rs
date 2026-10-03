use crate::utils::Sandbox;

/// Tooling tags a commit that must exist on the remote, and the local workspace commit never does.
/// What `but sha` prints has to be what a push publishes, so validate it against the real remote.
#[test]
fn sha_prints_the_commit_a_push_publishes() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("one-stack");
    env.setup_metadata(&["A"]);

    let remote_git = env.app_data_dir().join("origin.git");
    let remote_git = remote_git.display();
    env.invoke_bash(format!(
        "rm -rf {remote_git} && git clone --bare . {remote_git} && (git remote get-url origin >/dev/null 2>&1 && git remote set-url origin {remote_git} || git remote add origin {remote_git})",
    ));

    assert_ne!(
        env.invoke_git("rev-parse A"),
        env.invoke_git("rev-parse gitbutler/workspace"),
        "the fixture must have a workspace commit that differs from the branch commit"
    );

    // The fixture pins its commits, so the pushable tip is this fixed SHA. It has to arrive bare,
    // full and unshortened on a line of its own, because release tooling consumes it as
    // `$(but sha A)`.
    env.but("sha A")
        .assert()
        .success()
        .stdout_eq(snapbox::str![[r#"
9477ae721ab521d9d0174f70e804ce3ff9f6fb56

"#]]);

    env.but("push A").assert().success();

    assert_eq!(
        env.invoke_git("rev-parse origin/A"),
        "9477ae721ab521d9d0174f70e804ce3ff9f6fb56",
        "the SHA reported before the push must be the one that lands on the remote"
    );
}

/// A branch that is not part of the workspace has no pushable tip; saying so beats printing a SHA
/// that no remote can resolve.
#[test]
fn sha_rejects_a_branch_outside_the_workspace() {
    let env = Sandbox::init_scenario_with_target_and_default_settings("one-stack");
    env.setup_metadata(&["A"]);

    env.but("sha not-a-branch")
        .assert()
        .failure()
        .stdout_eq(snapbox::str![])
        .stderr_eq(snapbox::str![[r#"
Error: Branch 'not-a-branch' not found. Available branches:
  - A

"#]]);
}
