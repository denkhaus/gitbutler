use crate::args::atoms::CliIdArg;

#[derive(Debug, clap::Parser)]
pub struct Platform {
    /// Branch name or CLI ID whose pushable commit to print.
    ///
    /// This is the commit `but push` sends for the branch, i.e. the tip that exists on the remote
    /// after a push. It is not the local workspace commit, which is never pushed.
    #[clap(value_name = "BRANCH")]
    pub branch_id: CliIdArg,
}
