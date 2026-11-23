use std::collections::{BTreeMap, HashMap};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};

use super::Ctx;
use crate::config::{self, Config};
use crate::hub::{self, Route};
use crate::machine::Machines;
use crate::model::{
    Agent, AgentView, Approval, Batch, DaemonInfo, DaemonView, EpochMs, Event, EventKind, HubView,
    ItemState, ItemView, MachineId, MergedView, PostPhase, PostState, PrNumber, QueueItem,
    RecentBatch, RepoInfo, RepoView, SchemaV1, StatusView, TerminalId, Today, Usual, VerifiedBy,
    Warning, WarningKind,
};
use crate::queue;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 상태를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
    /// 등록된 모든 저장소를 배열로 출력한다
    #[arg(long)]
    pub all: bool,
}

const RECENT_BATCHES: usize = 10;
const RECENT_MERGED: usize = 5;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(20);

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let now = Local::now();
    let views = if args.all {
        let repos = ctx.home.repos()?;
        std::thread::scope(|scope| {
            let jobs: Vec<_> = repos
                .iter()
                .map(|info| scope.spawn(|| view_from_hub(ctx, info, &now)))
                .collect();
            jobs.into_iter()
                .map(|job| job.join().expect("status thread panicked"))
                .collect::<Result<Vec<_>>>()
        })?
    } else {
        let (info, _) = ctx.registered_repo()?;
        vec![view_from_hub(ctx, &info, &now)?]
    };
    if args.json {
        let text = if args.all {
            serde_json::to_string_pretty(&views)?
        } else {
            serde_json::to_string_pretty(&views[0])?
        };
        println!("{text}");
    } else if views.is_empty() {
        println!("등록된 저장소가 없다. 저장소에서 `chute init`을 실행한다.");
    } else {
        let text: Vec<String> = views.iter().map(human).collect();
        print!("{}", text.join("\n"));
    }
    Ok(ExitCode::SUCCESS)
}

fn view_from_hub(ctx: &Ctx, info: &RepoInfo, now: &DateTime<Local>) -> Result<StatusView> {
    if ctx.forwarded() {
        return view(ctx, info, now);
    }
    match hub::elsewhere(&ctx.home, &ctx.ext, &info.slug) {
        Ok(None) => view(ctx, info, now),
        Ok(Some(_)) => Ok(from_remote_hub(ctx, info, now)),
        Err(e) => {
            let hub = HubView {
                local: false,
                machine_id: ctx.home.repo(&info.slug).hub().ok().flatten(),
                host: None,
                machine: None,
                repo_path: None,
                reachable: false,
                error: None,
            };
            Ok(hub_down(ctx, info, hub, format!("{e:#}"), now))
        }
    }
}

fn from_remote_hub(ctx: &Ctx, info: &RepoInfo, now: &DateTime<Local>) -> StatusView {
    let seen = match hub::view(&ctx.home, &ctx.ext, &info.slug, info.path.clone()) {
        Ok(seen) => seen,
        Err(e) => {
            let hub = HubView {
                local: false,
                machine_id: None,
                host: None,
                machine: None,
                repo_path: None,
                reachable: false,
                error: None,
            };
            return hub_down(ctx, info, hub, format!("{e:#}"), now);
        }
    };
    if !seen.view.reachable {
        let reason = seen.view.error.clone().unwrap_or_default();
        return hub_down(ctx, info, seen.view, reason, now);
    }
    match ask_hub(ctx, info) {
        Ok(received) => rebase(ctx, info, received, now),
        Err(reason) => {
            let hub = hub::view(&ctx.home, &ctx.ext, &info.slug, info.path.clone())
                .map_or(seen.view, |now_seen| now_seen.view);
            hub_down(ctx, info, hub, reason, now)
        }
    }
}

fn ask_hub(ctx: &Ctx, info: &RepoInfo) -> Result<StatusView, String> {
    let route = hub::route(&ctx.home, &ctx.ext, &info.slug).map_err(|e| format!("{e:#}"))?;
    if route == Route::Local {
        return Err("허브 설정이 바뀌어 이 PC가 허브가 됐다. 다시 실행한다".to_owned());
    }
    let answer = hub::forward(
        &ctx.home,
        &ctx.ext,
        &info.slug,
        &route,
        &["status", "--json"],
        FORWARD_TIMEOUT,
    )
    .map_err(|e| format!("{e:#}"))?;
    if answer.code != 0 {
        let stderr = String::from_utf8_lossy(&answer.stderr);
        return Err(format!(
            "허브가 종료 코드 {}로 끝났다: {}",
            answer.code,
            stderr.trim()
        ));
    }
    if answer.truncated {
        return Err("허브의 상태 출력이 너무 커서 뒤가 잘렸다".to_owned());
    }
    serde_json::from_slice(&answer.stdout)
        .map_err(|e| format!("허브가 보낸 상태를 읽지 못했다: {e}"))
}

fn rebase(ctx: &Ctx, info: &RepoInfo, mut v: StatusView, now: &DateTime<Local>) -> StatusView {
    let store = ctx.home.repo(&info.slug);
    let seen = match hub::view(&ctx.home, &ctx.ext, &info.slug, info.path.clone()) {
        Ok(seen) => seen,
        Err(e) => return hub_down(ctx, info, v.hub, format!("{e:#}"), now),
    };
    let hub_path = std::mem::replace(&mut v.repo.path, info.path.clone());
    v.repo.state_dir = store.dir().to_owned();
    v.hub = HubView {
        local: false,
        machine_id: seen.view.machine_id.clone(),
        host: v.hub.host.take().or(seen.view.host.clone()),
        machine: seen.view.machine.clone(),
        repo_path: Some(hub_path),
        reachable: true,
        error: None,
    };
    if let Some(note) = seen.note {
        v.warnings.push(hub_warning(note, now));
    }
    if let Some(machines) = &seen.machines {
        let hub_id = seen.view.machine_id.as_ref();
        if let Some(hub_id) = hub_id {
            v.daemon.terminal = v
                .daemon
                .terminal
                .take()
                .map(|t| machines.address(hub_id, &bare(&t)));
        }
        for item in &mut v.queue {
            item.agent = item.agent.take().map(|a| rebase_agent(machines, hub_id, a));
            item.submitter = item
                .submitter
                .take()
                .map(|a| rebase_agent(machines, hub_id, a));
        }
    }
    v
}

fn bare(terminal: &TerminalId) -> TerminalId {
    TerminalId(
        terminal
            .0
            .rsplit('/')
            .next()
            .unwrap_or(&terminal.0)
            .to_owned(),
    )
}

fn rebase_agent(machines: &Machines, hub: Option<&MachineId>, agent: AgentView) -> AgentView {
    let Some(id) = agent.machine_id.clone().or(hub.cloned()) else {
        return agent;
    };
    let agent = AgentView {
        machine_id: Some(id.clone()),
        ..agent
    };
    if id == machines.local().id {
        return AgentView {
            terminal: bare(&agent.terminal),
            host: Some(machines.local().name.clone()),
            machine: None,
            ..agent
        };
    }
    match machines.by_id(&id) {
        Some(m) => AgentView {
            terminal: machines.address(&id, &bare(&agent.terminal)),
            host: m.host.clone().or(agent.host),
            machine: Some(m.name.clone()),
            ..agent
        },
        None => AgentView {
            machine: None,
            ..agent
        },
    }
}

fn hub_warning(message: String, now: &DateTime<Local>) -> Warning {
    Warning {
        kind: WarningKind::Hub,
        message,
        pr: None,
        at_ms: EpochMs(now.timestamp_millis().max(0) as u64),
    }
}

fn hub_down(
    ctx: &Ctx,
    info: &RepoInfo,
    mut hub: HubView,
    reason: String,
    now: &DateTime<Local>,
) -> StatusView {
    let name = hub
        .machine
        .clone()
        .or(hub.machine_id.as_ref().map(|id| id.to_string()))
        .unwrap_or_else(|| "?".to_owned());
    let message = if reason.starts_with("허브") {
        reason
    } else {
        format!("허브 `{name}`에 닿지 않는다: {reason}")
    };
    hub.reachable = false;
    hub.repo_path = None;
    hub.error = Some(message.clone());
    let approval = match Config::load_remote_branch(
        &ctx.ext,
        &info.path,
        &info.remote_name,
        &info.default_branch,
    ) {
        Ok(Some(c)) => c.approval,
        _ => Approval::default(),
    };
    StatusView {
        schema: SchemaV1,
        repo: RepoView {
            slug: info.slug.clone(),
            path: info.path.clone(),
            default_branch: info.default_branch.clone(),
            state_dir: ctx.home.repo(&info.slug).dir().to_owned(),
        },
        hub,
        approval,
        daemon: DaemonView {
            running: false,
            terminal: None,
            pid: None,
            doing: None,
        },
        blocked: None,
        batch: None,
        queue: Vec::new(),
        today: Today::default(),
        recent: Vec::new(),
        recent_merged: Vec::new(),
        warnings: vec![hub_warning(message, now)],
        post_verify: PostState::default(),
        usual: Usual::default(),
    }
}

pub fn view(ctx: &Ctx, info: &RepoInfo, now: &DateTime<Local>) -> Result<StatusView> {
    let store = ctx.home.repo(&info.slug);
    let state = store.load()?;
    let mut warnings = state.warnings.clone();
    let loaded = Config::load_remote_branch(
        &ctx.ext,
        &info.path,
        &info.remote_name,
        &info.default_branch,
    );
    let (approval, budget) = match loaded {
        Ok(Some(c)) => (c.approval, c.budget),
        failed => {
            let message = match failed {
                Ok(_) => format!(
                    "설정을 읽지 못했다: chute.toml이 {}/{}에 없다. 기본 브랜치에 올렸는지 확인하고 `git fetch`를 먼저 실행한다",
                    info.remote_name, info.default_branch
                ),
                Err(e) => format!("{e:#}"),
            };
            warnings.push(Warning {
                kind: WarningKind::Config,
                message,
                pr: None,
                at_ms: EpochMs(now.timestamp_millis().max(0) as u64),
            });
            (Approval::default(), config::DEFAULT_BUDGET)
        }
    };
    let seen = hub::view(&ctx.home, &ctx.ext, &info.slug, info.path.clone())?;
    if let Some(note) = seen.note {
        warnings.push(Warning {
            kind: WarningKind::Hub,
            message: note,
            pr: None,
            at_ms: EpochMs(now.timestamp_millis().max(0) as u64),
        });
    }
    let agent_view = |agent: &Agent| match (&seen.machines, &seen.view.machine_id) {
        (Some(machines), Some(home)) => machines.agent_view(home, agent),
        _ => AgentView {
            terminal: agent.terminal.clone(),
            name: agent.name.clone(),
            alive: agent.alive,
            machine_id: agent.machine.clone(),
            host: None,
            machine: None,
        },
    };
    let running = store.daemon_running()?;
    let daemon = if running { store.daemon()? } else { None };
    let history = store.history()?;
    if !history.broken_lines.is_empty() {
        warnings.push(Warning {
            kind: WarningKind::History,
            message: format!(
                "history.jsonl의 깨진 줄을 건너뛰었다: {:?}",
                history.broken_lines
            ),
            pr: None,
            at_ms: EpochMs(now.timestamp_millis().max(0) as u64),
        });
    }
    let (today, recent, recent_merged) = summarize(&history.events, now);
    let last_bounced = last_bounced_at(&history.events);
    Ok(StatusView {
        schema: SchemaV1,
        repo: RepoView {
            slug: info.slug.clone(),
            path: info.path.clone(),
            default_branch: info.default_branch.clone(),
            state_dir: store.dir().to_owned(),
        },
        hub: seen.view.clone(),
        approval,
        daemon: daemon_view(running, daemon),
        blocked: state.blocked.clone(),
        batch: state.batch.clone(),
        queue: state
            .queue
            .iter()
            .map(|item| {
                ItemView::new(
                    item,
                    budget,
                    state.preflights.get(&item.branch),
                    item.agent.as_ref().map(agent_view),
                    item.submitter.as_ref().map(agent_view),
                    bounced_ms(item, &last_bounced),
                )
            })
            .collect(),
        today,
        recent,
        recent_merged,
        warnings,
        post_verify: state.post.clone(),
        usual: usual(&history.events, state.batch.as_ref()),
    })
}

fn daemon_view(running: bool, info: Option<DaemonInfo>) -> DaemonView {
    let info = info.filter(|_| running);
    DaemonView {
        running,
        terminal: info.as_ref().and_then(|d| d.terminal.clone()),
        pid: info.as_ref().map(|d| d.pid),
        doing: info.and_then(|d| d.doing),
    }
}

const USUAL_RUNS: usize = 20;
const USUAL_MIN_SAMPLES: usize = 3;

fn median(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    match values.len() {
        0 => None,
        n if n % 2 == 1 => Some(values[n / 2]),
        n => {
            let (a, b) = (values[n / 2 - 1], values[n / 2]);
            Some(a / 2 + b / 2 + (a % 2 + b % 2) / 2)
        }
    }
}

fn usual_median(recent_first: impl Iterator<Item = u64>) -> Option<u64> {
    let last: Vec<u64> = recent_first.take(USUAL_RUNS).collect();
    if last.len() < USUAL_MIN_SAMPLES {
        return None;
    }
    median(last)
}

pub fn usual(events: &[Event], batch: Option<&Batch>) -> Usual {
    struct Finished<'a> {
        seconds: u64,
        ran: &'a [String],
        verifications: u32,
        step_times: &'a [crate::model::StepTime],
        merged_any: bool,
        bounced_any: bool,
    }
    let finished = || {
        events.iter().rev().filter_map(|e| match &e.kind {
            EventKind::BatchFinished {
                seconds,
                ran,
                verifications,
                step_times,
                merged,
                bounced,
                ..
            } => Some(Finished {
                seconds: *seconds,
                ran,
                verifications: *verifications,
                step_times,
                merged_any: !merged.is_empty(),
                bounced_any: !bounced.is_empty(),
            }),
            _ => None,
        })
    };
    let mut by_step: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for run in finished() {
        for time in run.step_times.iter().rev() {
            let runs = by_step.entry(time.step.as_str()).or_default();
            if runs.len() < USUAL_RUNS {
                runs.push(time.seconds);
            }
        }
    }
    let steps = by_step
        .into_iter()
        .filter_map(|(step, runs)| usual_median(runs.into_iter()).map(|s| (step.to_owned(), s)))
        .collect();
    let batch_s = batch.filter(|b| !b.steps.is_empty()).and_then(|b| {
        usual_median(
            finished()
                .filter(|run| {
                    run.verifications == 1
                        && !run.bounced_any
                        && run.merged_any
                        && run.ran == b.steps
                })
                .map(|run| run.seconds),
        )
    });
    let post_verify_s = usual_median(events.iter().rev().filter_map(|e| match &e.kind {
        EventKind::PostGreen { seconds, .. } => Some(*seconds),
        _ => None,
    }));
    Usual {
        batch_s,
        steps,
        post_verify_s,
    }
}

/// When a bounced item last bounced; other states have no such time.
fn bounced_ms(item: &QueueItem, last: &HashMap<PrNumber, EpochMs>) -> Option<EpochMs> {
    match item.state {
        ItemState::Bounced(_) => last.get(&item.pr).copied(),
        _ => None,
    }
}

/// The time of the latest `bounced` event of each PR.
pub fn last_bounced_at(events: &[Event]) -> HashMap<PrNumber, EpochMs> {
    let mut at = HashMap::new();
    for event in events {
        if let EventKind::Bounced { pr, .. } = &event.kind {
            at.insert(*pr, event.at_ms);
        }
    }
    at
}

pub fn summarize<Tz: TimeZone>(
    events: &[Event],
    now: &DateTime<Tz>,
) -> (Today, Vec<RecentBatch>, Vec<MergedView>) {
    let day = now.date_naive();
    let tz = now.timezone();
    let is_today = |ms: EpochMs| {
        tz.timestamp_millis_opt(ms.0 as i64)
            .single()
            .is_some_and(|t| t.date_naive() == day)
    };
    let mut today = Today::default();
    let mut leads = Vec::new();
    let mut recent = Vec::new();
    let mut recent_merged = Vec::new();
    let mut bounces: HashMap<PrNumber, u32> = HashMap::new();
    for event in events {
        match &event.kind {
            EventKind::Bounced { pr, .. } => *bounces.entry(*pr).or_default() += 1,
            EventKind::Removed { pr, .. } => {
                bounces.remove(pr);
            }
            EventKind::Merged {
                pr,
                batch,
                title,
                submitted_ms,
                ..
            } => recent_merged.push(MergedView {
                pr: *pr,
                title: title.clone(),
                batch: batch.clone(),
                merged_ms: event.at_ms,
                bounces: bounces.remove(pr).unwrap_or(0),
                submitted_ms: Some(*submitted_ms),
            }),
            _ => {}
        }
        if let EventKind::BatchFinished {
            batch,
            merged,
            bounced,
            seconds,
            ..
        } = &event.kind
        {
            recent.push(RecentBatch {
                batch: batch.clone(),
                merged: merged.clone(),
                bounced: bounced.clone(),
                seconds: *seconds,
                finished_ms: event.at_ms,
            });
        }
        if !is_today(event.at_ms) {
            continue;
        }
        match &event.kind {
            EventKind::Merged { submitted_ms, .. } => {
                today.merged += 1;
                leads.push(event.at_ms.0.saturating_sub(submitted_ms.0) / 1000);
            }
            EventKind::Bounced { .. } => today.bounced += 1,
            EventKind::Flaky { .. } => today.flaky += 1,
            EventKind::BatchFinished { verify_s, .. } => {
                today.batches += 1;
                today.verify_s += verify_s;
            }
            EventKind::Reverted { .. }
            | EventKind::GaveUp { .. }
            | EventKind::Closed { .. }
            | EventKind::NeedsHuman { .. }
            | EventKind::Removed { .. }
            | EventKind::Blocked { .. }
            | EventKind::Submitted { .. }
            | EventKind::Dropped { .. }
            | EventKind::Notified { .. }
            | EventKind::Resumed { .. }
            | EventKind::FlakyProbe { .. }
            | EventKind::FlakyResolved { .. }
            | EventKind::Reused { .. }
            | EventKind::PostGreen { .. }
            | EventKind::PostFailed { .. }
            | EventKind::PostFixed { .. } => {}
        }
    }
    today.median_lead_s = median(leads);
    recent.reverse();
    recent.truncate(RECENT_BATCHES);
    recent_merged.reverse();
    recent_merged.truncate(RECENT_MERGED);
    (today, recent, recent_merged)
}

fn post_lines(post: &PostState) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(run) = &post.run {
        let what = match run.phase {
            PostPhase::Verifying => "검사 중",
            PostPhase::Searching => "범인을 찾는 중",
        };
        let waiting = if run.waiting_machine {
            " · PC 자리를 기다리는 중"
        } else {
            ""
        };
        lines.push(format!(
            "머지 뒤 검사 {what}: 기본 브랜치 {} · 확인하는 커밋 {} · 단계 {}{waiting}",
            run.tip.short(),
            run.commit.short(),
            run.step.as_deref().unwrap_or("-")
        ));
    }
    if let Some(red) = &post.red {
        let culprit = red.culprit.as_ref().map_or(String::new(), |c| {
            let who =
                c.pr.map_or(format!("범인 커밋 {}", c.commit.short()), |pr| {
                    format!("범인 {pr}")
                });
            let revert = c.batch.as_ref().map_or(String::new(), |b| {
                format!(" · 되돌리려면 chute revert {}", b.0)
            });
            format!(" · {who}{revert}")
        });
        let green = red
            .last_green
            .as_ref()
            .map_or(String::new(), |g| format!(" · 마지막 통과 {}", g.short()));
        lines.push(format!(
            "머지 뒤 검사 실패: 커밋 {} · 단계 {}{green}{culprit}",
            red.commit.short(),
            red.step
        ));
    }
    if let Some(green) = &post.last_green {
        lines.push(format!(
            "머지 뒤 검사 마지막 통과: {}",
            green.commit.short()
        ));
    }
    lines
}

fn human(v: &StatusView) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    let daemon = match (&v.daemon.running, &v.daemon.terminal) {
        (true, Some(t)) => format!("실행 중 ({})", t.0),
        (true, None) => "실행 중".to_owned(),
        (false, _) => "멈춤".to_owned(),
    };
    let approval = match v.approval {
        Approval::Agent => "자동 머지",
        Approval::Human => "사람 승인",
    };
    line(format!(
        "{} ({}) · {approval} · 데몬 {daemon}",
        v.repo.slug, v.repo.default_branch
    ));
    if !v.hub.local {
        let name = v.hub.machine.as_deref().unwrap_or("?");
        let host = v
            .hub
            .host
            .as_deref()
            .map_or(String::new(), |h| format!(" ({h})"));
        let reach = if v.hub.reachable {
            ""
        } else {
            " · 닿지 않음"
        };
        line(format!("허브: {name}{host}{reach}"));
    }
    if let Some(b) = &v.blocked {
        line(format!(
            "큐 멈춤: 기준 커밋 {}이 검증을 통과하지 못한다",
            b.base
        ));
    }
    if let Some(b) = &v.batch {
        let prs: Vec<String> = b.prs.iter().map(|p| p.to_string()).collect();
        let waiting = if b.waiting_machine {
            " · PC 자리를 기다리는 중"
        } else {
            ""
        };
        let reused = match b.reused {
            Some(VerifiedBy::Preflight) => " · preflight의 통과 결과를 다시 씀",
            Some(VerifiedBy::Batch) => " · 앞선 batch의 통과 결과를 다시 씀",
            None => "",
        };
        line(format!(
            "batch {} {:?} {} · {}회차 · {}{waiting}{reused}",
            b.id.0,
            b.phase,
            b.step.as_deref().unwrap_or("-"),
            b.round,
            prs.join(" ")
        ));
    }
    for text in post_lines(&v.post_verify) {
        line(text);
    }
    if v.queue.is_empty() {
        line("큐가 비어 있다".to_owned());
    }
    for item in &v.queue {
        let agent = item.agent.as_ref().map_or("에이전트 없음".to_owned(), |a| {
            a.name.clone().unwrap_or(a.terminal.0.clone())
        });
        line(format!(
            "  {} {} · {} · {}",
            item.pr,
            queue::label(item.state),
            agent,
            item.title
        ));
    }
    let t = &v.today;
    let lead = t.median_lead_s.map_or("-".to_owned(), |s| format!("{s}초"));
    line(format!(
        "오늘: 머지 {} · 반송 {} · flaky {} · batch {} · 머지까지 중앙값 {lead}",
        t.merged, t.bounced, t.flaky, t.batches
    ));
    for w in &v.warnings {
        line(format!("경고: {}", w.message));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{Core, Machine};
    use crate::model::{BatchId, BounceReason, PrNumber, Sha, StepTime};
    use chrono::Utc;

    fn id(s: &str) -> MachineId {
        MachineId::try_from(s.to_owned()).unwrap()
    }

    fn viewer() -> Machines {
        let known = |s: &str, name: &str| Machine {
            id: id(s),
            name: name.into(),
            host: Some(format!("{name}-host")),
            state: "connected".into(),
        };
        Machines::new(
            Core {
                id: id("pc"),
                name: "pc-host".into(),
            },
            vec![known("hub", "main"), known("lap", "lappy")],
        )
    }

    fn agent(terminal: &str, machine: &str) -> AgentView {
        AgentView {
            terminal: TerminalId(terminal.into()),
            name: None,
            alive: true,
            machine_id: Some(id(machine)),
            host: Some("from-hub".into()),
            machine: Some("hub-name".into()),
        }
    }

    #[test]
    fn rebase_writes_agent_addresses_in_the_viewers_words() {
        let m = viewer();
        let show = |terminal: &str, machine: &str| {
            let a = rebase_agent(&m, None, agent(terminal, machine));
            (a.terminal.0, a.host, a.machine)
        };
        assert_eq!(
            show("t7", "hub"),
            (
                "main/t7".into(),
                Some("main-host".into()),
                Some("main".into())
            )
        );
        assert_eq!(
            show("lap/t7", "lap"),
            (
                "lappy/t7".into(),
                Some("lappy-host".into()),
                Some("lappy".into())
            )
        );
        assert_eq!(
            show("hubside/t7", "pc"),
            ("t7".into(), Some("pc-host".into()), None)
        );
        assert_eq!(
            show("desk/t7", "zzz"),
            ("desk/t7".into(), Some("from-hub".into()), None)
        );
    }

    #[test]
    fn an_agent_without_a_machine_lives_on_the_hub() {
        let m = viewer();
        let mut a = agent("t7", "hub");
        a.machine_id = None;
        let a = rebase_agent(&m, Some(&id("hub")), a);
        assert_eq!(
            (a.terminal.0, a.machine_id, a.machine),
            ("main/t7".into(), Some(id("hub")), Some("main".into()))
        );
    }

    #[test]
    fn the_daemon_terminal_of_a_hub_elsewhere_gets_the_hubs_name() {
        let m = viewer();
        assert_eq!(
            m.address(&id("hub"), &bare(&TerminalId("t1zz".into()))).0,
            "main/t1zz"
        );
    }

    fn at(now: &DateTime<Utc>, minutes_ago: i64) -> EpochMs {
        EpochMs((now.timestamp_millis() - minutes_ago * 60_000) as u64)
    }

    fn finished_with(
        seconds: u64,
        ran: &[&str],
        verifications: u32,
        steps: &[(&str, u64)],
    ) -> Event {
        Event {
            at_ms: EpochMs(1),
            kind: EventKind::BatchFinished {
                batch: BatchId("b".into()),
                merged: vec![PrNumber(1)],
                bounced: vec![],
                seconds,
                verify_s: seconds,
                ran: ran.iter().map(|s| (*s).to_owned()).collect(),
                verifications,
                step_times: steps
                    .iter()
                    .map(|(step, seconds)| StepTime {
                        step: (*step).to_owned(),
                        seconds: *seconds,
                    })
                    .collect(),
            },
        }
    }

    fn running_batch(steps: &[&str]) -> Batch {
        Batch {
            id: BatchId("now".into()),
            base: Sha::try_from("abc1234".to_owned()).unwrap(),
            prs: vec![PrNumber(1)],
            phase: crate::model::Phase::Verifying,
            step: None,
            steps: steps.iter().map(|s| (*s).to_owned()).collect(),
            round: 1,
            started_ms: EpochMs(1),
            waiting_machine: false,
            reused: None,
            step_started_ms: None,
            done: Vec::new(),
            skipped: Vec::new(),
            reruns: Vec::new(),
        }
    }

    #[test]
    fn the_usual_step_time_is_the_median_of_the_last_runs_of_that_step() {
        let mut events: Vec<Event> = (1..=25)
            .map(|n| finished_with(n, &["test"], 1, &[("test", n)]))
            .collect();
        events.push(finished_with(5, &["lint"], 1, &[("lint", 5)]));
        events.push(finished_with(7, &["lint"], 1, &[("lint", 7)]));

        let usual = usual(&events, None);

        assert_eq!(
            usual.steps.get("test"),
            Some(&15),
            "runs 6 to 25, the middle two are 15 and 16"
        );
        assert!(!usual.steps.contains_key("lint"), "two runs are too few");
    }

    #[test]
    fn the_usual_batch_time_counts_only_single_round_batches_that_ran_the_same_steps() {
        let now = running_batch(&["fmt", "test"]);
        let same = |seconds| finished_with(seconds, &["fmt", "test"], 1, &[]);
        let events = vec![
            same(100),
            finished_with(900, &["fmt"], 1, &[]),
            same(300),
            finished_with(900, &["fmt", "test"], 2, &[]),
            finished_with(900, &["test", "fmt"], 1, &[]),
            same(200),
        ];

        assert_eq!(usual(&events, Some(&now)).batch_s, Some(200));
        assert_eq!(usual(&events[..3], Some(&now)).batch_s, None, "two samples");
        assert_eq!(usual(&events, None).batch_s, None, "no batch now");
        assert_eq!(
            usual(&events, Some(&running_batch(&[]))).batch_s,
            None,
            "a batch with no steps has nothing to compare"
        );
    }

    #[test]
    fn a_bounced_batch_is_not_a_sample_of_the_usual_batch_time() {
        let now = running_batch(&["fmt", "test"]);
        let merged = |seconds| finished_with(seconds, &["fmt", "test"], 1, &[]);
        let outcome = |seconds, merged: &[u64], bounced: &[u64]| {
            let mut event = finished_with(seconds, &["fmt", "test"], 1, &[]);
            if let EventKind::BatchFinished {
                merged: m,
                bounced: b,
                ..
            } = &mut event.kind
            {
                *m = merged.iter().copied().map(PrNumber).collect();
                *b = bounced.iter().copied().map(PrNumber).collect();
            }
            event
        };

        let three_merged = vec![
            merged(100),
            outcome(900, &[], &[2]),
            merged(300),
            outcome(900, &[1], &[2]),
            outcome(900, &[], &[]),
            merged(200),
        ];
        assert_eq!(usual(&three_merged, Some(&now)).batch_s, Some(200));

        let mostly_bounced = vec![
            merged(100),
            outcome(900, &[], &[1]),
            outcome(900, &[], &[1, 2]),
            merged(300),
            outcome(900, &[], &[3]),
        ];
        assert_eq!(usual(&mostly_bounced, Some(&now)).batch_s, None);
    }

    #[test]
    fn the_usual_post_merge_time_is_the_median_of_recent_green_runs() {
        let green = |seconds| Event {
            at_ms: EpochMs(1),
            kind: EventKind::PostGreen {
                commit: Sha::try_from("abc1234".to_owned()).unwrap(),
                previous: None,
                seconds,
            },
        };
        let five: Vec<Event> = [400, 100, 300, 500, 200].map(green).into();

        assert_eq!(usual(&five, None).post_verify_s, Some(300));
        assert_eq!(usual(&five[..2], None).post_verify_s, None);
    }

    #[test]
    fn the_daemon_says_what_it_is_doing_only_while_it_runs() {
        let info = || DaemonInfo {
            pid: 7,
            terminal: None,
            doing: Some("큐를 보는 중".to_owned()),
            started_ms: EpochMs(1),
        };

        let alive = daemon_view(true, Some(info()));
        assert_eq!(alive.doing.as_deref(), Some("큐를 보는 중"));
        assert_eq!(alive.pid, Some(7));

        let stale = daemon_view(false, Some(info()));
        assert_eq!(stale.doing, None);
        assert_eq!(stale.pid, None);
        assert_eq!(daemon_view(true, None).doing, None);
    }

    #[test]
    fn summarize_counts_today_and_lists_recent_newest_first() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        let sha = Sha::try_from("abc1234".to_owned()).unwrap();
        let merged = |minutes_ago, lead_s: i64| Event {
            at_ms: at(&now, minutes_ago),
            kind: EventKind::Merged {
                pr: PrNumber(1),
                batch: BatchId("b".into()),
                commit: sha.clone(),
                submitted_ms: EpochMs(at(&now, minutes_ago).0 - lead_s as u64 * 1000),
                title: None,
            },
        };
        let finished = |name: &str, minutes_ago| Event {
            at_ms: at(&now, minutes_ago),
            kind: EventKind::BatchFinished {
                batch: BatchId(name.into()),
                merged: vec![PrNumber(1)],
                bounced: vec![],
                seconds: 60,
                verify_s: 50,
                ran: Vec::new(),
                verifications: 0,
                step_times: Vec::new(),
            },
        };
        let events = vec![
            finished("yesterday", 13 * 60),
            merged(13 * 60, 999),
            merged(30, 100),
            merged(20, 300),
            merged(10, 200),
            Event {
                at_ms: at(&now, 5),
                kind: EventKind::Bounced {
                    pr: PrNumber(2),
                    batch: None,
                    reason: BounceReason::Conflict,
                    attempts: 1,
                    unreachable: None,
                },
            },
            finished("first", 9),
            finished("second", 1),
        ];
        let (today, recent, _) = summarize(&events, &now);
        assert_eq!(
            today,
            Today {
                merged: 3,
                bounced: 1,
                flaky: 0,
                batches: 2,
                median_lead_s: Some(200),
                verify_s: 100,
            }
        );
        let names: Vec<&str> = recent.iter().map(|r| r.batch.0.as_str()).collect();
        assert_eq!(names, ["second", "first", "yesterday"]);
    }

    #[test]
    fn recent_merged_lists_five_newest_first_with_bounces_counted_before_the_merge() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        let sha = Sha::try_from("abc1234".to_owned()).unwrap();
        let bounced = |pr, minutes_ago| Event {
            at_ms: at(&now, minutes_ago),
            kind: EventKind::Bounced {
                pr: PrNumber(pr),
                batch: None,
                reason: BounceReason::Conflict,
                attempts: 1,
                unreachable: None,
            },
        };
        let merged = |pr, minutes_ago, title: Option<&str>| Event {
            at_ms: at(&now, minutes_ago),
            kind: EventKind::Merged {
                pr: PrNumber(pr),
                batch: BatchId(format!("b{pr}")),
                commit: sha.clone(),
                submitted_ms: EpochMs(1),
                title: title.map(str::to_owned),
            },
        };
        let events = vec![
            merged(1, 100, None),
            bounced(2, 90),
            bounced(2, 80),
            merged(2, 70, Some("fix: two")),
            bounced(3, 65),
            merged(3, 60, Some("feat: three")),
            merged(4, 50, Some("feat: four")),
            merged(5, 40, Some("feat: five")),
            merged(6, 30, Some("feat: six")),
            bounced(7, 20),
        ];
        let (_, _, recent) = summarize(&events, &now);
        let prs: Vec<u64> = recent.iter().map(|m| m.pr.0).collect();
        assert_eq!(prs, [6, 5, 4, 3, 2], "the oldest merge falls off");
        assert_eq!(recent[0].title.as_deref(), Some("feat: six"));
        assert_eq!(recent[0].batch, BatchId("b6".into()));
        assert_eq!(recent[0].merged_ms, at(&now, 30));
        let bounces: Vec<u32> = recent.iter().map(|m| m.bounces).collect();
        assert_eq!(
            bounces,
            [0, 0, 0, 1, 2],
            "bounces of #7 (not merged) count for no one"
        );

        let (_, _, all) = summarize(&events[..2], &now);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, None, "an old event has no title");
        assert_eq!(
            serde_json::to_value(&all[0]).unwrap()["title"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn recent_merged_carries_the_submit_time_of_the_merged_event() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        let events = vec![Event {
            at_ms: at(&now, 10),
            kind: EventKind::Merged {
                pr: PrNumber(1),
                batch: BatchId("b".into()),
                commit: Sha::try_from("abc1234".to_owned()).unwrap(),
                submitted_ms: at(&now, 25),
                title: None,
            },
        }];
        let (_, _, merged) = summarize(&events, &now);
        assert_eq!(merged[0].submitted_ms, Some(at(&now, 25)));
        assert_eq!(
            serde_json::to_value(&merged[0]).unwrap()["submitted_ms"],
            serde_json::json!(at(&now, 25).0)
        );
    }

    fn queue_item(pr: u64, state: ItemState) -> QueueItem {
        QueueItem {
            pr: PrNumber(pr),
            title: "t".into(),
            branch: format!("b{pr}"),
            head: Sha::try_from("abc1234".to_owned()).unwrap(),
            state,
            agent: None,
            attempts: 0,
            submitted_ms: EpochMs(1),
            touches_config: false,
            last_failure: None,
            submitter: None,
            priority: false,
            after: Vec::new(),
            repair: false,
            config_change: None,
        }
    }

    #[test]
    fn a_bounced_item_shows_its_latest_bounce_even_after_several() {
        let bounce = |pr, ms| Event {
            at_ms: EpochMs(ms),
            kind: EventKind::Bounced {
                pr: PrNumber(pr),
                batch: None,
                reason: BounceReason::Conflict,
                attempts: 1,
                unreachable: None,
            },
        };
        let events = vec![bounce(1, 100), bounce(2, 150), bounce(1, 300)];
        let last = last_bounced_at(&events);
        let bounced = queue_item(1, ItemState::Bounced(BounceReason::HeadChanged));
        assert_eq!(bounced_ms(&bounced, &last), Some(EpochMs(300)));
    }

    #[test]
    fn only_a_bounced_item_has_a_bounce_time() {
        let events = vec![Event {
            at_ms: EpochMs(300),
            kind: EventKind::Bounced {
                pr: PrNumber(1),
                batch: None,
                reason: BounceReason::Conflict,
                attempts: 1,
                unreachable: None,
            },
        }];
        let last = last_bounced_at(&events);
        let queued = queue_item(1, ItemState::Queued(None));
        assert_eq!(bounced_ms(&queued, &last), None);
    }

    #[test]
    fn a_bounced_item_without_a_bounce_event_has_no_bounce_time() {
        let last = last_bounced_at(&[]);
        let bounced = queue_item(1, ItemState::Bounced(BounceReason::Verify));
        assert_eq!(bounced_ms(&bounced, &last), None);
    }

    #[test]
    fn median_of_an_even_count_and_flaky_count() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        let sha = Sha::try_from("abc1234".to_owned()).unwrap();
        let mut events: Vec<Event> = [100u64, 400]
            .into_iter()
            .map(|lead_s| Event {
                at_ms: at(&now, 1),
                kind: EventKind::Merged {
                    pr: PrNumber(1),
                    batch: BatchId("b".into()),
                    commit: sha.clone(),
                    submitted_ms: EpochMs(at(&now, 1).0 - lead_s * 1000),
                    title: None,
                },
            })
            .collect();
        events.push(Event {
            at_ms: at(&now, 1),
            kind: EventKind::Flaky {
                batch: BatchId("b".into()),
                step: "test".into(),
                tests: vec!["a".into()],
                tree: sha.clone(),
            },
        });
        let (today, _, _) = summarize(&events, &now);
        assert_eq!(today.median_lead_s, Some(250));
        assert_eq!(today.flaky, 1);
    }

    #[test]
    fn the_usual_median_of_huge_values_does_not_overflow() {
        let green = |seconds| Event {
            at_ms: EpochMs(1),
            kind: EventKind::PostGreen {
                commit: Sha::try_from("abc1234".to_owned()).unwrap(),
                previous: None,
                seconds,
            },
        };
        let events: Vec<Event> = [u64::MAX, u64::MAX, 1, 2].map(green).into();

        assert_eq!(usual(&events, None).post_verify_s, Some(1 << 63));
    }

    #[test]
    fn the_median_of_two_huge_values_is_their_floored_mean() {
        assert_eq!(median(vec![u64::MAX, u64::MAX - 2]), Some(u64::MAX - 1));
        assert_eq!(median(vec![1, 2]), Some(1));
    }

    #[test]
    fn a_batch_waiting_for_a_pc_slot_says_so_in_the_text_and_the_json() {
        let fixture = include_str!("../../tests/fixtures/status-full.json");
        let mut view: StatusView = serde_json::from_str(fixture).unwrap();
        let batch = view.batch.as_mut().unwrap();
        assert!(!batch.waiting_machine);
        assert!(!human(&view).contains("PC 자리를 기다리는 중"));
        view.batch.as_mut().unwrap().waiting_machine = true;
        assert!(human(&view).contains("PC 자리를 기다리는 중"));
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["batch"]["waiting_machine"], true);
    }

    #[test]
    fn a_batch_that_took_over_a_pass_says_whose_in_the_text_and_the_json() {
        let fixture = include_str!("../../tests/fixtures/status-full.json");
        let mut view: StatusView = serde_json::from_str(fixture).unwrap();
        assert!(human(&view).contains("preflight의 통과 결과를 다시 씀"));
        assert_eq!(
            serde_json::to_value(&view).unwrap()["batch"]["reused"],
            "preflight"
        );
        view.batch.as_mut().unwrap().reused = Some(VerifiedBy::Batch);
        assert!(human(&view).contains("앞선 batch의 통과 결과를 다시 씀"));
        view.batch.as_mut().unwrap().reused = None;
        assert!(!human(&view).contains("통과 결과를 다시 씀"));
    }

    #[test]
    fn no_merges_means_no_median() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        assert_eq!(summarize(&[], &now).0.median_lead_s, None);
    }
}
