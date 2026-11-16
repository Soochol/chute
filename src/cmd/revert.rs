use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};

use super::batches::{self, Loaded, short};
use super::{CANNOT_RUN, Ctx, FORWARD_TIMEOUT, Refusal, finish, forward_to_hub};
use crate::config::Config;
use crate::engine::VERIFIED_BASES_KEPT;
use crate::gitops::{Gitops, PushOutcome, Slot, SlotName};
use crate::hub::{Route, route};
use crate::model::{
    Agent, BatchId, EpochMs, Event, EventKind, MergeRef, PrNumber, RepoInfo, RevertRecord,
    RevertedPr, Sha,
};
use crate::notify::{Line, Notifier};
use crate::store::RepoStore;
use crate::verify::{self, Options, Outcome};

const PUSH_TRIES: u32 = 3;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 되돌릴 batch ID
    pub batch: String,
    /// 검증을 건너뛰고 push한다
    #[arg(long)]
    pub no_verify: bool,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    if let Route::Remote { name, .. } = route(&ctx.home, &ctx.ext, &ctx.slug()?)? {
        let command = hub_command(ctx, &name, &args);
        eprintln!(
            "chute: revert는 허브에서만 실행한다. 이 저장소의 허브는 `{name}`이다. \
             검증까지 하면 시간이 오래 걸려 넘기지 않는다. 허브에서 실행하는 명령:\n  {command}"
        );
        return Ok(ExitCode::from(CANNOT_RUN));
    }
    finish(revert(&args, ctx))
}

fn hub_command(ctx: &Ctx, hub: &str, args: &Args) -> String {
    let path = hub_repo_path(ctx)
        .map(|path| shell_word(&path))
        .unwrap_or_else(|| "<허브의 저장소 경로>".to_owned());
    let mut command = format!(
        "hc run --on {hub} --cwd {path} chute revert {}",
        shell_word(&args.batch)
    );
    if args.no_verify {
        command.push_str(" --no-verify");
    }
    command
}

fn hub_repo_path(ctx: &Ctx) -> Option<String> {
    let answer = forward_to_hub(ctx, &["status", "--json"], FORWARD_TIMEOUT)
        .ok()
        .flatten()?;
    if answer.code != 0 {
        return None;
    }
    let status: serde_json::Value = serde_json::from_slice(&answer.stdout).ok()?;
    Some(status["repo"]["path"].as_str()?.to_owned())
}

fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:@+=,".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn refuse<T>(message: impl Into<String>) -> Result<T> {
    Err(Refusal(message.into()).into())
}

fn revert(args: &Args, ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    let id = BatchId(args.batch.clone());
    let gitops = Gitops::new(&ctx.ext, &ctx.home, &info);
    let Some(lock) = gitops.lock_slot(SlotName::Revert)? else {
        return refuse("다른 revert가 돌고 있다. 끝난 뒤 다시 실행한다");
    };
    let slot = lock.open()?;
    let all = batches::load_all(&store)?;
    let batch = all.iter().find(|b| b.record.id == id).ok_or_else(|| {
        Refusal(format!(
            "그런 batch가 없다: {id}. `chute batches`로 찾는다",
            id = id.0
        ))
    })?;
    let targets = revertable(batch)?;

    for _ in 0..PUSH_TRIES {
        let base = slot.fetch_base()?;
        slot.checkout(&base)?;
        let done = revert_all(ctx, &slot, &base, &targets, &all, batch)?;
        if done.made.is_empty() {
            let record = locate_reverts(ctx, &slot, &base, &targets)?;
            println!(
                "batch {}의 변경은 이미 {}에 없다. 아무것도 올리지 않았다.",
                id.0, info.default_branch
            );
            if record.prs.len() == targets.len() {
                record_push(&store, &id, &record)?;
                print!("{}", report(&info, &id, &record, &[], false));
            } else {
                println!(
                    "되돌린 커밋을 {}의 이력에서 모두 찾지는 못해 기록하지 않았다.",
                    info.default_branch
                );
            }
            return Ok(());
        }
        let tip = slot.head()?;
        if !args.no_verify {
            verify_tree(ctx, &info, &store, &slot, &id)?;
        }
        match slot.push(&tip)? {
            PushOutcome::MainMoved => continue,
            PushOutcome::Pushed => {
                let record = RevertRecord {
                    at_ms: EpochMs::now(),
                    tip,
                    prs: done.made,
                    verified: !args.no_verify,
                };
                record_push(&store, &id, &record).with_context(|| {
                    format!(
                        "되돌리기는 main에 올랐지만({}) 기록을 남기지 못했다",
                        record.tip
                    )
                })?;
                print!("{}", report(&info, &id, &record, &done.skipped, true));
                tell_submitters(ctx, &info, &store, &id, &record, &targets);
                return Ok(());
            }
        }
    }
    refuse(format!(
        "main이 계속 움직여 {PUSH_TRIES}번 시도하고도 올리지 못했다. 아무것도 올리지 않았다. 잠시 뒤 다시 실행한다"
    ))
}

fn tell_submitters(
    ctx: &Ctx,
    info: &RepoInfo,
    store: &RepoStore,
    id: &BatchId,
    record: &RevertRecord,
    targets: &[MergeRef],
) {
    let submitter_of = |pr: PrNumber| -> Option<&Agent> {
        targets
            .iter()
            .find(|t| t.pr == pr)
            .and_then(|t| t.submitter.as_ref())
    };
    let reverted: Vec<(Option<&Agent>, PrNumber)> = record
        .prs
        .iter()
        .map(|p| (submitter_of(p.pr), p.pr))
        .collect();
    Notifier::new(&ctx.ext, info, store).notify_each(&reverted, record.at_ms, |prs| {
        let mut prs = prs.to_vec();
        prs.sort();
        Line::Reverted {
            batch: id.clone(),
            prs,
            commit: record.tip.clone(),
        }
    });
}

fn revertable(batch: &Loaded) -> Result<Vec<MergeRef>> {
    let id = &batch.record.id.0;
    if batch.is_reverted() {
        return refuse(format!("이미 되돌린 batch다: {id}"));
    }
    if batch.record.finished_ms.is_none() {
        return refuse(format!(
            "batch {id}의 기록이 끝나지 않았다(데몬이 도중에 멈췄을 수 있다). 추측해서 되돌리지 않는다"
        ));
    }
    let merges = &batch.record.merges;
    if batch.merged().is_empty() && merges.is_empty() {
        return refuse(format!("batch {id}는 머지한 PR이 없어 되돌릴 것이 없다"));
    }
    if batch.finished.is_none() {
        return refuse(format!(
            "batch {id}의 마무리 기록이 history.jsonl에 없다. 기록이 불완전해 되돌리지 않는다"
        ));
    }
    let recorded = batches::pr_set(&batch.merged());
    let made = batches::pr_set(&merges.iter().map(|m| m.pr).collect::<Vec<PrNumber>>());
    let missing: Vec<String> = recorded
        .difference(&made)
        .map(|p| format!("#{}", p.0))
        .collect();
    let extra: Vec<String> = made
        .difference(&recorded)
        .map(|p| format!("#{}", p.0))
        .collect();
    if !missing.is_empty() {
        return refuse(format!(
            "batch {id}에서 머지된 {}의 merge commit이 batch 기록에 없다(chute가 만든 merge commit이 아닐 수 있다). 일부만 되돌리지 않는다",
            missing.join(" ")
        ));
    }
    if !extra.is_empty() {
        return refuse(format!(
            "batch {id}의 기록과 history.jsonl이 어긋난다({}). 되돌리지 않는다",
            extra.join(" ")
        ));
    }
    Ok(merges.clone())
}

fn revert_all(
    ctx: &Ctx,
    slot: &Slot,
    base: &Sha,
    targets: &[MergeRef],
    all: &[Loaded],
    batch: &Loaded,
) -> Result<Done> {
    let order = first_parent_order(ctx, slot, base)?;
    let mut ranked = Vec::with_capacity(targets.len());
    for target in targets {
        let Some(at) = order.iter().position(|c| *c == target.commit) else {
            return refuse(format!(
                "#{}의 merge commit {}이 main의 이력에 없다. 되돌리지 않는다",
                target.pr.0, target.commit
            ));
        };
        ranked.push((at, target));
    }
    ranked.sort_by_key(|(at, _)| *at);

    let mut done = Done::default();
    for (_, target) in ranked {
        let out = ctx.ext.git(
            slot.path(),
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "rerere.enabled=false",
                "revert",
                "--no-commit",
                "-m",
                "1",
                target.commit.as_str(),
            ],
        )?;
        if !out.success() {
            let conflicted = ctx
                .ext
                .git(slot.path(), ["diff", "--name-only", "--diff-filter=U"])?
                .ok()?;
            let _ = ctx.ext.git(slot.path(), ["revert", "--abort"]);
            slot.checkout(base)?;
            let detail = format!("{}{}", out.stdout_text(), out.stderr_text());
            return if conflicted.trim().is_empty() {
                refuse(format!(
                    "#{}을 git이 되돌리지 못했다. 아무것도 올리지 않았다.\ngit의 말: {}",
                    target.pr.0,
                    detail.trim()
                ))
            } else {
                refuse(conflict_message(target.pr, &conflicted, all, batch))
            };
        }
        let nothing_staged = ctx
            .ext
            .git(slot.path(), ["diff", "--cached", "--quiet"])?
            .success();
        if nothing_staged {
            // Nothing of this merge is left in main, so there is nothing to undo.
            let _ = ctx.ext.git(slot.path(), ["revert", "--abort"]);
            slot.checkout(&slot.head()?)?;
            done.skipped.push(target.pr);
            continue;
        }
        ctx.ext
            .git(
                slot.path(),
                [
                    "-c",
                    "core.hooksPath=/dev/null",
                    "commit",
                    "--quiet",
                    "--no-edit",
                ],
            )?
            .ok()?;
        done.made.push(RevertedPr {
            pr: target.pr,
            merge: target.commit.clone(),
            revert: slot.head()?,
        });
    }
    Ok(done)
}

#[derive(Default)]
struct Done {
    made: Vec<RevertedPr>,
    /// PRs whose merge no longer changes main.
    skipped: Vec<PrNumber>,
}

/// Finds the revert commits already on main, for the record only. The commit message names
/// the merge it undoes. Whether main still holds the batch was decided by `revert_all`, so a
/// miss here costs a detail in the record, never a wrong push.
fn locate_reverts(
    ctx: &Ctx,
    slot: &Slot,
    base: &Sha,
    targets: &[MergeRef],
) -> Result<RevertRecord> {
    let order = first_parent_order(ctx, slot, base)?;
    let mut found = Vec::new();
    for target in targets {
        let marker = format!("This reverts commit {}", target.commit);
        let out = ctx
            .ext
            .git(
                slot.path(),
                [
                    "log",
                    "--first-parent",
                    "--fixed-strings",
                    "--format=%H",
                    "-n1",
                    "--grep",
                    marker.as_str(),
                    base.as_str(),
                ],
            )?
            .ok()?;
        if let Ok(revert) = Sha::try_from(out.trim().to_owned()) {
            found.push(RevertedPr {
                pr: target.pr,
                merge: target.commit.clone(),
                revert,
            });
        }
    }
    let newest = found
        .iter()
        .filter_map(|p| order.iter().position(|c| *c == p.revert))
        .min()
        .map_or_else(|| base.clone(), |at| order[at].clone());
    Ok(RevertRecord {
        at_ms: EpochMs::now(),
        tip: newest,
        prs: found,
        verified: false,
    })
}

fn first_parent_order(ctx: &Ctx, slot: &Slot, base: &Sha) -> Result<Vec<Sha>> {
    let text = ctx
        .ext
        .git(slot.path(), ["rev-list", "--first-parent", base.as_str()])?
        .ok()?;
    text.lines()
        .map(|l| Sha::try_from(l.trim().to_owned()).map_err(|e| anyhow!(e)))
        .collect()
}

fn conflict_message(pr: PrNumber, files: &str, all: &[Loaded], batch: &Loaded) -> String {
    let at = |b: &Loaded| b.record.finished_ms.unwrap_or(b.record.started_ms);
    let mut text = format!(
        "#{}을 되돌리다 뒤에 머지된 변경과 충돌했다. 아무것도 올리지 않았다.\n충돌한 파일:\n",
        pr.0
    );
    for file in files.lines() {
        text.push_str(&format!("  {file}\n"));
    }
    let later: Vec<&Loaded> = all
        .iter()
        .filter(|b| b.record.id != batch.record.id && at(b) > at(batch) && !b.merged().is_empty())
        .collect();
    if later.is_empty() {
        text.push_str(
            "이 batch 뒤에 머지된 batch는 없다. main에 직접 들어온 변경과 겹쳤을 수 있다.",
        );
    } else {
        text.push_str("이 batch 뒤에 머지된 batch(먼저 되돌려야 할 수 있다):\n");
        for b in later.iter().rev() {
            let merged: Vec<String> = b.merged().iter().map(|p| format!("#{}", p.0)).collect();
            text.push_str(&format!("  {}  {}\n", b.record.id.0, merged.join(" ")));
        }
    }
    text
}

fn verify_tree(
    ctx: &Ctx,
    info: &RepoInfo,
    store: &RepoStore,
    slot: &Slot,
    id: &BatchId,
) -> Result<()> {
    let branch = &info.default_branch;
    let config = Config::load_remote_branch(&ctx.ext, &info.path, branch)?.ok_or_else(|| {
        anyhow!("chute.toml을 origin/{branch}에서 읽지 못했다. 기본 브랜치에 올렸는지 확인한다")
    })?;
    let log_dir = store.batch_dir(id).join("revert-logs");
    match verify::run(&config, store, slot.path(), &log_dir, &Options::default())? {
        Outcome::Passed { .. } => Ok(()),
        Outcome::Failed(failed) => refuse(format!(
            "되돌린 트리가 검증의 {} 단계에서 실패했다. push하지 않았다.\n로그: {}",
            failed.step,
            failed.log.display()
        )),
        Outcome::Cancelled => bail!("검증이 취소됐다"),
    }
}

fn record_push(store: &RepoStore, id: &BatchId, record: &RevertRecord) -> Result<()> {
    store.write_revert(id, record)?;
    store.append_history(&Event {
        at_ms: record.at_ms,
        kind: EventKind::Reverted {
            batch: id.clone(),
            prs: record.prs.iter().map(|p| p.pr).collect(),
            commit: record.tip.clone(),
        },
    })?;
    if record.verified {
        store.update(|s| {
            s.verified_bases.push(record.tip.clone());
            let excess = s.verified_bases.len().saturating_sub(VERIFIED_BASES_KEPT);
            s.verified_bases.drain(..excess);
            Ok(())
        })?;
    }
    Ok(())
}

fn report(
    info: &RepoInfo,
    id: &BatchId,
    record: &RevertRecord,
    skipped: &[PrNumber],
    pushed: bool,
) -> String {
    let branch = &info.default_branch;
    let mut out = String::new();
    if pushed {
        out.push_str(&format!(
            "batch {}를 되돌려 {branch}에 올렸다 ({})\n",
            id.0,
            short(&record.tip)
        ));
        if !record.verified {
            out.push_str("--no-verify라서 검증하지 않았다.\n");
        }
    }
    out.push_str("\n되돌린 PR:\n");
    for p in &record.prs {
        out.push_str(&format!("  #{}  revert 커밋 {}\n", p.pr.0, p.revert));
    }
    out.push_str(&format!(
        "\n다시 올리는 방법: 되돌린 PR의 커밋은 {branch}의 이력에 남아 있어서 같은 브랜치를 다시 머지해도 변경이 돌아오지 않는다.\n\
         PR 브랜치에서 {branch}를 합친 뒤 revert 커밋을 되돌리고, 고쳐서 다시 submit한다.\n"
    ));
    if !skipped.is_empty() {
        let names: Vec<String> = skipped.iter().map(|p| format!("#{}", p.0)).collect();
        out.push_str(&format!(
            "\n{}은 {branch}에 남은 변경이 없어 건너뛰었다.\n",
            names.join(" ")
        ));
    }
    for p in &record.prs {
        out.push_str(&format!(
            "  #{}: git merge origin/{branch} && git revert {}\n",
            p.pr.0, p.revert
        ));
    }
    out
}
