use std::process::ExitCode;

use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};

use super::Ctx;
use crate::config::{self, Config};
use crate::model::{
    Approval, DaemonView, EpochMs, Event, EventKind, ItemView, RecentBatch, RepoInfo, RepoView,
    SchemaV1, StatusView, Today, Warning, WarningKind,
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

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let now = Local::now();
    let views = if args.all {
        ctx.home
            .repos()?
            .iter()
            .map(|info| view(ctx, info, &now))
            .collect::<Result<Vec<_>>>()?
    } else {
        let (info, _) = ctx.registered_repo()?;
        vec![view(ctx, &info, &now)?]
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

pub fn view(ctx: &Ctx, info: &RepoInfo, now: &DateTime<Local>) -> Result<StatusView> {
    let store = ctx.home.repo(&info.slug);
    let state = store.load()?;
    let mut warnings = state.warnings.clone();
    let loaded = Config::load_remote_branch(&ctx.ext, &info.path, &info.default_branch);
    let (approval, budget) = match loaded {
        Ok(Some(c)) => (c.approval, c.budget),
        failed => {
            let message = match failed {
                Ok(_) => format!(
                    "설정을 읽지 못했다: chute.toml이 origin/{}에 없다. 기본 브랜치에 올렸는지 확인하고 `git fetch`를 먼저 실행한다",
                    info.default_branch
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
            .map(|item| ItemView::new(item, budget, state.preflights.get(&item.branch)))
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
            | EventKind::Blocked { .. } => {}
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
    if let Some(b) = &v.blocked {
        line(format!(
            "큐 멈춤: 기준 커밋 {}이 검증을 통과하지 못한다",
            b.base
        ));
    }
    if let Some(b) = &v.batch {
        let prs: Vec<String> = b.prs.iter().map(|p| p.to_string()).collect();
        line(format!(
            "batch {} {:?} {} · {}회차 · {}",
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
    use crate::model::{BatchId, BounceReason, PrNumber, Sha};
    use chrono::Utc;

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
    fn no_merges_means_no_median() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        assert_eq!(summarize(&[], &now).0.median_lead_s, None);
    }
}
