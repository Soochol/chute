use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 되돌릴 batch ID
    pub batch: String,
    /// 검증을 건너뛰고 push한다
    #[arg(long)]
    pub no_verify: bool,
}

pub fn run(_args: Args, _ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("revert")
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    #[test]
    fn not_implemented_yet() {
        assert_eq!(
            super::super::run_args(&["revert", "b20261115-150000", "--no-verify"]),
            ExitCode::from(2)
        );
    }
}
