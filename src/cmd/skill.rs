use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};

pub fn run(_ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("--skill")
}
