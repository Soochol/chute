use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};

use super::Ctx;
use crate::config::{self, Config};
use crate::hub::{self, Route};
use crate::machine::Machines;
use crate::model::{
    Agent, AgentView, Approval, DaemonView, EpochMs, Event, EventKind, HubView, ItemView,
    MachineId, RecentBatch, RepoInfo, RepoView, SchemaV1, StatusView, TerminalId, Today, Warning,
    WarningKind,
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
        },
        blocked: None,
        batch: None,
        queue: Vec::new(),
        today: Today::default(),
        recent: Vec::new(),
        warnings: vec![hub_warning(message, now)],
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
    let (today, recent) = summarize(&history.events, now);
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
        daemon: DaemonView {
            running,
            terminal: daemon.as_ref().and_then(|d| d.terminal.clone()),
            pid: daemon.as_ref().map(|d| d.pid),
        },
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
                )
            })
            .collect(),
        today,
        recent,
        warnings,
    })
}

pub fn summarize<Tz: TimeZone>(events: &[Event], now: &DateTime<Tz>) -> (Today, Vec<RecentBatch>) {
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
    for event in events {
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
            | EventKind::Blocked { .. }
            | EventKind::Submitted { .. }
            | EventKind::Dropped { .. }
            | EventKind::Notified { .. }
            | EventKind::Resumed { .. }
            | EventKind::FlakyProbe { .. } => {}
        }
    }
    leads.sort_unstable();
    today.median_lead_s = match leads.len() {
        0 => None,
        n if n % 2 == 1 => Some(leads[n / 2]),
        n => Some((leads[n / 2 - 1] + leads[n / 2]) / 2),
    };
    recent.reverse();
    recent.truncate(RECENT_BATCHES);
    (today, recent)
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
        line(format!(
            "batch {} {:?} {} · {}회차 · {}{waiting}",
            b.id.0,
            b.phase,
            b.step.as_deref().unwrap_or("-"),
            b.round,
            prs.join(" ")
        ));
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
    use crate::model::{BatchId, BounceReason, PrNumber, Sha};
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
                },
            },
            finished("first", 9),
            finished("second", 1),
        ];
        let (today, recent) = summarize(&events, &now);
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
        let (today, _) = summarize(&events, &now);
        assert_eq!(today.median_lead_s, Some(250));
        assert_eq!(today.flaky, 1);
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
    fn no_merges_means_no_median() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        assert_eq!(summarize(&[], &now).0.median_lead_s, None);
    }
}
