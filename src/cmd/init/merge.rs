use std::fs;
use std::io::{BufRead, Write};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use super::setup::{COMMIT_MESSAGE, Ready};
use crate::cmd::{Ctx, Refusal};
use crate::config;
use crate::gitops::{Gitops, ItemResult, MergeItem, PushOutcome, Slot};
use crate::model::{PrNumber, RepoInfo, Sha};

pub(super) const BRANCH: &str = "chute/setup";

pub(super) struct Console<'a> {
    pub tty: bool,
    pub input: &'a mut dyn BufRead,
}

impl Console<'_> {
    pub(super) fn confirm(&mut self, out: &mut dyn Write) -> Result<bool> {
        write!(out, "계속한다 [y/N] ")?;
        out.flush()?;
        let mut answer = String::new();
        self.input.read_line(&mut answer)?;
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenPr {
    pub number: PrNumber,
    pub head: Sha,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Landed {
    Merged,
    AlreadyConfigured,
    /// The default branch moved. `head` is the setup branch head this run left behind.
    Moved {
        head: Sha,
    },
}

/// What `land` may do with a `chute/setup` that is on the remote while no PR is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Overwrite {
    /// Stop: nobody confirmed overwriting it.
    Refuse,
    /// The terminal user saw its head and answered yes.
    Confirmed,
}

#[derive(Deserialize)]
struct ListedPr {
    number: u64,
    #[serde(rename = "headRefOid")]
    head: String,
    #[serde(rename = "isCrossRepository")]
    cross_repository: bool,
}

#[derive(Deserialize)]
struct ViewedPr {
    state: String,
    #[serde(rename = "baseRefName")]
    base: String,
    #[serde(rename = "headRefName")]
    head_name: String,
    #[serde(rename = "isCrossRepository")]
    cross_repository: bool,
}

fn refuse(message: String) -> anyhow::Error {
    Refusal(message).into()
}

fn sha(text: String) -> Result<Sha> {
    Sha::try_from(text).map_err(|e| anyhow!(e))
}

pub(super) fn find_open_pr(ctx: &Ctx, info: &RepoInfo) -> Result<Option<OpenPr>> {
    let out = ctx.ext.gh(
        &info.path,
        [
            "pr",
            "list",
            "--head",
            BRANCH,
            "--base",
            &info.default_branch,
            "--state",
            "open",
            "--json",
            "number,headRefOid,isCrossRepository",
        ],
    )?;
    let text = out.ok().context("열린 setup PR을 gh로 찾지 못했다")?;
    let listed: Vec<ListedPr> =
        serde_json::from_str(&text).context("gh pr list의 출력을 읽지 못했다")?;
    let mut mine: Vec<ListedPr> = listed
        .into_iter()
        .filter(|pr| !pr.cross_repository)
        .collect();
    match mine.len() {
        0 => Ok(None),
        1 => {
            let pr = mine.remove(0);
            Ok(Some(OpenPr {
                number: PrNumber(pr.number),
                head: sha(pr.head)?,
            }))
        }
        n => Err(refuse(format!(
            "{BRANCH}에서 {}으로 가는 열린 PR이 {n}개다. 하나만 남기고 닫은 뒤 다시 실행한다",
            info.default_branch
        ))),
    }
}

/// The head of `chute/setup` on the remote, read without fetching. `None` when there is none.
pub(super) fn remote_setup_tip(ctx: &Ctx, info: &RepoInfo) -> Result<Option<Sha>> {
    let out = ctx.ext.git(
        &info.path,
        [
            "ls-remote",
            "--heads",
            &info.remote_name,
            &format!("refs/heads/{BRANCH}"),
        ],
    )?;
    let text = out.ok().context("원격의 chute/setup을 읽지 못했다")?;
    text.split_whitespace()
        .next()
        .map(|tip| sha(tip.to_owned()))
        .transpose()
}

pub(super) fn land(
    ctx: &Ctx,
    ready: &Ready,
    existing: Option<OpenPr>,
    seen_tip: Option<Sha>,
    overwrite: Overwrite,
    out: &mut dyn Write,
) -> Result<Landed> {
    let gitops = Gitops::new(&ctx.ext, &ctx.home, &ready.info);
    let slot = gitops.lock_preflight_slot(ready.max_parallel)?.open()?;
    slot.fetch_base()?;
    let (commit, pr) = publish(ctx, &slot, ready, existing, seen_tip, overwrite, out)?;

    let base = match conditions(ctx, &slot, ready, pr, &commit)? {
        Verdict::Go(base) => base,
        Verdict::AlreadyConfigured => {
            writeln!(
                out,
                "기본 브랜치에 chute.toml이 이미 있어 등록만 했다: {}/{}",
                ready.info.remote_name, ready.info.default_branch
            )?;
            return Ok(Landed::AlreadyConfigured);
        }
        Verdict::Moved => {
            writeln!(out, "검증한 뒤 기본 브랜치가 움직여 처음부터 다시 판정한다")?;
            return Ok(Landed::Moved { head: commit });
        }
    };

    let item = MergeItem::Pr {
        pr,
        branch: BRANCH.to_owned(),
        title: COMMIT_MESSAGE.to_owned(),
        head: commit.clone(),
    };
    let merged = slot.merge(&base, std::slice::from_ref(&item))?;
    let merge_commit = match merged
        .items
        .into_iter()
        .next()
        .map(|outcome| outcome.result)
    {
        Some(ItemResult::Merged { commit }) => commit,
        Some(ItemResult::Conflict(files)) => {
            return Err(refuse(format!(
                "{BRANCH}를 기본 브랜치와 합치지 못했다(충돌 {}개)",
                files.len()
            )));
        }
        Some(ItemResult::AlreadyMerged) | None => {
            bail!("{BRANCH}가 이미 기본 브랜치에 들어 있는데 chute.toml이 없다")
        }
    };
    if slot.tree_of(&merge_commit)? != ready.tree {
        return Err(refuse(
            "머지 결과의 트리가 검증한 트리와 다르다. 머지하지 않았다".into(),
        ));
    }
    match slot.push(&merge_commit)? {
        PushOutcome::MainMoved => {
            writeln!(out, "검증한 뒤 기본 브랜치가 움직여 처음부터 다시 판정한다")?;
            return Ok(Landed::Moved { head: commit });
        }
        PushOutcome::Pushed => {}
    }
    writeln!(
        out,
        "머지했다: {}/{} {merge_commit} (PR {pr})",
        ready.info.remote_name, ready.info.default_branch
    )?;
    finish(&slot, ready, &merge_commit, out)?;
    Ok(Landed::Merged)
}

fn finish(slot: &Slot, ready: &Ready, merged: &Sha, out: &mut dyn Write) -> Result<()> {
    let work = ready.info.path.join(config::FILE_NAME);
    if work.is_file() {
        let landed = slot.blob_id(merged, config::FILE_NAME)?;
        let mine = slot.blob_id_as_added(config::FILE_NAME, &work)?;
        if landed.as_ref() == Some(&mine) {
            fs::remove_file(&work).with_context(|| format!("지우지 못했다: {}", work.display()))?;
            writeln!(
                out,
                "작업 폴더의 chute.toml이 머지된 것과 같아 지웠다: {}",
                work.display()
            )?;
        } else {
            writeln!(
                out,
                "작업 폴더의 chute.toml이 머지된 것과 달라 그대로 둔다: {}",
                work.display()
            )?;
        }
    }
    writeln!(out)?;
    writeln!(
        out,
        "`git pull`로 기본 브랜치를 받는다. 다음은 `chute install`이다. 첫 `chute submit`이 데몬을 띄운다."
    )?;
    Ok(())
}

fn publish(
    ctx: &Ctx,
    slot: &Slot,
    ready: &Ready,
    existing: Option<OpenPr>,
    seen_tip: Option<Sha>,
    overwrite: Overwrite,
    out: &mut dyn Write,
) -> Result<(Sha, PrNumber)> {
    let repo = &ready.info.path;
    if let Some(pr) = existing {
        if slot.fetch_pr_head(pr.number, &pr.head)? != crate::gitops::PrHead::Unchanged {
            return Err(refuse(format!(
                "PR {}의 HEAD가 그 사이 바뀌었다. 다시 실행한다",
                pr.number
            )));
        }
        if is_reusable(slot, ready, &pr.head)? {
            writeln!(out, "열린 PR {}의 내용이 같아 그대로 쓴다", pr.number)?;
            return Ok((pr.head, pr.number));
        }
        let commit = build(slot, ready)?;
        slot.push_branch(&commit, BRANCH, Some(&pr.head))?;
        writeln!(
            out,
            "PR {}의 {BRANCH}를 새 setup 커밋으로 고쳤다(덮어쓴 HEAD {})",
            pr.number, pr.head
        )?;
        return Ok((commit, pr.number));
    }
    let commit = match slot.fetch_remote_branch(BRANCH)? {
        None => {
            let commit = build(slot, ready)?;
            slot.push_branch(&commit, BRANCH, None)?;
            commit
        }
        Some(tip) if is_reusable(slot, ready, &tip)? => tip,
        Some(tip) => {
            overwrite_left_over(ready, &tip, seen_tip.as_ref(), overwrite)?;
            let commit = build(slot, ready)?;
            slot.push_branch(&commit, BRANCH, Some(&tip))?;
            writeln!(
                out,
                "열린 PR 없이 남은 {BRANCH}(HEAD {tip})를 새 setup 커밋으로 덮어썼다"
            )?;
            commit
        }
    };
    let steps = ready.steps.join(", ");
    let body = format!(
        "`chute init --setup`이 만들었다.\n\n검증한 단계: {steps}\n\n이 PR은 chute가 큐를 거치지 않고 기본 브랜치에 직접 머지한다."
    );
    let created = ctx.ext.gh(
        repo,
        [
            "pr",
            "create",
            "--base",
            &ready.info.default_branch,
            "--head",
            BRANCH,
            "--title",
            COMMIT_MESSAGE,
            "--body",
            &body,
        ],
    )?;
    created.ok().context("setup PR을 만들지 못했다")?;
    let Some(pr) = find_open_pr(ctx, &ready.info)? else {
        bail!("PR을 만들었는데 열린 {BRANCH} PR이 보이지 않는다");
    };
    if pr.head != commit {
        return Err(refuse(format!(
            "PR {}의 HEAD가 방금 push한 setup 커밋과 다르다",
            pr.number
        )));
    }
    writeln!(out, "PR {}을 만들었다", pr.number)?;
    Ok((commit, pr.number))
}

/// Lets `publish` overwrite the remote `chute/setup` at `tip` only when the terminal user saw
/// exactly that head.
fn overwrite_left_over(
    ready: &Ready,
    tip: &Sha,
    seen_tip: Option<&Sha>,
    overwrite: Overwrite,
) -> Result<()> {
    match overwrite {
        Overwrite::Refuse => Err(refuse(format!(
            "원격에 {BRANCH}가 이미 있고 열린 PR이 없다(HEAD {tip}). 덮어쓰지 않고 멈춘다. \
             터미널에서 실행해 덮어쓸지 확인하거나, 필요 없는 브랜치면 \
             `git push {} --delete {BRANCH}`로 지운 뒤 다시 실행한다",
            ready.info.remote_name
        ))),
        Overwrite::Confirmed if seen_tip != Some(tip) => Err(refuse(format!(
            "원격 {BRANCH}가 확인한 뒤 바뀌었다(확인 {}, 지금 {tip}). 덮어쓰지 않고 멈춘다. 다시 실행한다",
            seen_tip.map_or_else(|| "브랜치 없음".to_owned(), ToString::to_string)
        ))),
        Overwrite::Confirmed => Ok(()),
    }
}

fn build(slot: &Slot, ready: &Ready) -> Result<Sha> {
    let files: Vec<(&str, &[u8])> = ready
        .files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_slice()))
        .collect();
    let commit = slot.commit_files(&ready.base, &files, COMMIT_MESSAGE)?;
    if slot.tree_of(&commit)? != ready.tree {
        bail!("다시 만든 setup 커밋의 트리가 검증한 트리와 다르다");
    }
    Ok(commit)
}

/// A commit the setup can use as it is: the verified tree on top of the verified base alone.
/// Merging it brings in nothing that the verification did not see.
fn is_reusable(slot: &Slot, ready: &Ready, commit: &Sha) -> Result<bool> {
    Ok(slot.tree_of(commit)? == ready.tree
        && slot.parents_of(commit)? == std::slice::from_ref(&ready.base))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Go(Sha),
    AlreadyConfigured,
    Moved,
}

pub(super) fn conditions(
    ctx: &Ctx,
    slot: &Slot,
    ready: &Ready,
    pr: PrNumber,
    commit: &Sha,
) -> Result<Verdict> {
    let info = &ready.info;
    let base = slot.fetch_base()?;
    if slot.read_file(&base, config::FILE_NAME)?.is_some() {
        return Ok(Verdict::AlreadyConfigured);
    }
    if base != ready.base {
        return Ok(Verdict::Moved);
    }
    let viewed = ctx.ext.gh(
        &info.path,
        [
            "pr",
            "view",
            &pr.0.to_string(),
            "--json",
            "state,baseRefName,headRefName,isCrossRepository",
        ],
    )?;
    let viewed = viewed
        .ok()
        .with_context(|| format!("PR {pr}을 gh로 읽지 못했다"))?;
    let viewed: ViewedPr =
        serde_json::from_str(&viewed).context("gh pr view의 출력을 읽지 못했다")?;
    if viewed.state != "OPEN" {
        return Err(refuse(format!(
            "PR {pr}이 열려 있지 않다({})",
            viewed.state
        )));
    }
    if viewed.base != info.default_branch {
        return Err(refuse(format!(
            "PR {pr}의 base가 기본 브랜치 {}이 아니라 {}이다",
            info.default_branch, viewed.base
        )));
    }
    if viewed.head_name != BRANCH || viewed.cross_repository {
        return Err(refuse(format!(
            "PR {pr}의 head가 이 저장소의 {BRANCH}가 아니다"
        )));
    }
    if slot.fetch_pr_head(pr, commit)? != crate::gitops::PrHead::Unchanged {
        return Err(refuse(format!(
            "PR {pr}의 HEAD가 확인한 setup 커밋 {commit}과 다르다"
        )));
    }
    let changed = slot.changed_since_fork(&base, commit)?;
    let instructions = &ready.instructions;
    for path in &changed {
        if !ready.files.iter().any(|(file, _)| file == path) {
            return Err(refuse(format!(
                "PR {pr}가 {}과 지침 파일 밖의 파일 {path}도 바꾼다. 머지하지 않았다",
                config::FILE_NAME
            )));
        }
    }
    for (path, _) in &ready.files {
        if !changed.iter().any(|changed| changed == path) {
            return Err(refuse(format!(
                "PR {pr}가 {path}를 바꾸지 않는다. 머지하지 않았다"
            )));
        }
        if slot.read_file(commit, path)? != slot.read_file(&ready.tree, path)? {
            let what = if *path == instructions.file {
                "지침 파일의 변경이 chute 한 줄 추가가 아니다"
            } else {
                "chute.toml이 검증한 초안과 다르다"
            };
            return Err(refuse(format!("PR {pr}의 {path}: {what}. 머지하지 않았다")));
        }
    }
    if !is_reusable(slot, ready, commit)? {
        return Err(refuse(format!(
            "PR {pr}의 HEAD {commit}가 검증한 setup 커밋이 아니다. 트리가 검증한 트리와 다르거나 부모가 기준 커밋 {} 하나가 아니다. 머지하지 않았다",
            ready.base
        )));
    }
    Ok(Verdict::Go(base))
}
