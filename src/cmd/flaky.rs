use std::process::ExitCode;

use anyhow::Result;
use chrono::{Local, TimeZone};
use serde::Serialize;

use super::events::probe_evidence;
use super::{Ctx, run_on_hub};
use crate::config::{Config, FlakyPolicy};
use crate::flaky::{Entry, Key, ledger, resolve as resolve_key};
use crate::model::EpochMs;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 이 일수 안에 흔들린 것만 센다. 없으면 설정의 flaky.chronic_days
    #[arg(long, value_name = "일", value_parser = clap::value_parser!(u32).range(1..))]
    pub days: Option<u32>,
    /// 장부를 JSON 하나로 낸다
    #[arg(long)]
    pub json: bool,
    /// 이 단계(와 시험)를 고친 것으로 표시한다. 지금까지의 흔들림은 만성 판정과 반송 판단에서 빠지고, 다시 흔들리면 풀림이 무효가 된다
    #[arg(
        long,
        num_args = 1..=2,
        value_names = ["단계", "시험"],
        requires = "note",
        conflicts_with_all = ["days", "json"]
    )]
    pub resolve: Option<Vec<String>>,
    /// --resolve의 근거(고친 PR이나 커밋, 반복 실행 결과)
    #[arg(long, value_name = "근거", requires = "resolve")]
    pub note: Option<String>,
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
    if let Some(resolve) = &args.resolve {
        forwarded.push("--resolve".into());
        forwarded.extend(resolve.iter().cloned());
    }
    if let Some(note) = &args.note {
        forwarded.extend(["--note".into(), note.clone()]);
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
    if let Some(resolve) = &args.resolve {
        let key = Key {
            step: resolve[0].clone(),
            test: resolve.get(1).cloned(),
        };
        let note = args.note.as_deref().unwrap_or_default();
        let set_aside = resolve_key(&store, EpochMs::now(), &policy, &key, note)?;
        println!(
            "{}: 고친 것으로 표시했다. 지금까지의 흔들림 {set_aside}번은 만성 판정과 반송 판단에서 빠진다. 다시 흔들리면 풀림이 무효가 된다",
            key.label()
        );
        return Ok(ExitCode::SUCCESS);
    }
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
        let at = when(entry.last_ms, tz);
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
        if let Some(resolved) = &entry.resolved {
            text.push_str(&format!(
                " · 풀림 {} ({})",
                when(resolved.at_ms, tz),
                resolved.note
            ));
        }
        text.push('\n');
    }
    text
}

fn when<Tz: TimeZone>(at: EpochMs, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    i64::try_from(at.0)
        .ok()
        .and_then(|ms| tz.timestamp_millis_opt(ms).single())
        .map_or_else(
            || format!("at={}", at.0),
            |t| t.format("%Y-%m-%d %H:%M").to_string(),
        )
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
            resolved: None,
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
    fn a_resolved_line_shows_when_and_why_instead_of_chronic() {
        let mut fixed = entry(Some("a::b"), 5, false);
        fixed.resolved = Some(crate::flaky::Resolution {
            at_ms: EpochMs(1_700_100_000_000),
            note: "PR #648".into(),
        });

        let text = render(&[fixed], 30, &FlakyPolicy::default(), &Utc);

        assert!(
            text.ends_with(
                "단계 test의 시험 a::b · 5번 · 마지막 2023-11-14 22:13 · batch b1 · 흔들림을 재현함 · 풀림 2023-11-16 02:00 (PR #648)\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn resolve_needs_a_note_and_takes_a_step_with_an_optional_test() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            args: Args,
        }
        let parse = |argv: &[&str]| Cli::try_parse_from([&["chute"], argv].concat());

        assert!(parse(&["--resolve", "e2e"]).is_err());
        assert!(parse(&["--note", "x"]).is_err());
        assert!(parse(&["--resolve", "e2e", "--note", "x", "--json"]).is_err());
        let step = parse(&["--resolve", "e2e", "--note", "x"]).unwrap().args;
        assert_eq!(step.resolve, Some(vec!["e2e".to_owned()]));
        let test = parse(&["--resolve", "e2e", "a.spec.ts:1", "--note", "x"])
            .unwrap()
            .args;
        assert_eq!(
            test.resolve,
            Some(vec!["e2e".to_owned(), "a.spec.ts:1".to_owned()])
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
