use std::process::ExitCode;

use anyhow::{Result, bail};
use chrono::{DateTime, Local, TimeZone};

use super::batches::short;
use super::{Ctx, run_on_hub};
use crate::model::{
    BounceReason, Delivered, EpochMs, Event, EventKind, HumanReason, NotifyKind, PrNumber,
    ProbeEvidence, RemovedReason, ResumedBy, VerifiedBy,
};

const DEFAULT_LIMIT: u64 = 500;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 이 시각 이후(같은 시각 포함)의 사건만 낸다. epoch 밀리초, RFC 3339 시각, 상대 시간(30m, 2h, 1d)
    #[arg(long, value_name = "시각")]
    pub since: Option<String>,
    /// 이 PR이 든 사건만 낸다
    #[arg(long, value_name = "PR")]
    pub pr: Option<PrNumber>,
    /// 낼 사건 수의 상한. 넘으면 가장 최근 것만 남긴다
    #[arg(long, default_value_t = DEFAULT_LIMIT, value_parser = clap::value_parser!(u64).range(1..))]
    pub limit: u64,
    /// 사건을 history 한 줄과 같은 JSON으로 한 줄에 하나씩 낸다
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let since = args
        .since
        .as_deref()
        .map(|text| parse_since(text, EpochMs::now()))
        .transpose()?;
    let mut forwarded = vec![
        "events".to_owned(),
        "--limit".into(),
        args.limit.to_string(),
    ];
    if let Some(since) = since {
        forwarded.extend(["--since".into(), since.0.to_string()]);
    }
    if let Some(pr) = args.pr {
        forwarded.extend(["--pr".into(), pr.0.to_string()]);
    }
    if args.json {
        forwarded.push("--json".into());
    }
    let forwarded: Vec<&str> = forwarded.iter().map(String::as_str).collect();
    if let Some(code) = run_on_hub(ctx, &forwarded)? {
        return Ok(code);
    }
    let (_, store) = ctx.registered_repo()?;
    let history = store.history()?;
    if !history.broken_lines.is_empty() {
        eprintln!(
            "경고: history에서 읽지 못한 줄이 {}개 있어 건너뛰었다",
            history.broken_lines.len()
        );
    }
    let limit = usize::try_from(args.limit).unwrap_or(usize::MAX);
    let picked = select(&history.events, since, args.pr, limit);
    if picked.cut > 0 {
        eprintln!(
            "경고: 조건에 맞는 사건 {}개 가운데 가장 최근 {}개만 냈다. --limit으로 늘리거나 --since로 좁힌다",
            picked.events.len() + picked.cut,
            picked.events.len()
        );
    }
    for event in picked.events {
        if args.json {
            println!("{}", serde_json::to_string(event)?);
        } else {
            println!("{}", render(event, &Local));
        }
    }
    Ok(ExitCode::SUCCESS)
}

struct Selection<'a> {
    events: Vec<&'a Event>,
    cut: usize,
}

fn select(
    events: &[Event],
    since: Option<EpochMs>,
    pr: Option<PrNumber>,
    limit: usize,
) -> Selection<'_> {
    let mut picked: Vec<&Event> = events
        .iter()
        .filter(|e| since.is_none_or(|since| e.at_ms >= since))
        .filter(|e| pr.is_none_or(|pr| involves(&e.kind, pr)))
        .collect();
    picked.sort_by_key(|e| e.at_ms);
    let cut = picked.len().saturating_sub(limit);
    picked.drain(..cut);
    Selection {
        events: picked,
        cut,
    }
}

fn involves(kind: &EventKind, target: PrNumber) -> bool {
    match kind {
        EventKind::Merged { pr, .. }
        | EventKind::Bounced { pr, .. }
        | EventKind::GaveUp { pr, .. }
        | EventKind::Closed { pr }
        | EventKind::NeedsHuman { pr, .. }
        | EventKind::Removed { pr, .. }
        | EventKind::Submitted { pr, .. }
        | EventKind::Dropped { pr } => *pr == target,
        EventKind::Notified { pr, .. } | EventKind::Resumed { pr, .. } => *pr == Some(target),
        EventKind::BatchFinished {
            merged, bounced, ..
        } => merged.contains(&target) || bounced.contains(&target),
        EventKind::Reverted { prs, .. } => prs.contains(&target),
        EventKind::Flaky { .. }
        | EventKind::Blocked { .. }
        | EventKind::FlakyProbe { .. }
        | EventKind::Reused { .. }
        | EventKind::PostGreen { .. } => false,
        EventKind::PostFailed { culprit, .. } => {
            culprit.as_ref().and_then(|c| c.pr) == Some(target)
        }
        EventKind::PostFixed { pr, .. } => *pr == Some(target),
    }
}

fn parse_since(text: &str, now: EpochMs) -> Result<EpochMs> {
    const HINT: &str = "epoch 밀리초(1763270000123), RFC 3339 시각(2026-11-16T14:10:25+09:00), 상대 시간(30m, 2h, 1d) 가운데 하나여야 한다";
    let text = text.trim();
    if text.is_empty() {
        bail!("--since 값이 비었다. {HINT}");
    }
    if let Ok(ms) = text.parse::<u64>() {
        return Ok(EpochMs(ms));
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        bail!("--since `{text}`가 epoch 밀리초로 쓸 수 있는 범위를 넘는다");
    }
    if let Some((digits, unit)) = text.split_at_checked(text.len() - 1)
        && let Ok(count) = digits.parse::<u64>()
    {
        let unit_ms: u64 = match unit {
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            _ => bail!("--since의 상대 시간 단위 `{unit}`을 알지 못한다. m, h, d 중 하나를 쓴다"),
        };
        return Ok(EpochMs(now.0.saturating_sub(count.saturating_mul(unit_ms))));
    }
    match DateTime::parse_from_rfc3339(text) {
        Ok(at) => match u64::try_from(at.timestamp_millis()) {
            Ok(ms) => Ok(EpochMs(ms)),
            Err(_) => bail!("--since `{text}`가 1970년보다 앞선다"),
        },
        Err(_) => bail!("--since `{text}`를 읽지 못했다. {HINT}"),
    }
}

fn render<Tz: TimeZone>(event: &Event, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let at = i64::try_from(event.at_ms.0)
        .ok()
        .and_then(|ms| tz.timestamp_millis_opt(ms).single())
        .map_or_else(
            || format!("at={}", event.at_ms.0),
            |t| t.format("%Y-%m-%d %H:%M:%S").to_string(),
        );
    format!("{at}  {}", describe(&event.kind))
}

fn describe(kind: &EventKind) -> String {
    match kind {
        EventKind::Merged {
            pr, batch, commit, ..
        } => format!("{pr} 머지됨 · batch {} · 커밋 {}", batch.0, short(commit)),
        EventKind::Bounced {
            pr,
            batch,
            reason,
            attempts,
            unreachable,
        } => {
            let batch = batch
                .as_ref()
                .map_or(String::new(), |b| format!(" · batch {}", b.0));
            let unreachable = unreachable.as_deref().map_or(String::new(), |why| {
                format!(" · 작업자에게 닿지 않아 submit한 터미널로 보냄: {why}")
            });
            format!(
                "{pr} 반송됨 · 이유 {} · {attempts}번째{batch}{unreachable}",
                bounce_reason(*reason)
            )
        }
        EventKind::Flaky {
            batch, step, tests, ..
        } => format!(
            "흔들린 시험 · batch {} · 단계 {step} · 시험 {}",
            batch.0,
            names(tests)
        ),
        EventKind::BatchFinished {
            batch,
            merged,
            bounced,
            seconds,
            verify_s,
        } => format!(
            "batch {} 끝 · 머지 {} · 반송 {} · {seconds}초(검증 {verify_s}초)",
            batch.0,
            prs(merged),
            prs(bounced)
        ),
        EventKind::Reverted {
            batch,
            prs: list,
            commit,
        } => format!(
            "batch {} 되돌림 · {} · 커밋 {}",
            batch.0,
            prs(list),
            short(commit)
        ),
        EventKind::Reused {
            batch,
            tree,
            by,
            verified_ms,
        } => format!(
            "검증 생략 · batch {} · 트리 {} · {} 검증의 통과 결과를 다시 썼다(검증 시각 epoch {}ms)",
            batch.0,
            short(tree),
            verified_by(*by),
            verified_ms.0
        ),
        EventKind::GaveUp { pr, reason } => format!("{pr} 포기됨 · {reason}"),
        EventKind::Closed { pr } => format!("{pr} 닫힘"),
        EventKind::NeedsHuman { pr, reason, detail } => {
            let detail = detail
                .as_deref()
                .map_or(String::new(), |d| format!(" · {d}"));
            format!("{pr} 사람 차례 · 이유 {}{detail}", human_reason(*reason))
        }
        EventKind::Removed {
            pr,
            reason,
            detail,
            report,
        } => {
            let detail = detail
                .as_deref()
                .map_or(String::new(), |d| format!(" · {d}"));
            let report = report
                .as_ref()
                .map_or(String::new(), |r| format!(" · 보고서 {}", r.display()));
            format!(
                "{pr} 큐에서 빠짐 · 이유 {}{detail}{report}",
                removed_reason(*reason)
            )
        }
        EventKind::Blocked { base, .. } => {
            format!("큐 멈춤 · 기준 {}이 깨졌다", short(base))
        }
        EventKind::Submitted {
            pr,
            head,
            priority,
            after,
            repair,
        } => {
            let mut marks = String::new();
            if *priority {
                marks.push_str(" · 우선");
            }
            if !after.is_empty() {
                marks.push_str(&format!(" · {} 뒤", prs(after)));
            }
            if *repair {
                marks.push_str(" · 복구");
            }
            format!("{pr} 큐에 올림 · HEAD {}{marks}", short(head))
        }
        EventKind::Dropped { pr } => format!("{pr} 큐에서 뺌"),
        EventKind::Notified {
            kind,
            pr,
            to,
            delivered,
        } => {
            let pr = pr.map_or(String::new(), |p| format!(" {p}"));
            let to = to.as_ref().map_or(String::new(), |t| format!(" → {}", t.0));
            let lost = if matches!(delivered, Delivered::Sent | Delivered::SameAsAgent) {
                ""
            } else {
                " (놓친 알림)"
            };
            format!(
                "알림 {}{pr}{to} · {}{lost}",
                notify_kind(*kind),
                delivered_label(*delivered)
            )
        }
        EventKind::Resumed { base, by, pr } => {
            let pr = pr.map_or(String::new(), |p| format!(" · {p}"));
            format!(
                "큐 재개 · 새 기준 {} · {}{pr}",
                short(base),
                resumed_by(*by)
            )
        }
        EventKind::FlakyProbe {
            batch,
            step,
            tests,
            evidence,
            runs,
            failures,
        } => format!(
            "흔들림 실험 · batch {} · 단계 {step} · 시험 {} · {runs}번 중 {failures}번 실패 · {}",
            batch.0,
            names(tests),
            probe_evidence(*evidence)
        ),
        EventKind::PostGreen {
            commit,
            previous,
            seconds,
        } => {
            let previous = previous
                .as_ref()
                .map_or(String::new(), |p| format!(" · 이전 통과 {}", short(p)));
            format!(
                "머지 뒤 검사 통과 · 커밋 {}{previous} · {seconds}초",
                short(commit)
            )
        }
        EventKind::PostFailed {
            commit,
            step,
            tests,
            last_green,
            culprit,
        } => {
            let green = last_green
                .as_ref()
                .map_or(" · 마지막 통과 없음".to_owned(), |g| {
                    format!(" · 마지막 통과 {}", short(g))
                });
            let culprit = match culprit {
                Some(c) => match c.pr {
                    Some(pr) => format!(" · 범인 {pr}"),
                    None => format!(" · 범인 커밋 {}(chute가 머지하지 않았다)", short(&c.commit)),
                },
                None => " · 범인을 찾지 못했다".to_owned(),
            };
            format!(
                "머지 뒤 검사 실패 · 커밋 {} · 단계 {step} · 시험 {}{green}{culprit}",
                short(commit),
                names(tests)
            )
        }
        EventKind::PostFixed { commit, was, pr } => {
            let pr = pr.map_or(String::new(), |p| format!(" · 범인이던 {p}"));
            format!(
                "머지 뒤 검사 풀림 · 커밋 {} · 실패했던 {}{pr}",
                short(commit),
                short(was)
            )
        }
    }
}

fn prs(list: &[PrNumber]) -> String {
    if list.is_empty() {
        return "-".to_owned();
    }
    list.iter()
        .map(PrNumber::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

fn names(list: &[String]) -> String {
    if list.is_empty() {
        "-".to_owned()
    } else {
        list.join(", ")
    }
}

fn verified_by(by: VerifiedBy) -> &'static str {
    match by {
        VerifiedBy::Preflight => "preflight",
        VerifiedBy::Batch => "batch",
    }
}

fn bounce_reason(reason: BounceReason) -> &'static str {
    match reason {
        BounceReason::Conflict => "충돌",
        BounceReason::Verify => "검증 실패",
        BounceReason::Timeout => "시간 초과",
        BounceReason::HeadChanged => "HEAD가 바뀜",
        BounceReason::Ci => "CI 실패",
    }
}

fn human_reason(reason: HumanReason) -> &'static str {
    match reason {
        HumanReason::Budget => "예산 소진",
        HumanReason::Unreachable => "작업자에게 닿지 않음",
        HumanReason::GaveUp => "작업자가 포기",
    }
}

fn removed_reason(reason: RemovedReason) -> &'static str {
    match reason {
        RemovedReason::Budget => "반송 한도 초과",
        RemovedReason::GaveUp => "작업자가 포기",
        RemovedReason::Upgrade => "옛 사람 차례 항목 정리",
    }
}

fn notify_kind(kind: NotifyKind) -> &'static str {
    match kind {
        NotifyKind::Merged => "머지",
        NotifyKind::Bounced => "반송",
        NotifyKind::Human => "사람 차례",
        NotifyKind::Removed => "큐에서 빠짐",
        NotifyKind::Halted => "멈춤",
        NotifyKind::Resumed => "재개",
        NotifyKind::Reverted => "되돌림",
        NotifyKind::Waiting => "대기",
        NotifyKind::Flaky => "흔들림",
        NotifyKind::PostFailed => "머지 뒤 검사 실패",
        NotifyKind::PostFixed => "머지 뒤 검사 풀림",
    }
}

fn delivered_label(delivered: Delivered) -> &'static str {
    match delivered {
        Delivered::Sent => "보냄",
        Delivered::SameAsAgent => "작업자와 같아 한 번만 보냄",
        Delivered::NoTerminal => "받을 터미널이 없음",
        Delivered::Gone => "터미널이 사라짐",
        Delivered::Blocked => "막힘",
        Delivered::UnknownMachine => "모르는 머신",
        Delivered::Failed => "보내지 못함",
    }
}

fn resumed_by(by: ResumedBy) -> &'static str {
    match by {
        ResumedBy::Repair => "복구 PR로",
        ResumedBy::MainMoved => "main이 움직여서",
    }
}

pub(super) fn probe_evidence(evidence: ProbeEvidence) -> &'static str {
    match evidence {
        ProbeEvidence::Base => "기준 트리에서도 흔들림",
        ProbeEvidence::Made => "흔들림을 재현함",
        ProbeEvidence::Unresolved => "원인 미확인",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::{BatchId, BlockReason, Sha, TerminalId};

    const NOW: EpochMs = EpochMs(1_763_270_000_000);

    fn sha(hex: &str) -> Sha {
        Sha::try_from(hex.to_owned()).unwrap()
    }

    fn closed(at: u64, pr: u64) -> Event {
        Event {
            at_ms: EpochMs(at),
            kind: EventKind::Closed { pr: PrNumber(pr) },
        }
    }

    fn since(text: &str) -> Result<EpochMs> {
        parse_since(text, NOW)
    }

    #[test]
    fn since_takes_epoch_millis_rfc3339_and_relative_times() {
        assert_eq!(since("1763270000123").unwrap(), EpochMs(1_763_270_000_123));
        assert_eq!(
            since("2023-11-14T22:13:20+00:00").unwrap(),
            EpochMs(1_700_000_000_000)
        );
        assert_eq!(
            since("2023-11-15T07:13:20+09:00").unwrap(),
            EpochMs(1_700_000_000_000)
        );
        assert_eq!(since("30m").unwrap(), EpochMs(NOW.0 - 30 * 60_000));
        assert_eq!(since("2h").unwrap(), EpochMs(NOW.0 - 2 * 3_600_000));
        assert_eq!(since("1d").unwrap(), EpochMs(NOW.0 - 86_400_000));
    }

    #[test]
    fn a_relative_time_reaching_before_1970_starts_at_zero() {
        assert_eq!(since("99999999999d").unwrap(), EpochMs(0));
    }

    #[test]
    fn a_bad_since_says_why() {
        for bad in [
            "",
            "  ",
            "yesterday",
            "2x",
            "m",
            "-5m",
            "1.5h",
            "2026-13-40",
        ] {
            let message = since(bad).unwrap_err().to_string();
            assert!(message.contains("--since"), "{bad:?}: {message}");
            assert!(!message.contains("input"), "{bad:?}: {message}");
        }
        assert!(since("5w").unwrap_err().to_string().contains("m, h, d"));
        assert!(
            since("1969-12-31T00:00:00Z")
                .unwrap_err()
                .to_string()
                .contains("1970")
        );
    }

    #[test]
    fn a_number_too_big_for_epoch_millis_is_called_out_of_range() {
        for big in ["99999999999999999999", "18446744073709551616"] {
            let message = since(big).unwrap_err().to_string();
            assert!(message.contains("범위"), "{big}: {message}");
            assert!(!message.contains("단위"), "{big}: {message}");
        }
    }

    #[test]
    fn a_time_beyond_the_calendar_prints_the_raw_millis() {
        for at in [u64::MAX, 1 << 63] {
            let event = Event {
                at_ms: EpochMs(at),
                kind: EventKind::Closed { pr: PrNumber(1) },
            };
            assert_eq!(render(&event, &Utc), format!("at={at}  #1 닫힘"));
        }
    }

    #[test]
    fn a_multibyte_since_is_an_error_and_not_a_panic() {
        assert!(since("30분").is_err());
        assert!(since("분").is_err());
    }

    #[test]
    fn since_includes_events_at_the_same_instant() {
        let events = [
            closed(99, 1),
            closed(100, 2),
            closed(100, 3),
            closed(101, 4),
        ];
        let picked = select(&events, Some(EpochMs(100)), None, 10);
        let prs: Vec<u64> = picked
            .events
            .iter()
            .map(|e| match &e.kind {
                EventKind::Closed { pr } => pr.0,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(prs, [2, 3, 4]);
    }

    #[test]
    fn events_come_out_in_time_order_even_if_written_out_of_order() {
        let events = [closed(30, 3), closed(10, 1), closed(20, 2)];
        let picked = select(&events, None, None, 10);
        let ats: Vec<u64> = picked.events.iter().map(|e| e.at_ms.0).collect();
        assert_eq!(ats, [10, 20, 30]);
    }

    #[test]
    fn limit_keeps_the_most_recent_events_and_counts_the_cut() {
        let events: Vec<Event> = (1..=5).map(|i| closed(i * 10, i)).collect();
        let picked = select(&events, None, None, 2);
        let ats: Vec<u64> = picked.events.iter().map(|e| e.at_ms.0).collect();
        assert_eq!(ats, [40, 50]);
        assert_eq!(picked.cut, 3);
        assert_eq!(select(&events, None, None, 5).cut, 0);
    }

    #[test]
    fn pr_keeps_events_that_name_the_pr_in_a_field_or_a_list() {
        let batch = BatchId("b1".into());
        let events = [
            closed(1, 7),
            closed(2, 8),
            Event {
                at_ms: EpochMs(3),
                kind: EventKind::BatchFinished {
                    batch: batch.clone(),
                    merged: vec![PrNumber(7)],
                    bounced: vec![PrNumber(9)],
                    seconds: 1,
                    verify_s: 1,
                },
            },
            Event {
                at_ms: EpochMs(4),
                kind: EventKind::Reverted {
                    batch: batch.clone(),
                    prs: vec![PrNumber(7), PrNumber(8)],
                    commit: sha("abcd"),
                },
            },
            Event {
                at_ms: EpochMs(5),
                kind: EventKind::Notified {
                    kind: NotifyKind::Merged,
                    pr: Some(PrNumber(7)),
                    to: None,
                    delivered: Delivered::NoTerminal,
                },
            },
            Event {
                at_ms: EpochMs(6),
                kind: EventKind::Blocked {
                    reason: BlockReason::BaseBroken,
                    base: sha("abcd"),
                },
            },
        ];
        let ats = |pr| -> Vec<u64> {
            select(&events, None, Some(PrNumber(pr)), 10)
                .events
                .iter()
                .map(|e| e.at_ms.0)
                .collect()
        };
        assert_eq!(ats(7), [1, 3, 4, 5]);
        assert_eq!(ats(9), [3]);
        assert_eq!(ats(8), [2, 4]);
        assert!(ats(10).is_empty());
    }

    #[test]
    fn pr_keeps_the_check_after_the_merge_events_that_name_the_culprit() {
        let culprit = |pr| crate::model::Culprit {
            commit: sha("abcd"),
            pr,
            batch: None,
        };
        let failed = EventKind::PostFailed {
            commit: sha("9f8e7d6a5b4c"),
            step: "e2e".into(),
            tests: vec![],
            last_green: None,
            culprit: Some(culprit(Some(PrNumber(7)))),
        };
        let unknown = EventKind::PostFailed {
            commit: sha("9f8e7d6a5b4c"),
            step: "e2e".into(),
            tests: vec![],
            last_green: None,
            culprit: Some(culprit(None)),
        };
        let fixed = EventKind::PostFixed {
            commit: sha("9f8e7d6a5b4c"),
            was: sha("abcd"),
            pr: Some(PrNumber(7)),
        };
        let green = EventKind::PostGreen {
            commit: sha("9f8e7d6a5b4c"),
            previous: None,
            seconds: 1,
        };
        assert!(involves(&failed, PrNumber(7)) && involves(&fixed, PrNumber(7)));
        assert!(!involves(&failed, PrNumber(8)) && !involves(&unknown, PrNumber(7)));
        assert!(!involves(&green, PrNumber(7)));
    }

    #[test]
    fn every_kind_of_event_has_a_one_line_korean_description() {
        let batch = BatchId("b1".into());
        let kinds = [
            EventKind::Merged {
                pr: PrNumber(1),
                batch: batch.clone(),
                commit: sha("9f8e7d6a5b4c"),
                submitted_ms: EpochMs(1),
                title: None,
            },
            EventKind::Bounced {
                pr: PrNumber(1),
                batch: None,
                reason: BounceReason::Conflict,
                attempts: 2,
                unreachable: None,
            },
            EventKind::Flaky {
                batch: batch.clone(),
                step: "test".into(),
                tests: vec!["a".into(), "b".into()],
                tree: sha("abcd"),
            },
            EventKind::BatchFinished {
                batch: batch.clone(),
                merged: vec![PrNumber(1)],
                bounced: vec![],
                seconds: 9,
                verify_s: 4,
            },
            EventKind::Reverted {
                batch: batch.clone(),
                prs: vec![PrNumber(1)],
                commit: sha("abcd"),
            },
            EventKind::GaveUp {
                pr: PrNumber(1),
                reason: "막힘".into(),
            },
            EventKind::Closed { pr: PrNumber(1) },
            EventKind::NeedsHuman {
                pr: PrNumber(1),
                reason: HumanReason::Budget,
                detail: Some("세 번 반송".into()),
            },
            EventKind::Blocked {
                reason: BlockReason::BaseBroken,
                base: sha("abcd"),
            },
            EventKind::Submitted {
                pr: PrNumber(1),
                head: sha("abcd"),
                priority: true,
                after: vec![PrNumber(2)],
                repair: true,
            },
            EventKind::Dropped { pr: PrNumber(1) },
            EventKind::Notified {
                kind: NotifyKind::Bounced,
                pr: Some(PrNumber(1)),
                to: Some(TerminalId("t1".into())),
                delivered: Delivered::Sent,
            },
            EventKind::Resumed {
                base: sha("abcd"),
                by: ResumedBy::Repair,
                pr: Some(PrNumber(1)),
            },
            EventKind::FlakyProbe {
                batch,
                step: "test".into(),
                tests: vec![],
                evidence: ProbeEvidence::Made,
                runs: 5,
                failures: 2,
            },
            EventKind::PostGreen {
                commit: sha("9f8e7d6a5b4c"),
                previous: Some(sha("abcd")),
                seconds: 480,
            },
            EventKind::PostFailed {
                commit: sha("9f8e7d6a5b4c"),
                step: "e2e".into(),
                tests: vec!["login".into()],
                last_green: None,
                culprit: Some(crate::model::Culprit {
                    commit: sha("abcd"),
                    pr: None,
                    batch: None,
                }),
            },
            EventKind::PostFixed {
                commit: sha("9f8e7d6a5b4c"),
                was: sha("abcd"),
                pr: Some(PrNumber(1)),
            },
        ];
        for kind in kinds {
            let line = describe(&kind);
            assert!(!line.is_empty() && !line.contains('\n'), "{kind:?}");
            assert!(line.chars().any(|c| ('가'..='힣').contains(&c)), "{line}");
        }
    }

    #[test]
    fn a_line_starts_with_the_time_and_says_what_happened() {
        let event = Event {
            at_ms: EpochMs(1_700_000_000_000),
            kind: EventKind::Merged {
                pr: PrNumber(14),
                batch: BatchId("b1".into()),
                commit: sha("9f8e7d6a5b4c"),
                submitted_ms: EpochMs(1),
                title: None,
            },
        };
        assert_eq!(
            render(&event, &Utc),
            "2023-11-14 22:13:20  #14 머지됨 · batch b1 · 커밋 9f8e7d6"
        );
    }

    #[test]
    fn a_notification_that_was_not_sent_is_marked_as_missed() {
        let line = describe(&EventKind::Notified {
            kind: NotifyKind::Merged,
            pr: Some(PrNumber(3)),
            to: None,
            delivered: Delivered::NoTerminal,
        });
        assert!(line.contains("놓친 알림"), "{line}");
        let sent = describe(&EventKind::Notified {
            kind: NotifyKind::Merged,
            pr: Some(PrNumber(3)),
            to: None,
            delivered: Delivered::Sent,
        });
        assert!(!sent.contains("놓친"), "{sent}");
    }

    #[test]
    fn a_notification_sent_as_the_agents_return_line_is_not_marked_as_missed() {
        let line = describe(&EventKind::Notified {
            kind: NotifyKind::Merged,
            pr: Some(PrNumber(3)),
            to: None,
            delivered: Delivered::SameAsAgent,
        });
        assert!(!line.contains("놓친"), "{line}");
    }
}
