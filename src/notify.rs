use std::cell::OnceCell;
use std::fmt::Display;
use std::path::{Path, PathBuf};

use crate::ext::Ext;
use crate::feedback::{
    HC_BLOCKED, HC_FINISHED, HC_NOT_FOUND, PLAIN_MARKS, terminal, without_shell_syntax,
};
use crate::machine::Machines;
use crate::model::{
    Agent, BatchId, BounceReason, Delivered, EpochMs, Event, EventKind, Failure, HumanReason,
    MachineId, Mode, NotifyKind, PrNumber, RepoInfo, ResumedBy, Sha, Slug, TerminalId, VerifyMode,
};
use crate::store::RepoStore;

const SHA_LEN: usize = 12;
const DETAIL_CHARS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BounceDelivery {
    Sent,
    Spawned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitingWhy {
    Bounced,
    Human,
    Dropped,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Merged {
        pr: PrNumber,
        head: Sha,
        batch: BatchId,
        commit: Sha,
        repair: bool,
    },
    Bounced {
        pr: PrNumber,
        head: Sha,
        reason: BounceReason,
        mode: Option<Mode>,
        partners: Vec<PrNumber>,
        attempts: u32,
        budget: u32,
        to: Option<TerminalId>,
        delivered: BounceDelivery,
        report: Option<PathBuf>,
    },
    Human {
        pr: PrNumber,
        reason: HumanReason,
        detail: Option<String>,
    },
    Halted {
        base: Sha,
        step: Option<String>,
        pushed_by_chute: Option<bool>,
        rechecked: Option<bool>,
        log: Option<PathBuf>,
        suspect: Option<PrNumber>,
    },
    Resumed {
        base: Sha,
        by: ResumedBy,
        pr: Option<PrNumber>,
    },
    Reverted {
        batch: BatchId,
        prs: Vec<PrNumber>,
        commit: Sha,
    },
    Waiting {
        pr: PrNumber,
        after: PrNumber,
        why: WaitingWhy,
    },
    Flaky {
        batch: BatchId,
        prs: Vec<PrNumber>,
        step: String,
        tests: Vec<String>,
        made: bool,
        chronic: bool,
    },
}

pub fn mode_of(failure: &Failure) -> (Option<Mode>, Vec<PrNumber>) {
    match failure {
        Failure::Verify(step) | Failure::Timeout(step) => match &step.mode {
            VerifyMode::Solo => (Some(Mode::Solo), Vec::new()),
            VerifyMode::Combo { partners } => {
                (Some(Mode::Combo), partners.iter().map(|p| p.pr).collect())
            }
        },
        Failure::Conflict { .. } | Failure::Ci { .. } => (None, Vec::new()),
    }
}

impl Line {
    pub fn kind(&self) -> NotifyKind {
        match self {
            Line::Merged { .. } => NotifyKind::Merged,
            Line::Bounced { .. } => NotifyKind::Bounced,
            Line::Human { .. } => NotifyKind::Human,
            Line::Halted { .. } => NotifyKind::Halted,
            Line::Resumed { .. } => NotifyKind::Resumed,
            Line::Reverted { .. } => NotifyKind::Reverted,
            Line::Waiting { .. } => NotifyKind::Waiting,
            Line::Flaky { .. } => NotifyKind::Flaky,
        }
    }

    pub fn pr(&self) -> Option<PrNumber> {
        match self {
            Line::Merged { pr, .. }
            | Line::Bounced { pr, .. }
            | Line::Human { pr, .. }
            | Line::Waiting { pr, .. } => Some(*pr),
            Line::Resumed { pr, .. } => *pr,
            Line::Reverted { prs, .. } | Line::Flaky { prs, .. } => match prs.as_slice() {
                [only] => Some(*only),
                _ => None,
            },
            Line::Halted { .. } => None,
        }
    }

    pub fn render(&self, slug: &Slug, at: EpochMs) -> String {
        let mut keys = Keys::default();
        let mut sentence = self.fill(&mut keys);
        if keys.dropped {
            sentence.push_str(" 줄에 쓸 수 없는 값을 뺐다. chute events로 본다.");
        }
        let mut line = format!(
            "chute 결과 {} slug={slug} at={}",
            kind_word(self.kind()),
            at.0
        );
        for (key, value) in &keys.pairs {
            line.push_str(&format!(" {key}={value}"));
        }
        line.push(' ');
        line.push_str(&without_shell_syntax(&sentence));
        line
    }

    fn fill(&self, keys: &mut Keys) -> String {
        match self {
            Line::Merged {
                pr,
                head,
                batch,
                commit,
                repair,
            } => {
                keys.put("pr", pr.0);
                keys.sha("head", head);
                keys.text("batch", &batch.0);
                keys.sha("commit", commit);
                if *repair {
                    keys.put("repair", "yes");
                    format!("PR {}: 복구 PR이 기본 브랜치에 머지됐다.", pr.0)
                } else {
                    format!("PR {}: 기본 브랜치에 머지됐다.", pr.0)
                }
            }
            Line::Bounced {
                pr,
                head,
                reason,
                mode,
                partners,
                attempts,
                budget,
                to,
                delivered,
                report,
            } => {
                keys.put("pr", pr.0);
                keys.sha("head", head);
                keys.put("reason", bounce_word(*reason));
                if let Some(mode) = mode {
                    keys.put("mode", mode_word(*mode));
                }
                if !partners.is_empty() {
                    keys.list("partners", partners.iter().map(|p| p.0));
                }
                keys.put("attempts", attempts);
                keys.put("budget", budget);
                if let Some(to) = to {
                    keys.text("to", &to.0);
                }
                keys.put(
                    "delivered",
                    match delivered {
                        BounceDelivery::Sent => "sent",
                        BounceDelivery::Spawned => "spawned",
                    },
                );
                if let Some(report) = report {
                    keys.path("report", report);
                }
                format!(
                    "PR {}: 반송했다({}). {attempts}/{budget}번째. 작업자에게 반송 줄을 보냈다.",
                    pr.0,
                    bounce_label(*reason)
                )
            }
            Line::Human { pr, reason, detail } => {
                keys.put("pr", pr.0);
                keys.put("reason", human_word(*reason));
                let why = match reason {
                    HumanReason::Budget => "재시도 예산을 모두 써서",
                    HumanReason::Unreachable => "작업자에게 반송을 전달하지 못해서",
                    HumanReason::GaveUp => "포기해서",
                };
                let mut text = format!("PR {}: {why} 사람 차례가 됐다.", pr.0);
                if let Some(detail) = detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                    let detail: String = detail.chars().take(DETAIL_CHARS).collect();
                    text.push_str(&format!(" 이유: {detail}"));
                }
                text
            }
            Line::Halted {
                base,
                step,
                pushed_by_chute,
                rechecked,
                log,
                suspect,
            } => {
                keys.sha("base", base);
                if let Some(step) = step {
                    keys.text("step", step);
                }
                if let Some(yes) = pushed_by_chute {
                    keys.put("pushed_by_chute", yes_no(*yes));
                }
                if let Some(yes) = rechecked {
                    keys.put("rechecked", yes_no(*yes));
                }
                if let Some(log) = log {
                    keys.path("log", log);
                }
                if let Some(suspect) = suspect {
                    keys.put("suspect", suspect.0);
                }
                "큐가 멈췄다. 복구 PR을 chute submit --repair로 넣는다.".to_owned()
            }
            Line::Resumed { base, by, pr } => {
                keys.sha("base", base);
                keys.put(
                    "by",
                    match by {
                        ResumedBy::Repair => "repair",
                        ResumedBy::MainMoved => "main_moved",
                    },
                );
                if let Some(pr) = pr {
                    keys.put("pr", pr.0);
                }
                match by {
                    ResumedBy::Repair => "복구 PR이 머지돼 큐 멈춤이 풀렸다.".to_owned(),
                    ResumedBy::MainMoved => "기본 브랜치가 바뀌어 큐 멈춤이 풀렸다.".to_owned(),
                }
            }
            Line::Reverted { batch, prs, commit } => {
                keys.text("batch", &batch.0);
                keys.list("prs", prs.iter().map(|p| p.0));
                keys.sha("commit", commit);
                format!("batch {}를 되돌렸다.", batch.0)
            }
            Line::Waiting { pr, after, why } => {
                keys.put("pr", pr.0);
                keys.put("after", after.0);
                keys.put(
                    "why",
                    match why {
                        WaitingWhy::Bounced => "bounced",
                        WaitingWhy::Human => "human",
                        WaitingWhy::Dropped => "dropped",
                        WaitingWhy::Closed => "closed",
                    },
                );
                format!(
                    "PR {}: 먼저 머지돼야 할 PR {}가 머지되지 않아 잡혀 있다. 기다리거나 chute drop을 하거나 --after 없이 다시 submit한다.",
                    pr.0, after.0
                )
            }
            Line::Flaky {
                batch,
                prs,
                step,
                tests,
                made,
                chronic,
            } => {
                keys.text("batch", &batch.0);
                keys.list("prs", prs.iter().map(|p| p.0));
                keys.text("step", step);
                if !tests.is_empty() {
                    keys.texts("tests", tests);
                }
                if *made {
                    keys.put("evidence", "made");
                }
                if *chronic {
                    keys.put("chronic", "yes");
                }
                if *chronic {
                    format!("단계 {step}의 시험이 자주 흔들린다.")
                } else {
                    format!("단계 {step}의 시험이 흔들렸다.")
                }
            }
        }
    }
}

fn kind_word(kind: NotifyKind) -> &'static str {
    match kind {
        NotifyKind::Merged => "merged",
        NotifyKind::Bounced => "bounced",
        NotifyKind::Human => "human",
        NotifyKind::Halted => "halted",
        NotifyKind::Resumed => "resumed",
        NotifyKind::Reverted => "reverted",
        NotifyKind::Waiting => "waiting",
        NotifyKind::Flaky => "flaky",
    }
}

fn bounce_word(reason: BounceReason) -> &'static str {
    match reason {
        BounceReason::Conflict => "conflict",
        BounceReason::Verify => "verify",
        BounceReason::Timeout => "timeout",
        BounceReason::HeadChanged => "head_changed",
        BounceReason::Ci => "ci",
    }
}

fn bounce_label(reason: BounceReason) -> &'static str {
    match reason {
        BounceReason::Conflict => "충돌",
        BounceReason::Verify => "검증 실패",
        BounceReason::Timeout => "시간 초과",
        BounceReason::HeadChanged => "HEAD가 바뀜",
        BounceReason::Ci => "원격 CI 실패",
    }
}

fn human_word(reason: HumanReason) -> &'static str {
    match reason {
        HumanReason::Budget => "budget",
        HumanReason::Unreachable => "unreachable",
        HumanReason::GaveUp => "gave_up",
    }
}

fn mode_word(mode: Mode) -> &'static str {
    match mode {
        Mode::Solo => "solo",
        Mode::Combo => "combo",
    }
}

fn yes_no(yes: bool) -> &'static str {
    if yes { "yes" } else { "no" }
}

fn plain(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || PLAIN_MARKS.contains(c))
}

#[derive(Default)]
struct Keys {
    pairs: Vec<(&'static str, String)>,
    dropped: bool,
}

impl Keys {
    fn put(&mut self, key: &'static str, value: impl Display) {
        self.pairs.push((key, value.to_string()));
    }

    fn sha(&mut self, key: &'static str, sha: &Sha) {
        let text = sha.as_str();
        self.put(key, &text[..text.len().min(SHA_LEN)]);
    }

    fn text(&mut self, key: &'static str, value: &str) {
        if plain(value) {
            self.put(key, value);
        } else {
            self.dropped = true;
        }
    }

    fn texts(&mut self, key: &'static str, values: &[String]) {
        if values.iter().all(|v| plain(v)) {
            self.put(key, values.join(","));
        } else {
            self.dropped = true;
        }
    }

    fn path(&mut self, key: &'static str, path: &Path) {
        match path.to_str() {
            Some(text) => self.text(key, text),
            None => self.dropped = true,
        }
    }

    fn list(&mut self, key: &'static str, values: impl Iterator<Item = u64>) {
        let joined: Vec<String> = values.map(|v| v.to_string()).collect();
        self.put(key, joined.join(","));
    }
}

/// `hc` addresses another machine's terminal as `<machine>/<terminal>`.
fn bare(terminal: &TerminalId) -> &str {
    terminal
        .0
        .rsplit_once('/')
        .map_or(terminal.0.as_str(), |(_, bare)| bare)
}

/// hc may reuse an address, so `created_ms` is part of a terminal's identity.
pub fn same_terminal(a: &Agent, b: &Agent, local: Option<&MachineId>) -> bool {
    let machine = |agent: &Agent| agent.machine.clone().or_else(|| local.cloned());
    let (ma, mb) = (machine(a), machine(b));
    let address = |agent: &Agent| {
        if ma.is_none() && mb.is_none() {
            agent.terminal.0.clone()
        } else {
            bare(&agent.terminal).to_owned()
        }
    };
    ma == mb && address(a) == address(b) && a.created_ms == b.created_ms
}

pub struct Notifier<'a> {
    ext: &'a Ext,
    repo: &'a RepoInfo,
    store: &'a RepoStore,
    machines: OnceCell<Option<Machines>>,
}

impl<'a> Notifier<'a> {
    pub fn new(ext: &'a Ext, repo: &'a RepoInfo, store: &'a RepoStore) -> Notifier<'a> {
        Notifier {
            ext,
            repo,
            store,
            machines: OnceCell::new(),
        }
    }

    fn machines(&self) -> Option<&Machines> {
        self.machines
            .get_or_init(|| Machines::load(self.ext).ok())
            .as_ref()
    }

    fn same(&self, a: &Agent, b: &Agent) -> bool {
        let local = (a.machine.is_some() != b.machine.is_some())
            .then(|| self.machines().map(|m| m.local().id.clone()))
            .flatten();
        same_terminal(a, b, local.as_ref())
    }

    pub fn notify(&self, to: Option<&Agent>, at: EpochMs, line: &Line) -> Delivered {
        let Some(to) = to else {
            self.record(line, None, Delivered::NoTerminal);
            return Delivered::NoTerminal;
        };
        let (address, delivered) = match self.address(to) {
            Ok(address) => {
                let delivered = self.send(&address, to, &line.render(&self.repo.slug, at));
                (address, delivered)
            }
            Err(delivered) => (to.terminal.clone(), delivered),
        };
        self.record(line, Some(address), delivered);
        delivered
    }

    pub fn notify_unless_same(
        &self,
        to: Option<&Agent>,
        worker: Option<&Agent>,
        at: EpochMs,
        line: &Line,
    ) -> Delivered {
        match (to, worker) {
            (Some(to), Some(worker)) if self.same(to, worker) => {
                self.record(line, Some(to.terminal.clone()), Delivered::SameAsAgent);
                Delivered::SameAsAgent
            }
            _ => self.notify(to, at, line),
        }
    }

    pub fn notify_each(
        &self,
        targets: &[(Option<&Agent>, PrNumber)],
        at: EpochMs,
        line_for: impl Fn(&[PrNumber]) -> Line,
    ) {
        let mut groups: Vec<(&Agent, Vec<PrNumber>)> = Vec::new();
        for (agent, pr) in targets {
            let Some(agent) = agent else { continue };
            match groups.iter_mut().find(|(seen, _)| self.same(seen, agent)) {
                Some((_, prs)) => prs.push(*pr),
                None => groups.push((agent, vec![*pr])),
            }
        }
        if groups.is_empty() {
            let all: Vec<PrNumber> = targets.iter().map(|(_, pr)| *pr).collect();
            self.notify(None, at, &line_for(&all));
        }
        for (agent, prs) in groups {
            self.notify(Some(agent), at, &line_for(&prs));
        }
    }

    pub fn notify_waiters(&self, held: PrNumber, why: WaitingWhy, at: EpochMs) {
        let state = match self.store.load() {
            Ok(state) => state,
            Err(e) => {
                eprintln!("경고: 기다리는 PR에 알릴 큐를 읽지 못했다: {e:#}");
                return;
            }
        };
        for item in state.queue.iter().filter(|i| i.after.contains(&held)) {
            let line = Line::Waiting {
                pr: item.pr,
                after: held,
                why,
            };
            self.notify(item.submitter.as_ref(), at, &line);
        }
    }

    pub fn address_of(&self, agent: &Agent) -> TerminalId {
        self.address(agent)
            .unwrap_or_else(|_| agent.terminal.clone())
    }

    fn address(&self, to: &Agent) -> Result<TerminalId, Delivered> {
        let Some(machine) = &to.machine else {
            return Ok(to.terminal.clone());
        };
        let machines = self.machines().ok_or(Delivered::Failed)?;
        if machines.local().id == *machine {
            return Ok(TerminalId(bare(&to.terminal).to_owned()));
        }
        match machines.by_id(machine) {
            Some(found) => Ok(TerminalId(format!("{}/{}", found.name, bare(&to.terminal)))),
            None => Err(Delivered::UnknownMachine),
        }
    }

    fn send(&self, address: &TerminalId, to: &Agent, line: &str) -> Delivered {
        match terminal(self.ext, address) {
            Err(_) => return Delivered::Failed,
            Ok(Some(info)) if info.is_live_agent(to.created_ms) => {}
            Ok(_) => return Delivered::Gone,
        }
        match self.ext.hc(["send", address.0.as_str(), line]) {
            Err(_) => Delivered::Failed,
            Ok(out) => match out.code {
                Some(0) => Delivered::Sent,
                Some(HC_BLOCKED) => Delivered::Blocked,
                Some(HC_NOT_FOUND | HC_FINISHED) => Delivered::Gone,
                _ => Delivered::Failed,
            },
        }
    }

    fn record(&self, line: &Line, to: Option<TerminalId>, delivered: Delivered) {
        let written = self.store.append_history(&Event {
            at_ms: EpochMs::now(),
            kind: EventKind::Notified {
                kind: line.kind(),
                pr: line.pr(),
                to,
                delivered,
            },
        });
        if let Err(e) = written {
            eprintln!("경고: 알림 결과를 history.jsonl에 남기지 못했다: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NonEmpty, Partner, StepFailure};
    use crate::store::Home;
    use crate::testkit::{Reply, SANDBOX_MACHINE_ID, Sandbox};

    fn sha(s: &str) -> Sha {
        Sha::try_from(s.to_owned()).unwrap()
    }

    fn slug() -> Slug {
        Slug::new("acme", "widget").unwrap()
    }

    fn pr(n: u64) -> PrNumber {
        PrNumber(n)
    }

    fn agent(terminal: &str, created_ms: u64, machine: Option<&str>) -> Agent {
        Agent {
            terminal: TerminalId(terminal.into()),
            created_ms: EpochMs(created_ms),
            name: None,
            workdir: "/w".into(),
            alive: true,
            machine: machine.map(|m| MachineId::try_from(m.to_owned()).unwrap()),
        }
    }

    fn all_lines() -> Vec<Line> {
        vec![
            Line::Merged {
                pr: pr(14),
                head: sha("1a2b3c4d5e6f7a8b"),
                batch: BatchId("b20261116-140000".into()),
                commit: sha("9f8e7d6a5b4c3d2e"),
                repair: true,
            },
            Line::Bounced {
                pr: pr(15),
                head: sha("1a2b3c4d5e6f7a8b"),
                reason: BounceReason::Verify,
                mode: Some(Mode::Combo),
                partners: vec![pr(12), pr(13)],
                attempts: 1,
                budget: 3,
                to: Some(TerminalId("lap/t4".into())),
                delivered: BounceDelivery::Spawned,
                report: Some("/state/reports/pr-15/report.md".into()),
            },
            Line::Human {
                pr: pr(16),
                reason: HumanReason::GaveUp,
                detail: Some("이유: `rm -rf`; $(x) \"끝\"".into()),
            },
            Line::Halted {
                base: sha("9f8e7d6a5b4c3d2e"),
                step: Some("test".into()),
                pushed_by_chute: Some(true),
                rechecked: Some(true),
                log: Some("/state/batches/b1/verify-3/test.1.log".into()),
                suspect: Some(pr(15)),
            },
            Line::Resumed {
                base: sha("aaaaaaaaaaaaaaaa"),
                by: ResumedBy::Repair,
                pr: Some(pr(20)),
            },
            Line::Reverted {
                batch: BatchId("b1".into()),
                prs: vec![pr(1), pr(2)],
                commit: sha("bbbbbbbbbbbbbbbb"),
            },
            Line::Waiting {
                pr: pr(21),
                after: pr(20),
                why: WaitingWhy::Dropped,
            },
            Line::Flaky {
                batch: BatchId("b2".into()),
                prs: vec![pr(3)],
                step: "test".into(),
                tests: vec!["a::b".into(), "c::d".into()],
                made: true,
                chronic: true,
            },
        ]
    }

    fn keys_of(line: &str) -> Vec<(String, String)> {
        line.split_whitespace()
            .skip(3)
            .map_while(|w| w.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }

    fn key_names(line: &str) -> Vec<String> {
        keys_of(line).into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn the_eight_kinds_of_line_carry_the_planned_keys_in_the_allowed_characters() {
        let planned: [(&str, &[&str]); 8] = [
            (
                "merged",
                &["slug", "at", "pr", "head", "batch", "commit", "repair"],
            ),
            (
                "bounced",
                &[
                    "slug",
                    "at",
                    "pr",
                    "head",
                    "reason",
                    "mode",
                    "partners",
                    "attempts",
                    "budget",
                    "to",
                    "delivered",
                    "report",
                ],
            ),
            ("human", &["slug", "at", "pr", "reason"]),
            (
                "halted",
                &[
                    "slug",
                    "at",
                    "base",
                    "step",
                    "pushed_by_chute",
                    "rechecked",
                    "log",
                    "suspect",
                ],
            ),
            ("resumed", &["slug", "at", "base", "by", "pr"]),
            ("reverted", &["slug", "at", "batch", "prs", "commit"]),
            ("waiting", &["slug", "at", "pr", "after", "why"]),
            (
                "flaky",
                &[
                    "slug", "at", "batch", "prs", "step", "tests", "evidence", "chronic",
                ],
            ),
        ];
        for (line, (kind, keys)) in all_lines().iter().zip(planned) {
            let text = line.render(&slug(), EpochMs(1763270000123));
            assert!(
                text.starts_with(&format!(
                    "chute 결과 {kind} slug=acme/widget at=1763270000123"
                )),
                "{text}"
            );
            assert_eq!(key_names(&text), keys, "{text}");
            let bad: Vec<char> = text
                .chars()
                .filter(|c| !(c.is_alphanumeric() || c.is_whitespace() || PLAIN_MARKS.contains(*c)))
                .collect();
            assert!(bad.is_empty(), "{bad:?} in {text}");
            assert!(!text.contains('\n'), "{text}");
        }
    }

    #[test]
    fn values_are_short_shas_lists_and_words_and_the_sentence_starts_after_the_last_key() {
        let lines = all_lines();
        let merged = lines[0].render(&slug(), EpochMs(5));
        assert!(merged.contains(" head=1a2b3c4d5e6f "), "{merged}");
        assert!(merged.contains(" commit=9f8e7d6a5b4c "), "{merged}");
        assert!(merged.contains(" repair=yes "), "{merged}");
        let bounced = lines[1].render(&slug(), EpochMs(5));
        assert!(bounced.contains(" partners=12,13 "), "{bounced}");
        assert!(bounced.contains(" to=lap/t4 "), "{bounced}");
        for line in &lines {
            let text = line.render(&slug(), EpochMs(5));
            let sentence: Vec<&str> = text
                .split_whitespace()
                .skip(3)
                .skip_while(|w| w.contains('='))
                .collect();
            assert!(!sentence.is_empty(), "{text}");
            assert!(!sentence[0].contains('='), "{text}");
        }
        let human = lines[2].render(&slug(), EpochMs(5));
        assert!(human.contains("reason=gave_up"), "{human}");
        assert!(human.contains("끝"), "{human}");
    }

    #[test]
    fn a_path_with_characters_the_line_cannot_carry_loses_its_key_and_the_sentence_says_so() {
        let halted = |log: &str| Line::Halted {
            base: sha("9f8e7d6a5b4c3d2e"),
            step: None,
            pushed_by_chute: None,
            rechecked: None,
            log: Some(log.into()),
            suspect: None,
        };
        let odd = halted("/state/my logs/it's (1).log").render(&slug(), EpochMs(5));
        assert_eq!(key_names(&odd), ["slug", "at", "base"], "{odd}");
        assert!(odd.contains("chute events로 본다"), "{odd}");
        let fine = halted("/state/logs/a.log").render(&slug(), EpochMs(5));
        assert!(fine.contains(" log=/state/logs/a.log "), "{fine}");
        assert!(!fine.contains("chute events로 본다"), "{fine}");
    }

    #[test]
    fn the_halted_keys_the_block_does_not_know_are_left_out() {
        let bare_halt = Line::Halted {
            base: sha("9f8e7d6a5b4c3d2e"),
            step: None,
            pushed_by_chute: None,
            rechecked: None,
            log: None,
            suspect: None,
        };
        let text = bare_halt.render(&slug(), EpochMs(5));
        assert_eq!(key_names(&text), ["slug", "at", "base"], "{text}");
    }

    #[test]
    fn the_bounce_line_reads_its_mode_and_partners_from_the_failure() {
        let partner = |n: u64| Partner {
            pr: pr(n),
            title: "t".into(),
            files: Vec::new(),
            diff: "/d".into(),
        };
        let step = |mode: VerifyMode| StepFailure {
            step: "test".into(),
            mode,
            failed_tests: Vec::new(),
            log: "/l".into(),
            tree: sha("abcdef1234567"),
        };
        assert_eq!(
            mode_of(&Failure::Verify(step(VerifyMode::Solo))),
            (Some(Mode::Solo), vec![])
        );
        let combo = VerifyMode::Combo {
            partners: NonEmpty::new(vec![partner(7), partner(9)]).unwrap(),
        };
        assert_eq!(
            mode_of(&Failure::Timeout(step(combo))),
            (Some(Mode::Combo), vec![pr(7), pr(9)])
        );
    }

    #[test]
    fn a_terminal_is_the_same_only_with_the_same_machine_address_and_creation_time() {
        let lap = "0000000000000000000000000000000b";
        let desk = "0000000000000000000000000000000c";
        let local = MachineId::try_from(lap.to_owned()).unwrap();
        let base = agent("t1", 100, Some(lap));
        assert!(same_terminal(&base, &agent("t1", 100, Some(lap)), None));
        assert!(!same_terminal(&base, &agent("t2", 100, Some(lap)), None));
        assert!(!same_terminal(&base, &agent("t1", 101, Some(lap)), None));
        assert!(!same_terminal(&base, &agent("t1", 100, Some(desk)), None));
        assert!(same_terminal(
            &base,
            &agent("desk/t1", 100, Some(lap)),
            None
        ));
        assert!(same_terminal(&base, &agent("t1", 100, None), Some(&local)));
        assert!(!same_terminal(&base, &agent("t1", 100, None), None));
        let (a, b) = (agent("pcA/t9", 100, None), agent("pcB/t9", 100, None));
        assert!(!same_terminal(&a, &b, None));
    }

    struct Rig {
        sb: Sandbox,
        store: RepoStore,
        info: RepoInfo,
        ext: Ext,
    }

    impl Rig {
        fn new() -> Rig {
            let sb = Sandbox::new();
            let info = RepoInfo {
                slug: slug(),
                path: sb.root().join("work"),
                remote: "origin".into(),
                default_branch: "main".into(),
            };
            let store = Home::at(sb.chute_home()).repo(&info.slug);
            store.register(&info).unwrap();
            let ext = Ext::from_vars(|k| sb.var(k));
            Rig {
                sb,
                store,
                info,
                ext,
            }
        }

        fn live(&self, address: &str, created_ms: u64) {
            self.sb.fake("hc").respond(
                &format!("get {address} --json"),
                Reply::ok(format!(
                    r#"{{"address":"{address}","state":"live","created_ms":{created_ms},"foreground":{{"name":"claude"}}}}"#
                )),
            );
        }

        fn notify(&self, to: Option<&Agent>) -> Delivered {
            let line = Line::Merged {
                pr: pr(14),
                head: sha("1a2b3c4d5e6f"),
                batch: BatchId("b1".into()),
                commit: sha("9f8e7d6a5b4c"),
                repair: false,
            };
            Notifier::new(&self.ext, &self.info, &self.store).notify(to, EpochMs(7), &line)
        }

        fn sends(&self) -> Vec<Vec<String>> {
            self.sb
                .fake("hc")
                .calls()
                .into_iter()
                .filter(|c| c.first().is_some_and(|v| v == "send"))
                .collect()
        }

        fn notified(&self) -> Vec<(Option<String>, Delivered)> {
            self.store
                .history()
                .unwrap()
                .events
                .into_iter()
                .filter_map(|e| match e.kind {
                    EventKind::Notified { to, delivered, .. } => Some((to.map(|t| t.0), delivered)),
                    _ => None,
                })
                .collect()
        }
    }

    #[test]
    fn a_live_terminal_gets_the_line_through_hc_send_and_the_event_says_sent() {
        let rig = Rig::new();
        rig.live("t5", 100);
        rig.sb.fake("hc").respond("send t5 *", Reply::ok(""));
        let delivered = rig.notify(Some(&agent("t5", 100, None)));
        assert_eq!(delivered, Delivered::Sent);
        let sends = rig.sends();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0][1], "t5");
        assert!(
            sends[0][2].starts_with("chute 결과 merged slug=acme/widget at=7 pr=14"),
            "{:?}",
            sends[0]
        );
        assert_eq!(rig.notified(), [(Some("t5".into()), Delivered::Sent)]);
    }

    #[test]
    fn a_terminal_that_cannot_take_the_line_is_not_sent_to_and_the_event_says_why() {
        let rig = Rig::new();
        let hc = rig.sb.fake("hc");
        hc.respond(
            "get done --json",
            Reply::ok(r#"{"address":"done","state":"finished","created_ms":100}"#),
        );
        hc.respond("get gone --json", Reply::ok("").code(3));
        rig.live("reused", 999);
        rig.live("stuck", 100);
        hc.respond("send stuck *", Reply::ok("").code(9));
        hc.respond("get lost --json", Reply::ok("").code(1));
        let cases = [
            ("done", 100, Delivered::Gone),
            ("gone", 100, Delivered::Gone),
            ("reused", 100, Delivered::Gone),
            ("stuck", 100, Delivered::Blocked),
            ("lost", 100, Delivered::Failed),
        ];
        for (address, created, want) in cases {
            assert_eq!(
                rig.notify(Some(&agent(address, created, None))),
                want,
                "{address}"
            );
        }
        assert_eq!(rig.sends().len(), 1, "{:?}", rig.sends());
        assert_eq!(rig.sends()[0][1], "stuck");
        let recorded: Vec<Delivered> = rig.notified().into_iter().map(|(_, d)| d).collect();
        assert_eq!(recorded, cases.map(|(_, _, d)| d));
    }

    #[test]
    fn no_terminal_and_an_unknown_machine_are_recorded_without_any_hc_send() {
        let rig = Rig::new();
        assert_eq!(rig.notify(None), Delivered::NoTerminal);
        let stranger = "0000000000000000000000000000000f";
        assert_eq!(
            rig.notify(Some(&agent("t1", 100, Some(stranger)))),
            Delivered::UnknownMachine
        );
        assert!(rig.sends().is_empty());
        assert_eq!(
            rig.notified(),
            [
                (None, Delivered::NoTerminal),
                (Some("t1".into()), Delivered::UnknownMachine)
            ]
        );
    }

    #[test]
    fn a_terminal_on_another_machine_is_addressed_by_the_name_the_hub_knows() {
        let rig = Rig::new();
        let desk = "0000000000000000000000000000000c";
        rig.sb
            .fake("hc")
            .clear_rules()
            .respond(
                "server status --json",
                Reply::ok(format!(
                    r#"{{"core":{{"id":"{SANDBOX_MACHINE_ID}","name":"sandbox-pc"}}}}"#
                )),
            )
            .respond(
                "machine ls --json",
                Reply::ok(format!(
                    r#"{{"machines":[{{"id":"{desk}","name":"desk","state":"connected"}}]}}"#
                )),
            );
        rig.live("desk/t9", 100);
        rig.sb.fake("hc").respond("send desk/t9 *", Reply::ok(""));
        let delivered = rig.notify(Some(&agent("t9", 100, Some(desk))));
        assert_eq!(delivered, Delivered::Sent);
        assert_eq!(rig.sends()[0][1], "desk/t9");
        assert_eq!(rig.notified(), [(Some("desk/t9".into()), Delivered::Sent)]);
    }

    #[test]
    fn the_submitter_that_is_also_the_worker_gets_nothing_and_the_event_says_so() {
        let rig = Rig::new();
        rig.live("t5", 100);
        let notifier = Notifier::new(&rig.ext, &rig.info, &rig.store);
        let line = all_lines().remove(1);
        let same = agent("t5", 100, None);
        let delivered = notifier.notify_unless_same(Some(&same), Some(&same), EpochMs(7), &line);
        assert_eq!(delivered, Delivered::SameAsAgent);
        assert!(rig.sends().is_empty());
        assert_eq!(
            rig.notified(),
            [(Some("t5".into()), Delivered::SameAsAgent)]
        );
    }

    #[test]
    fn each_terminal_gets_one_line_with_its_own_prs() {
        let rig = Rig::new();
        rig.live("t5", 100);
        rig.live("t6", 100);
        let hc = rig.sb.fake("hc");
        hc.respond("send t5 *", Reply::ok(""));
        hc.respond("send t6 *", Reply::ok(""));
        let (a, b) = (agent("t5", 100, None), agent("t6", 100, None));
        let notifier = Notifier::new(&rig.ext, &rig.info, &rig.store);
        notifier.notify_each(
            &[
                (Some(&a), pr(1)),
                (Some(&b), pr(2)),
                (Some(&a), pr(3)),
                (None, pr(4)),
            ],
            EpochMs(9),
            |prs| Line::Reverted {
                batch: BatchId("b1".into()),
                prs: prs.to_vec(),
                commit: sha("bbbbbbbbbbbb"),
            },
        );
        let sends = rig.sends();
        assert_eq!(sends.len(), 2, "{sends:?}");
        assert!(sends[0][2].contains("prs=1,3"), "{:?}", sends[0]);
        assert!(sends[1][2].contains("prs=2 "), "{:?}", sends[1]);
    }
}
