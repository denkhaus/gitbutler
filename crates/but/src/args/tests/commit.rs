use clap::Parser as _;

use crate::args::{Args, Subcommands, atoms::CliIdArg, commit::Platform};

fn commit_args(args: &[&str]) -> Platform {
    let args = Args::try_parse_from(std::iter::once("but").chain(args.iter().copied())).unwrap();
    match args.cmd.unwrap() {
        Subcommands::Commit(platform) => platform,
        cmd => panic!("expected commit command, got {cmd:?}"),
    }
}

fn names(ids: &[CliIdArg]) -> Vec<&str> {
    ids.iter().map(|id| id.0.as_str()).collect()
}

#[test]
fn file_is_repeatable_and_kept_apart_from_the_positional_selection() {
    let platform = commit_args(&["commit", "ab", "--file", "a/b.rs", "--file", "c/"]);

    assert_eq!(names(&platform.changes), ["ab"]);
    assert_eq!(names(&platform.file), ["a/b.rs", "c/"]);
    assert!(!platform.hunks);
}

#[test]
fn hunks_lists_with_a_selection() {
    let platform = commit_args(&["commit", "--hunks", "--file", "a/b.rs", "ab"]);

    assert!(platform.hunks);
    assert_eq!(names(&platform.changes), ["ab"]);
    assert_eq!(names(&platform.file), ["a/b.rs"]);
}

#[test]
fn hunks_rejects_the_selection_flags_it_replaces() {
    assert!(
        Args::try_parse_from(["but", "commit", "--hunks", "--empty"]).is_err(),
        "--hunks lists what --empty would commit, so both cannot be given"
    );
    assert!(
        Args::try_parse_from(["but", "commit", "--hunks", "--interactive"]).is_err(),
        "--hunks is the non-interactive alternative to --interactive"
    );
}
