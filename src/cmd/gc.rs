use std::process::ExitCode;

use anyhow::{Result, anyhow};
use serde_json::json;

use super::{Ctx, REFUSED};
use crate::gc::{self, Site, Swept};
use crate::model::EpochMs;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 지우지 않고 지울 것만 보인다
    #[arg(long)]
    pub dry_run: bool,
    /// 결과를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let swept = sweep(&args, ctx)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&to_json(&swept, &args))?);
    } else {
        print!("{}", render(&swept, args.dry_run));
    }
    for failure in &swept.failures {
        eprintln!("chute: 정리하지 못했다: {failure}");
    }
    Ok(if swept.failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(REFUSED)
    })
}

fn sweep(args: &Args, ctx: &Ctx) -> Result<Swept> {
    let (info, store) = ctx.registered_repo()?;
    let site = Site {
        ext: &ctx.ext,
        home: &ctx.home,
        info: &info,
        store: &store,
    };
    gc::run(&site, args.dry_run, EpochMs::now())?.ok_or_else(|| {
        anyhow!(
            "{}/{}에서 chute.toml을 읽지 못했다. 보관 기간을 알 수 없어 정리하지 않는다. 기본 브랜치에 올렸는지 확인한다",
            info.remote_name,
            info.default_branch
        )
    })
}

fn to_json(swept: &Swept, args: &Args) -> serde_json::Value {
    json!({
        "dry_run": args.dry_run,
        "items": swept.items,
        "bytes": swept.bytes(),
        "failures": swept.failures,
    })
}

fn render(swept: &Swept, dry_run: bool) -> String {
    let verb = if dry_run { "지울 것" } else { "지웠다" };
    let mut out = String::new();
    for item in &swept.items {
        out.push_str(&format!(
            "{verb}: {} {} ({})\n",
            item.kind.label(),
            item.target,
            size(item.bytes)
        ));
    }
    if swept.items.is_empty() {
        out.push_str("지울 것이 없다\n");
    } else {
        out.push_str(&format!(
            "합계: {}건 {}{}\n",
            swept.items.len(),
            size(swept.bytes()),
            if dry_run {
                " (지우지 않았다)"
            } else {
                ""
            }
        ));
    }
    out
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::ext::Ext;
    use crate::model::{MachineId, RepoInfo};
    use crate::store::Home;
    use crate::testkit::{Sandbox, TestRepo};

    struct World {
        sb: Sandbox,
        repo: TestRepo,
        ctx: Ctx,
        info: RepoInfo,
    }

    impl World {
        fn new() -> World {
            let sb = Sandbox::new();
            let repo = TestRepo::new(&sb, "acme/widget");
            repo.commit_file(
                "chute.toml",
                "[[verify]]\nname = \"t\"\nrun = \"true\"\n",
                "chore: 설정",
            );
            repo.git(&["push", "--quiet", "origin", "main"]);
            let ext = Ext::from_vars(|k| sb.var(k));
            let home = Home::at(sb.chute_home());
            let info = crate::repo::discover(&ext, repo.path(), &[]).unwrap();
            home.repo(&info.slug).register(&info).unwrap();
            let ctx = Ctx::new(home, ext, Some(repo.path().to_path_buf()));
            World {
                sb,
                repo,
                ctx,
                info,
            }
        }

        fn old_log(&self) -> PathBuf {
            let dir = self
                .ctx
                .home
                .repo(&self.info.slug)
                .dir()
                .join("preflight/old-1");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("a.log"), "x".repeat(2048)).unwrap();
            let long_ago = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
            fs::File::open(&dir)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
            dir
        }

        fn args(&self, dry_run: bool) -> Args {
            Args {
                dry_run,
                json: false,
            }
        }
    }

    #[test]
    fn gc_dry_run_lists_what_a_real_run_then_removes_and_removes_nothing_itself() {
        let w = World::new();
        let log = w.old_log();

        let planned = sweep(&w.args(true), &w.ctx).unwrap();
        assert!(log.exists());
        let text = render(&planned, true);
        assert!(text.contains("지울 것: preflight 로그"), "{text}");
        assert!(text.contains("합계: 1건 2.0 KiB (지우지 않았다)"), "{text}");

        let done = sweep(&w.args(false), &w.ctx).unwrap();
        assert!(!log.exists());
        assert_eq!(done.items, planned.items);
        assert!(render(&done, false).contains("지웠다: preflight 로그"));
        assert_eq!(
            render(&sweep(&w.args(false), &w.ctx).unwrap(), false),
            "지울 것이 없다\n"
        );
    }

    #[test]
    fn gc_json_names_the_kind_the_target_and_the_size() {
        let w = World::new();
        let log = w.old_log();

        let swept = sweep(&w.args(true), &w.ctx).unwrap();
        let value = to_json(&swept, &w.args(true));

        assert_eq!(value["dry_run"], true);
        assert_eq!(value["bytes"], 2048);
        assert_eq!(value["items"][0]["kind"], "preflight_log");
        assert_eq!(value["items"][0]["target"], log.display().to_string());
        assert_eq!(value["items"][0]["bytes"], 2048);
        assert_eq!(value["failures"], serde_json::json!([]));
    }

    #[test]
    fn gc_on_a_pc_that_is_not_the_hub_cleans_this_pc_and_forwards_nothing() {
        let w = World::new();
        let log = w.old_log();
        let other = MachineId::try_from("0000000000000000000000000000000b".to_owned()).unwrap();
        w.ctx.home.repo(&w.info.slug).set_hub(other).unwrap();

        sweep(&w.args(false), &w.ctx).unwrap();

        assert!(!log.exists());
        assert!(
            w.sb.fake("hc")
                .calls()
                .iter()
                .all(|call| !call.iter().any(|a| a == "chute")),
            "nothing is sent to another machine"
        );
    }

    #[test]
    fn gc_without_a_config_on_origin_says_so() {
        let w = World::new();
        w.repo.git(&[
            "push",
            "--quiet",
            "origin",
            "HEAD~1:refs/heads/main",
            "--force",
        ]);
        w.repo.git(&["fetch", "--quiet", "origin"]);

        let e = sweep(&w.args(true), &w.ctx).unwrap_err();

        assert!(format!("{e}").contains("chute.toml을 읽지 못했다"), "{e}");
    }
}
