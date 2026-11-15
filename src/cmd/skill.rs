use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};

pub fn run(_ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("--skill")
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    #[test]
    fn not_implemented_yet() {
        assert_eq!(super::super::run_args(&["--skill"]), ExitCode::from(2));
    }
}
