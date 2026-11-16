use std::process::ExitCode;

use anyhow::Result;
use chrono::{Local, TimeZone};
use serde::Serialize;

use super::events::probe_evidence;
use super::{Ctx, run_on_hub};
use crate::config::{Config, FlakyPolicy};
use crate::flaky::{Entry, ledger};
use crate::model::EpochMs;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 이 일수 안에 흔들린 것만 센다. 없으면 설정의 flaky.chronic_days
    #[arg(long, value_name = "일", value_parser = clap::value_parser!(u32).range(1..))]
    pub days: Option<u32>,
    /// 장부를 JSON 하나로 낸다
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct Report<'a> {
    days: u32,
    chronic: u32,
    chronic_days: u32,
    entries: &'a [Entry],
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let mut forwarded = vec!["flaky".to_owned()];
    if let Some(days) = args.days {
        forwarded.extend(["--days".into(), days.to_string()]);
    }
    if args.json {
        forwarded.push("--json".into());
    }
    let forwarded: Vec<&str> = forwarded.iter().map(String::as_str).collect();
    if let Some(code) = run_on_hub(ctx, &forwarded)? {
        return Ok(code);
    }
    let (info, store) = ctx.registered_repo()?;
    let policy = match Config::load_remote_branch(
        &ctx.ext,
        &info.path,
        &info.remote_name,
        &info.default_branch,
    ) {
        Ok(Some(config)) => config.flaky,
        Ok(None) => {
            eprintln!("경고: chute.toml을 읽지 못해 기본 기준으로 센다");
            FlakyPolicy::default()
        }
        Err(e) => {
            eprintln!("경고: chute.toml을 읽지 못해 기본 기준으로 센다: {e:#}");
            FlakyPolicy::default()
        }
    };
    let history = store.history()?;
    if !history.broken_lines.is_empty() {
        eprintln!(
            "경고: history에서 읽지 못한 줄이 {}개 있어 건너뛰었다",
            history.broken_lines.len()
        );
    }
    let days = args.days.unwrap_or(policy.chronic_days);
    let entries = ledger(&history.events, EpochMs::now(), &policy, days);
    if args.json {
        let report = Report {
            days,
            chronic: policy.chronic,
            chronic_days: policy.chronic_days,
            entries: &entries,
        };
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print!("{}", render(&entries, days, &policy, &Local));
    }
    Ok(ExitCode::SUCCESS)
}

fn render<Tz: TimeZone>(entries: &[Entry], days: u32, policy: &FlakyPolicy, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if entries.is_empty() {
        return format!("최근 {days}일에 흔들린 시험이 없다\n");
    }
    let mut text = format!(
        "최근 {days}일에 흔들린 시험 (만성: {}일에 {}번)\n",
        policy.chronic_days, policy.chronic
    );
    for entry in entries {
        let at = i64::try_from(entry.last_ms.0)
            .ok()
            .and_then(|ms| tz.timestamp_millis_opt(ms).single())
            .map_or_else(
                || format!("at={}", entry.last_ms.0),
                |t| t.format("%Y-%m-%d %H:%M").to_string(),
            );
        text.push_str(&format!(
            "{} · {}번 · 마지막 {at} · batch {}",
            entry.key.label(),
            entry.count,
            entry.last_batch.0
        ));
        if let Some(evidence) = entry.evidence {
            text.push_str(&format!(" · {}", probe_evidence(evidence)));
        }
        if entry.chronic {
            text.push_str(" · 만성");
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::flaky::Key;
    use crate::model::{BatchId, ProbeEvidence};

    fn entry(test: Option<&str>, count: u32, chronic: bool) -> Entry {
        Entry {
            key: Key {
                step: "test".into(),
                test: test.map(str::to_owned),
            },
            count,
            last_ms: EpochMs(1_700_000_000_000),
            last_batch: BatchId("b1".into()),
            evidence: Some(ProbeEvidence::Made),
            chronic,
        }
    }

    #[test]
    fn flaky_lines_name_the_test_the_count_the_evidence_and_chronic() {
        let text = render(
            &[entry(Some("a::b"), 3, true), entry(None, 1, false)],
            30,
            &FlakyPolicy::default(),
            &Utc,
        );

        assert_eq!(
            text,
            "최근 30일에 흔들린 시험 (만성: 30일에 3번)\n\
             단계 test의 시험 a::b · 3번 · 마지막 2023-11-14 22:13 · batch b1 · 흔들림을 재현함 · 만성\n\
             단계 test · 1번 · 마지막 2023-11-14 22:13 · batch b1 · 흔들림을 재현함\n"
        );
    }

    #[test]
    fn flaky_without_entries_says_so() {
        assert_eq!(
            render(&[], 7, &FlakyPolicy::default(), &Utc),
            "최근 7일에 흔들린 시험이 없다\n"
        );
    }
}
