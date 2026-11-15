use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};
use crate::model::PrNumber;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호. 없으면 지금 브랜치의 PR
    pub pr: Option<PrNumber>,
    /// 반송을 받을 hc 터미널 주소. 없으면 HC_TERMINAL_ID
    #[arg(long, value_name = "주소")]
    pub agent: Option<String>,
    /// 결과를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
}

pub fn run(_args: Args, _ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("submit")
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    #[test]
    fn not_implemented_yet() {
        assert_eq!(
            super::super::run_args(&["submit", "#12", "--agent", "t1rs", "--json"]),
            ExitCode::from(2)
        );
    }
}
