use std::process::ExitCode;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, TimeZone};

use super::Ctx;
use crate::config::{self, Config};
use crate::model::{
    Approval, DaemonView, EpochMs, Event, EventKind, ItemState, ItemView, RecentBatch, RepoInfo,
    RepoView, SchemaV1, StatusView, Today, Warning, WarningKind,
};
use crate::repo;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 플랜 5.5의 상태 JSON을 출력한다
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
        let slug = repo::slug(&ctx.ext, &ctx.target_dir()?)?;
        let info = ctx.home.repo(&slug).info()?.ok_or_else(|| {
            anyhow!("chute에 등록되지 않은 저장소다: {slug}. 저장소에서 `chute init`을 실행한다")
        })?;
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

/// Builds the status JSON from the state folder. The approval mode and budget come from
/// `chute.toml` on the remote default branch; when that cannot be read, defaults apply and a warning says so.
pub fn view(ctx: &Ctx, info: &RepoInfo, now: &DateTime<Local>) -> Result<StatusView> {
    let store = ctx.home.repo(&info.slug);
    let state = store.load()?;
    let mut warnings = state.warnings.clone();
    let rev = format!("origin/{}", info.default_branch);
    let (approval, budget) = match Config::load_at(&ctx.ext, &info.path, &rev) {
        Ok(c) => (c.approval, c.budget),
        Err(e) => {
            warnings.push(Warning {
                kind: WarningKind::Config,
                message: format!("{e:#}"),
                pr: None,
                at_ms: EpochMs(now.timestamp_millis().max(0) as u64),
            });
            (Approval::default(), config::DEFAULT_BUDGET)
        }
    };
    let running = store.daemon_running()?;
    let daemon = if running { store.daemon()? } else { None };
    let (today, recent) = summarize(&store.history()?, now);
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

/// Today's numbers by the calendar day of `now`, and the latest finished batches, newest first.
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
            EventKind::Reverted { .. } => {}
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
            state_label(item.state),
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

fn state_label(state: ItemState) -> String {
    use crate::model::{BounceReason as B, HumanReason as H, QueuedReason as Q};
    match state {
        ItemState::AwaitingApproval => "승인 대기".into(),
        ItemState::Queued(None) => "대기".into(),
        ItemState::Queued(Some(Q::CiPending)) => "대기(CI 기다림)".into(),
        ItemState::InBatch => "검증 중".into(),
        ItemState::Bounced(r) => format!(
            "반송({})",
            match r {
                B::Conflict => "충돌",
                B::Verify => "검증 실패",
                B::Timeout => "시간 초과",
                B::HeadChanged => "HEAD 바뀜",
                B::Ci => "CI 실패",
            }
        ),
        ItemState::NeedsHuman(r) => format!(
            "사람 차례({})",
            match r {
                H::Budget => "예산 소진",
                H::Unreachable => "전달 실패",
                H::GaveUp => "포기",
            }
        ),
    }
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
    fn no_merges_means_no_median() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        assert_eq!(summarize(&[], &now).0.median_lead_s, None);
    }
}
