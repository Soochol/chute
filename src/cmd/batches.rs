use std::collections::BTreeSet;
use std::process::ExitCode;

use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};
use serde::Serialize;

use super::Ctx;
use crate::model::{BatchId, BatchRecord, EpochMs, EventKind, PrNumber, RevertRecord, Sha};
use crate::store::RepoStore;

const DEFAULT_LIMIT: usize = 20;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 결과를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
    /// 보일 batch 수
    #[arg(long, short = 'n', default_value_t = DEFAULT_LIMIT)]
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct Loaded {
    pub record: BatchRecord,
    pub finished: Option<(Vec<PrNumber>, Vec<PrNumber>)>,
    pub revert: Option<RevertRecord>,
    pub reverted_event: bool,
}

impl Loaded {
    pub fn is_reverted(&self) -> bool {
        self.revert.is_some() || self.reverted_event
    }

    pub fn merged(&self) -> Vec<PrNumber> {
        match &self.finished {
            Some((merged, _)) => merged.clone(),
            None => self.record.merges.iter().map(|m| m.pr).collect(),
        }
    }

    pub fn bounced(&self) -> Vec<PrNumber> {
        match &self.finished {
            Some((_, bounced)) => bounced.clone(),
            None => self.record.bounced.clone(),
        }
    }

    fn sort_key(&self) -> (u64, &str) {
        let at = self.record.finished_ms.unwrap_or(self.record.started_ms);
        (at.0, self.record.id.0.as_str())
    }
}

pub fn load_all(store: &RepoStore) -> Result<Vec<Loaded>> {
    let history = store.history()?;
    let mut all = Vec::new();
    for id in store.batch_ids()? {
        let Some(record) = store.read_batch(&id)? else {
            continue;
        };
        let finished = history.events.iter().find_map(|e| match &e.kind {
            EventKind::BatchFinished {
                batch,
                merged,
                bounced,
                ..
            } if *batch == id => Some((merged.clone(), bounced.clone())),
            _ => None,
        });
        let reverted_event = history
            .events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::Reverted { batch, .. } if *batch == id));
        all.push(Loaded {
            revert: store.read_revert(&id)?,
            record,
            finished,
            reverted_event,
        });
    }
    all.sort_by(|a, b| b.sort_key().cmp(&a.sort_key()));
    Ok(all)
}

#[derive(Debug, Serialize)]
struct Entry {
    id: BatchId,
    started_ms: EpochMs,
    finished_ms: Option<EpochMs>,
    merged: Vec<PrNumber>,
    bounced: Vec<PrNumber>,
    reverted: bool,
    revert_commit: Option<Sha>,
}

fn entries(all: &[Loaded], limit: usize) -> Vec<Entry> {
    all.iter()
        .filter(|b| !b.merged().is_empty() || !b.bounced().is_empty())
        .take(limit)
        .map(|b| Entry {
            id: b.record.id.clone(),
            started_ms: b.record.started_ms,
            finished_ms: b.record.finished_ms,
            merged: b.merged(),
            bounced: b.bounced(),
            reverted: b.is_reverted(),
            revert_commit: b.revert.as_ref().map(|r| r.tip.clone()),
        })
        .collect()
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let (_, store) = ctx.registered_repo()?;
    let entries = entries(&load_all(&store)?, args.limit);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        print!("{}", render(&entries, &Local));
    }
    Ok(ExitCode::SUCCESS)
}

fn prs(list: &[PrNumber]) -> String {
    let names: Vec<String> = list.iter().map(|p| format!("#{}", p.0)).collect();
    if names.is_empty() {
        "-".to_owned()
    } else {
        names.join(" ")
    }
}

fn render<Tz: TimeZone>(entries: &[Entry], tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if entries.is_empty() {
        return "batch가 아직 없다\n".to_owned();
    }
    let mut out = String::new();
    for e in entries {
        let finished = e
            .finished_ms
            .and_then(|ms| tz.timestamp_millis_opt(ms.0 as i64).single())
            .map_or_else(
                || "끝나지 않음".to_owned(),
                |t: DateTime<Tz>| t.format("%Y-%m-%d %H:%M").to_string(),
            );
        let mark = match &e.revert_commit {
            Some(tip) => format!(" · 되돌림 {}", short(tip)),
            None if e.reverted => " · 되돌림".to_owned(),
            None => String::new(),
        };
        out.push_str(&format!(
            "{}  {finished}  머지 {}  반송 {}{mark}\n",
            e.id.0,
            prs(&e.merged),
            prs(&e.bounced),
        ));
    }
    out
}

pub fn short(sha: &Sha) -> &str {
    &sha.as_str()[..sha.as_str().len().min(7)]
}

pub fn pr_set(list: &[PrNumber]) -> BTreeSet<PrNumber> {
    list.iter().copied().collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn entry(id: &str, merged: &[u64], reverted: bool) -> Entry {
        Entry {
            id: BatchId(id.into()),
            started_ms: EpochMs(0),
            finished_ms: Some(EpochMs(1_700_000_000_000)),
            merged: merged.iter().copied().map(PrNumber).collect(),
            bounced: vec![PrNumber(9)],
            reverted,
            revert_commit: None,
        }
    }

    #[test]
    fn a_line_names_the_merged_and_bounced_prs_and_marks_a_reverted_batch() {
        let text = render(
            &[entry("b2", &[3, 4], true), entry("b1", &[1], false)],
            &Utc,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "b2  2023-11-14 22:13  머지 #3 #4  반송 #9 · 되돌림"
        );
        assert_eq!(lines[1], "b1  2023-11-14 22:13  머지 #1  반송 #9");
    }

    #[test]
    fn no_batches_says_so() {
        assert_eq!(render(&[], &Utc), "batch가 아직 없다\n");
    }
}
