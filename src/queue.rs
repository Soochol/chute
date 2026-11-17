use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroUsize;

use crate::model::{
    Agent, Approval, BounceReason, ConfigChange, ConfigChangeKind, EpochMs, FailureKind,
    FailureRecord, ItemState, PrNumber, PreflightRecord, QueueItem, QueuedReason, Sha, State,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Approve,
    Resubmit {
        head_changed: bool,
        needs_approval: bool,
    },
    Take,
    Requeue,
    Merge,
    Close,
    Bounce(BounceReason),
    OverBudget,
    GiveUp,
    Drop,
    MarkCiPending,
    ClearCiPending,
}

impl Action {
    fn verb(self) -> &'static str {
        match self {
            Action::Approve => "승인",
            Action::Resubmit { .. } => "다시 submit",
            Action::Take => "batch로 가져가기",
            Action::Requeue => "큐로 되돌리기",
            Action::Merge => "머지 처리",
            Action::Close => "닫힌 PR 빼기",
            Action::Bounce(_) => "반송",
            Action::OverBudget => "반송 한도 넘겨 빼기",
            Action::GiveUp => "포기로 빼기",
            Action::Drop => "큐에서 빼기",
            Action::MarkCiPending | Action::ClearCiPending => "CI 대기 표시 바꾸기",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    To(ItemState),
    Remove,
}

pub fn step(from: ItemState, action: Action) -> Option<Next> {
    use Action::*;
    use ItemState::*;
    let to = |state| Some(Next::To(state));
    match (from, action) {
        (AwaitingApproval, Approve) => to(Queued(None)),
        (
            AwaitingApproval,
            Resubmit {
                head_changed: false,
                needs_approval: false,
            },
        ) => to(Queued(None)),
        (
            AwaitingApproval | Queued(_),
            Resubmit {
                head_changed: false,
                ..
            },
        ) => to(from),
        (
            AwaitingApproval | Queued(_),
            Resubmit {
                head_changed: true,
                needs_approval,
            },
        ) => to(entry_state(needs_approval)),
        (
            Bounced(_),
            Resubmit {
                head_changed,
                needs_approval,
            },
        ) => to(if head_changed {
            entry_state(needs_approval)
        } else {
            Queued(None)
        }),
        (Queued(_), Take) => to(InBatch),
        (InBatch, Requeue) => to(Queued(None)),
        (InBatch, Merge | Close) => Some(Next::Remove),
        (InBatch, Bounce(reason)) => to(Bounced(reason)),
        (InBatch, OverBudget) => Some(Next::Remove),
        (AwaitingApproval | Queued(_) | Bounced(_), GiveUp | Drop) => Some(Next::Remove),
        (Queued(_), MarkCiPending) => to(Queued(Some(QueuedReason::CiPending))),
        (Queued(_), ClearCiPending) => to(Queued(None)),
        _ => None,
    }
}

pub fn entry_state(needs_approval: bool) -> ItemState {
    if needs_approval {
        ItemState::AwaitingApproval
    } else {
        ItemState::Queued(None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionError {
    NotQueued(PrNumber),
    NotAllowed {
        pr: PrNumber,
        from: ItemState,
        action: Action,
    },
    HeadMoved {
        pr: PrNumber,
        seen: String,
        now: Sha,
    },
    BadHead {
        pr: PrNumber,
        given: String,
    },
    AfterSelf(PrNumber),
    AfterCycle {
        pr: PrNumber,
        after: PrNumber,
    },
    AfterGone {
        pr: PrNumber,
        after: PrNumber,
    },
    RepairNotHalted(PrNumber),
    RepairTaken {
        pr: PrNumber,
        held: PrNumber,
        state: ItemState,
    },
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransitionError::NotQueued(pr) => write!(f, "{pr}: 큐에 없다"),
            TransitionError::NotAllowed { pr, from, action } => {
                f.write_str(&explain(*pr, *from, *action))
            }
            TransitionError::HeadMoved { pr, seen, now } => write!(
                f,
                "{pr}: HEAD가 바뀌어 승인하지 않았다. 승인하려던 HEAD {seen}, 지금 HEAD {now}. 바뀐 내용을 확인한 뒤 다시 승인한다"
            ),
            TransitionError::BadHead { pr, given } => {
                let shown: String = given.chars().take(60).collect();
                let cut = if shown.len() < given.len() { "…" } else { "" };
                write!(
                    f,
                    "{pr}: 승인하지 않았다. `--head`는 4자 이상의 16진수 커밋 SHA이고 지금 HEAD보다 길 수 없다. 받은 값: `{shown}{cut}`"
                )
            }
            TransitionError::AfterSelf(pr) => {
                write!(f, "{pr}: 자기 자신 뒤에 둘 수 없다. `--after`에서 뺀다")
            }
            TransitionError::AfterGone { pr, after } => write!(
                f,
                "{pr}: --after {after}가 그사이 큐에서 빠졌다. 다시 submit한다"
            ),
            TransitionError::RepairNotHalted(pr) => write!(
                f,
                "{pr}: 큐가 멈춰 있지 않아 복구 PR로 받지 않았다. 복구 PR은 기준 커밋이 깨져 큐가 멈췄을 때나 머지 뒤 검사 실패로 PR을 잡아 둘 때만 받는다. 일반 PR이면 `--repair` 없이 submit한다"
            ),
            TransitionError::RepairTaken { pr, held, state } => write!(
                f,
                "{pr}: 복구 PR {held}이 아직 큐에 있어({}) 받지 않았다. 복구 PR은 한 번에 하나만 받는다. {held}이 머지되길 기다리거나 `chute drop {}`으로 뺀 뒤 다시 submit한다",
                label(*state),
                held.0
            ),
            TransitionError::AfterCycle { pr, after } => write!(
                f,
                "{pr}: {after}가 이미 {pr} 뒤에 있어 서로 기다리게 된다. `--after`를 고치거나 {after}를 `--after` 없이 다시 submit한다"
            ),
        }
    }
}

impl std::error::Error for TransitionError {}

fn explain(pr: PrNumber, from: ItemState, action: Action) -> String {
    use Action::*;
    use ItemState::*;
    let now = label(from);
    match (from, action) {
        (InBatch, Resubmit { .. } | GiveUp | Drop) => {
            format!("{pr}: 지금 batch에서 검증 중이다. batch가 끝난 뒤 다시 한다")
        }
        (_, Approve) => {
            format!("{pr}: 승인을 기다리는 PR만 승인할 수 있다. 지금 상태는 {now}")
        }
        _ => format!("{pr}: {}할 수 없다. 지금 상태는 {now}", action.verb()),
    }
}

pub fn label(state: ItemState) -> String {
    use crate::model::{BounceReason as B, QueuedReason as Q};
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
    }
}

fn item_mut(state: &mut State, pr: PrNumber) -> Result<&mut QueueItem, TransitionError> {
    state
        .queue
        .iter_mut()
        .find(|i| i.pr == pr)
        .ok_or(TransitionError::NotQueued(pr))
}

fn moved(pr: PrNumber, from: ItemState, action: Action) -> Result<ItemState, TransitionError> {
    match step(from, action) {
        Some(Next::To(to)) => Ok(to),
        _ => Err(TransitionError::NotAllowed { pr, from, action }),
    }
}

fn move_to(
    state: &mut State,
    pr: PrNumber,
    action: Action,
) -> Result<&mut QueueItem, TransitionError> {
    let item = item_mut(state, pr)?;
    item.state = moved(pr, item.state, action)?;
    Ok(item)
}

fn take_out(state: &mut State, pr: PrNumber, action: Action) -> Result<QueueItem, TransitionError> {
    let at = state
        .queue
        .iter()
        .position(|i| i.pr == pr)
        .ok_or(TransitionError::NotQueued(pr))?;
    let from = state.queue[at].state;
    match step(from, action) {
        Some(Next::Remove) => Ok(state.queue.remove(at)),
        _ => Err(TransitionError::NotAllowed { pr, from, action }),
    }
}

#[derive(Clone, Debug)]
pub struct Submission {
    pub pr: PrNumber,
    pub title: String,
    pub branch: String,
    pub head: Sha,
    pub agent: Option<Agent>,
    pub submitter: Option<Agent>,
    pub touches_config: bool,
    pub config_change: Option<ConfigChange>,
    pub approval: Approval,
    pub priority: bool,
    pub after: Vec<PrNumber>,
    pub repair: bool,
    pub now: EpochMs,
}

impl Submission {
    pub fn needs_approval(&self) -> bool {
        self.approval == Approval::Human
            || matches!(
                self.config_change.as_ref().map(|c| c.kind),
                Some(ConfigChangeKind::Invalid | ConfigChangeKind::Removed)
            )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Added,
    Requeued,
    HeadUpdated,
    OptionsUpdated,
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Submitted {
    pub state: ItemState,
    pub position: NonZeroUsize,
    pub change: Change,
}

pub fn submit(state: &mut State, mut s: Submission) -> Result<Submitted, TransitionError> {
    let needs_approval = s.needs_approval();
    let pr = s.pr;
    if s.repair {
        check_repair(state, pr)?;
    }
    s.after = checked_after(state, s.pr, &s.after)?;
    let Some(at) = state.queue.iter().position(|i| i.pr == s.pr) else {
        let entry = entry_state(needs_approval);
        forget_other_heads(&mut state.preflights, &s.branch, &s.head);
        state.queue.push(QueueItem {
            pr: s.pr,
            title: s.title,
            branch: s.branch,
            head: s.head,
            state: entry,
            agent: s.agent,
            attempts: 0,
            submitted_ms: s.now,
            touches_config: s.touches_config,
            last_failure: None,
            submitter: s.submitter,
            priority: s.priority,
            after: s.after,
            repair: s.repair,
            config_change: s.config_change,
        });
        return Ok(Submitted {
            state: entry,
            position: place_in_batch_order(&state.queue, pr),
            change: Change::Added,
        });
    };
    let item = &mut state.queue[at];
    let head_changed = item.head != s.head;
    let was_bounced = matches!(item.state, ItemState::Bounced(_));
    let to = moved(
        s.pr,
        item.state,
        Action::Resubmit {
            head_changed,
            needs_approval,
        },
    )?;
    let released = item.state == ItemState::AwaitingApproval && to != item.state;
    if s.submitter.is_some() {
        item.submitter = s.submitter;
    }
    let options_changed = item.priority != s.priority
        || !same_set(&item.after, &s.after)
        || (s.repair && !item.repair);
    item.priority = s.priority;
    item.after = s.after;
    item.repair |= s.repair;
    let change = match (was_bounced || (released && !head_changed), head_changed) {
        (true, _) => Change::Requeued,
        (false, true) => Change::HeadUpdated,
        (false, false) => {
            return Ok(Submitted {
                state: item.state,
                position: place_in_batch_order(&state.queue, pr),
                change: if options_changed {
                    Change::OptionsUpdated
                } else {
                    Change::Unchanged
                },
            });
        }
    };
    forget_other_heads(&mut state.preflights, &s.branch, &s.head);
    item.state = to;
    item.head = s.head;
    item.title = s.title;
    item.touches_config = s.touches_config;
    if head_changed || item.config_change.is_none() {
        item.config_change = s.config_change;
    }
    if s.agent.is_some() {
        item.agent = s.agent;
    }
    Ok(Submitted {
        state: to,
        position: place_in_batch_order(&state.queue, pr),
        change,
    })
}

fn check_repair(state: &State, pr: PrNumber) -> Result<(), TransitionError> {
    if state.blocked.is_none() && !state.post.holding() {
        return Err(TransitionError::RepairNotHalted(pr));
    }
    match state.queue.iter().find(|i| i.repair && i.pr != pr) {
        Some(held) => Err(TransitionError::RepairTaken {
            pr,
            held: held.pr,
            state: held.state,
        }),
        None => Ok(()),
    }
}

fn checked_after(
    state: &State,
    pr: PrNumber,
    after: &[PrNumber],
) -> Result<Vec<PrNumber>, TransitionError> {
    let mut kept: Vec<PrNumber> = Vec::new();
    for &n in after {
        if n == pr {
            return Err(TransitionError::AfterSelf(pr));
        }
        if !state.queue.iter().any(|i| i.pr == n) {
            return Err(TransitionError::AfterGone { pr, after: n });
        }
        if waits_for(&state.queue, n, pr) {
            return Err(TransitionError::AfterCycle { pr, after: n });
        }
        if !kept.contains(&n) {
            kept.push(n);
        }
    }
    Ok(kept)
}

fn waits_for(queue: &[QueueItem], pr: PrNumber, target: PrNumber) -> bool {
    let mut seen = vec![pr];
    let mut todo = vec![pr];
    while let Some(next) = todo.pop() {
        let Some(item) = queue.iter().find(|i| i.pr == next) else {
            continue;
        };
        for &n in &item.after {
            if n == target {
                return true;
            }
            if !seen.contains(&n) {
                seen.push(n);
                todo.push(n);
            }
        }
    }
    false
}

fn forget_other_heads(
    preflights: &mut BTreeMap<String, PreflightRecord>,
    branch: &str,
    head: &Sha,
) {
    preflights.retain(|b, p| b != branch || &p.head == head);
}

fn same_set(a: &[PrNumber], b: &[PrNumber]) -> bool {
    a.len() == b.len() && a.iter().all(|n| b.contains(n))
}

/// Where `pr` stands in the order a batch takes items.
fn place_in_batch_order(queue: &[QueueItem], pr: PrNumber) -> NonZeroUsize {
    let at = batch_order(queue)
        .iter()
        .position(|p| *p == pr)
        .unwrap_or(0);
    place(at)
}

fn place(zero_based_index: usize) -> NonZeroUsize {
    NonZeroUsize::MIN.saturating_add(zero_based_index)
}

/// `seen_head` is the HEAD the approver looked at, as a full SHA or a prefix of one.
pub fn approve(
    state: &mut State,
    pr: PrNumber,
    seen_head: Option<&str>,
) -> Result<(), TransitionError> {
    let item = item_mut(state, pr)?;
    let to = moved(pr, item.state, Action::Approve)?;
    if let Some(given) = seen_head {
        let bad = || TransitionError::BadHead {
            pr,
            given: given.to_owned(),
        };
        let seen = Sha::try_from(given.trim().to_ascii_lowercase()).map_err(|_| bad())?;
        if seen.as_str().len() > item.head.as_str().len() {
            return Err(bad());
        }
        if !item.head.as_str().starts_with(seen.as_str()) {
            return Err(TransitionError::HeadMoved {
                pr,
                seen: seen.to_string(),
                now: item.head.clone(),
            });
        }
    }
    item.state = to;
    Ok(())
}

pub fn drop_item(state: &mut State, pr: PrNumber) -> Result<QueueItem, TransitionError> {
    take_out(state, pr, Action::Drop)
}

pub fn give_up(state: &mut State, pr: PrNumber) -> Result<QueueItem, TransitionError> {
    take_out(state, pr, Action::GiveUp)
}

pub fn batch_order(queue: &[QueueItem]) -> Vec<PrNumber> {
    let mut order: Vec<&QueueItem> = queue.iter().collect();
    order.sort_by_key(|item| (!item.repair, !item.priority));
    order.into_iter().map(|item| item.pr).collect()
}

pub fn repair_ready(state: &State) -> bool {
    state
        .queue
        .iter()
        .any(|i| i.repair && matches!(i.state, ItemState::Queued(_)))
}

pub fn take_repair(state: &mut State, due_waiters: &BTreeSet<PrNumber>) -> Vec<PrNumber> {
    let Some(item) = state.queue.iter_mut().find(|i| {
        let waiting = i.state == ItemState::Queued(Some(QueuedReason::CiPending));
        i.repair && i.after.is_empty() && (!waiting || due_waiters.contains(&i.pr))
    }) else {
        return Vec::new();
    };
    match step(item.state, Action::Take) {
        Some(Next::To(to)) => {
            item.state = to;
            vec![item.pr]
        }
        _ => Vec::new(),
    }
}

pub fn take_batch(
    state: &mut State,
    max_batch: u32,
    due_waiters: &BTreeSet<PrNumber>,
) -> Vec<PrNumber> {
    take_batch_except(state, max_batch, due_waiters, &BTreeSet::new())
}

pub fn take_batch_except(
    state: &mut State,
    max_batch: u32,
    due_waiters: &BTreeSet<PrNumber>,
    skip: &BTreeSet<PrNumber>,
) -> Vec<PrNumber> {
    let mut taken = Vec::new();
    let mut others = 0;
    let order = batch_order(&state.queue);
    for ci_pass in [true, false] {
        for pr in &order {
            let Some(item) = state.queue.iter_mut().find(|i| i.pr == *pr) else {
                continue;
            };
            let is_waiting = item.state == ItemState::Queued(Some(QueuedReason::CiPending));
            let due = !is_waiting || due_waiters.contains(&item.pr);
            let held = !item.after.is_empty() || skip.contains(&item.pr);
            if is_waiting != ci_pass || !due || held || (!ci_pass && others >= max_batch) {
                continue;
            }
            if let Some(Next::To(to)) = step(item.state, Action::Take) {
                item.state = to;
                taken.push(item.pr);
                others += u32::from(!ci_pass);
            }
        }
    }
    taken
}

pub fn merged(state: &mut State, pr: PrNumber) -> Result<QueueItem, TransitionError> {
    let item = take_out(state, pr, Action::Merge)?;
    release_after(state, pr);
    Ok(item)
}

/// Frees every item that waited for `pr`, which is now in the default branch.
pub fn release_after(state: &mut State, pr: PrNumber) {
    for waiting in &mut state.queue {
        waiting.after.retain(|n| *n != pr);
    }
}

pub fn closed(state: &mut State, pr: PrNumber) -> Result<QueueItem, TransitionError> {
    take_out(state, pr, Action::Close)
}

pub fn record_agent(state: &mut State, pr: PrNumber, agent: Agent) -> Result<(), TransitionError> {
    item_mut(state, pr)?.agent = Some(agent);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    Bounced(BounceReason),
    OverBudget(Box<QueueItem>),
}

pub fn record_failure(
    state: &mut State,
    pr: PrNumber,
    failure: FailureRecord,
    budget: u32,
) -> Result<Recorded, TransitionError> {
    let item = item_mut(state, pr)?;
    if item.attempts >= budget {
        let mut gone = take_out(state, pr, Action::OverBudget)?;
        gone.last_failure = Some(failure);
        return Ok(Recorded::OverBudget(Box::new(gone)));
    }
    let reason = bounce_reason(failure.failure.shape().kind());
    item.state = moved(pr, item.state, Action::Bounce(reason))?;
    item.attempts += 1;
    item.last_failure = Some(failure);
    Ok(Recorded::Bounced(reason))
}

pub fn head_changed(state: &mut State, pr: PrNumber) -> Result<(), TransitionError> {
    move_to(state, pr, Action::Bounce(BounceReason::HeadChanged)).map(|_| ())
}

pub fn requeue(state: &mut State, pr: PrNumber) -> Result<(), TransitionError> {
    move_to(state, pr, Action::Requeue).map(|_| ())
}

pub fn set_ci_pending(state: &mut State, pr: PrNumber) -> Result<(), TransitionError> {
    move_to(state, pr, Action::MarkCiPending).map(|_| ())
}

pub fn clear_ci_pending(state: &mut State, pr: PrNumber) -> Result<(), TransitionError> {
    move_to(state, pr, Action::ClearCiPending).map(|_| ())
}

fn bounce_reason(kind: FailureKind) -> BounceReason {
    match kind {
        FailureKind::Conflict => BounceReason::Conflict,
        FailureKind::Verify => BounceReason::Verify,
        FailureKind::Timeout => BounceReason::Timeout,
        FailureKind::Ci => BounceReason::Ci,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CommitRef, ConflictFile, FailedCheck, Failure, NonEmpty, StepFailure, VerifyMode,
    };

    const AWAITING: ItemState = ItemState::AwaitingApproval;
    const QUEUED: ItemState = ItemState::Queued(None);
    const CI_PENDING: ItemState = ItemState::Queued(Some(QueuedReason::CiPending));
    const IN_BATCH: ItemState = ItemState::InBatch;

    const BOUNCE_REASONS: [BounceReason; 5] = [
        BounceReason::Conflict,
        BounceReason::Verify,
        BounceReason::Timeout,
        BounceReason::HeadChanged,
        BounceReason::Ci,
    ];

    fn bounced_states() -> Vec<ItemState> {
        BOUNCE_REASONS.map(ItemState::Bounced).to_vec()
    }

    fn all_states() -> Vec<ItemState> {
        let mut states = vec![AWAITING, QUEUED, CI_PENDING, IN_BATCH];
        states.extend(bounced_states());
        states
    }

    fn all_actions() -> Vec<Action> {
        let mut actions = vec![
            Action::Approve,
            Action::Take,
            Action::Requeue,
            Action::Merge,
            Action::Close,
            Action::OverBudget,
            Action::GiveUp,
            Action::Drop,
            Action::MarkCiPending,
            Action::ClearCiPending,
        ];
        for head_changed in [false, true] {
            for needs_approval in [false, true] {
                actions.push(Action::Resubmit {
                    head_changed,
                    needs_approval,
                });
            }
        }
        actions.extend(BOUNCE_REASONS.map(Action::Bounce));
        actions
    }

    fn resubmit(head_changed: bool, needs_approval: bool) -> Action {
        Action::Resubmit {
            head_changed,
            needs_approval,
        }
    }

    fn allow(
        table: &mut Vec<(ItemState, Action, Next)>,
        from: &[ItemState],
        action: Action,
        next: Next,
    ) {
        table.extend(from.iter().map(|&f| (f, action, next)));
    }

    fn plan_table() -> Vec<(ItemState, Action, Next)> {
        use Next::{Remove, To};
        let mut t = Vec::new();
        let waiting = [vec![AWAITING, QUEUED, CI_PENDING], bounced_states()].concat();

        allow(&mut t, &[AWAITING], Action::Approve, To(QUEUED));

        allow(&mut t, &[AWAITING], resubmit(false, true), To(AWAITING));
        allow(&mut t, &[AWAITING], resubmit(false, false), To(QUEUED));
        for needs_approval in [false, true] {
            allow(
                &mut t,
                &[QUEUED],
                resubmit(false, needs_approval),
                To(QUEUED),
            );
            allow(
                &mut t,
                &[CI_PENDING],
                resubmit(false, needs_approval),
                To(CI_PENDING),
            );
            allow(
                &mut t,
                &bounced_states(),
                resubmit(false, needs_approval),
                To(QUEUED),
            );
        }
        allow(&mut t, &waiting, resubmit(true, true), To(AWAITING));
        allow(&mut t, &waiting, resubmit(true, false), To(QUEUED));

        allow(&mut t, &[QUEUED, CI_PENDING], Action::Take, To(IN_BATCH));
        allow(&mut t, &[IN_BATCH], Action::Requeue, To(QUEUED));
        allow(&mut t, &[IN_BATCH], Action::Merge, Remove);
        allow(&mut t, &[IN_BATCH], Action::Close, Remove);
        for reason in BOUNCE_REASONS {
            allow(
                &mut t,
                &[IN_BATCH],
                Action::Bounce(reason),
                To(ItemState::Bounced(reason)),
            );
        }
        allow(&mut t, &[IN_BATCH], Action::OverBudget, Remove);
        allow(&mut t, &waiting, Action::GiveUp, Remove);
        allow(&mut t, &waiting, Action::Drop, Remove);
        allow(
            &mut t,
            &[QUEUED, CI_PENDING],
            Action::MarkCiPending,
            To(CI_PENDING),
        );
        allow(
            &mut t,
            &[QUEUED, CI_PENDING],
            Action::ClearCiPending,
            To(QUEUED),
        );
        t
    }

    #[test]
    fn transition_table_matches_the_plan_and_refuses_everything_else() {
        let table = plan_table();
        for (i, (from, action, _)) in table.iter().enumerate() {
            assert!(
                !table[..i].iter().any(|(f, a, _)| f == from && a == action),
                "표에 {from:?} + {action:?}가 두 번 있다"
            );
        }
        let mut checked = 0;
        for from in all_states() {
            for action in all_actions() {
                let expected = table
                    .iter()
                    .find(|(f, a, _)| *f == from && *a == action)
                    .map(|(_, _, next)| *next);
                assert_eq!(step(from, action), expected, "{from:?} + {action:?}");
                checked += 1;
            }
        }
        assert_eq!(checked, 9 * 19);
    }

    #[test]
    fn nothing_leaves_a_batch_except_by_merge_bounce_over_budget_or_requeue() {
        for action in all_actions() {
            let allowed = step(IN_BATCH, action).is_some();
            let expected = matches!(
                action,
                Action::Requeue
                    | Action::Merge
                    | Action::Close
                    | Action::Bounce(_)
                    | Action::OverBudget
            );
            assert_eq!(allowed, expected, "{action:?}");
        }
    }

    fn take_all(state: &mut State) -> Vec<PrNumber> {
        let due = state.queue.iter().map(|i| i.pr).collect();
        take_batch(state, 8, &due)
    }

    fn sha(s: &str) -> Sha {
        Sha::try_from(s.to_owned()).unwrap()
    }

    fn item(pr: u64, state: ItemState) -> QueueItem {
        QueueItem {
            pr: PrNumber(pr),
            title: format!("PR {pr}"),
            branch: format!("feat/{pr}"),
            head: sha("abc1234"),
            state,
            agent: None,
            attempts: 0,
            submitted_ms: EpochMs(pr),
            touches_config: false,
            last_failure: None,
            submitter: None,
            priority: false,
            after: Vec::new(),
            repair: false,
            config_change: None,
        }
    }

    fn queue_of(items: &[(u64, ItemState)]) -> State {
        State {
            queue: items.iter().map(|&(pr, s)| item(pr, s)).collect(),
            ..State::default()
        }
    }

    fn state_of(state: &State, pr: u64) -> ItemState {
        state
            .queue
            .iter()
            .find(|i| i.pr == PrNumber(pr))
            .unwrap()
            .state
    }

    fn prs(taken: &[PrNumber]) -> Vec<u64> {
        taken.iter().map(|p| p.0).collect()
    }

    #[test]
    fn take_batch_keeps_submit_order_and_stops_at_max_batch() {
        let mut state = queue_of(&[
            (7, QUEUED),
            (3, QUEUED),
            (9, QUEUED),
            (4, QUEUED),
            (5, QUEUED),
        ]);
        let taken = take_batch(&mut state, 3, &BTreeSet::new());
        assert_eq!(prs(&taken), [7, 3, 9]);
        assert_eq!(state_of(&state, 7), IN_BATCH);
        assert_eq!(state_of(&state, 9), IN_BATCH);
        assert_eq!(state_of(&state, 4), QUEUED);
        assert_eq!(state_of(&state, 5), QUEUED);
    }

    fn flag(state: &mut State, pr: u64, priority: bool, after: &[u64]) {
        let item = state
            .queue
            .iter_mut()
            .find(|i| i.pr == PrNumber(pr))
            .unwrap();
        item.priority = priority;
        item.after = after.iter().map(|n| PrNumber(*n)).collect();
    }

    #[test]
    fn take_batch_takes_priority_items_first_and_each_group_in_submit_order() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED), (3, QUEUED), (4, QUEUED)]);
        flag(&mut state, 3, true, &[]);
        flag(&mut state, 4, true, &[]);
        assert_eq!(
            prs(&take_batch(&mut state, 8, &BTreeSet::new())),
            [3, 4, 1, 2]
        );
    }

    #[test]
    fn max_batch_counts_priority_and_other_items_together() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED), (3, QUEUED), (4, QUEUED)]);
        flag(&mut state, 4, true, &[]);
        assert_eq!(prs(&take_batch(&mut state, 2, &BTreeSet::new())), [4, 1]);
        assert_eq!(state_of(&state, 2), QUEUED);
        assert_eq!(state_of(&state, 3), QUEUED);
    }

    #[test]
    fn a_priority_item_waiting_on_ci_still_waits_for_its_turn_to_be_due() {
        let mut state = queue_of(&[(1, QUEUED), (2, CI_PENDING)]);
        flag(&mut state, 2, true, &[]);
        assert_eq!(prs(&take_batch(&mut state, 8, &BTreeSet::new())), [1]);
        assert_eq!(state_of(&state, 2), CI_PENDING);
    }

    #[test]
    fn an_item_with_after_left_is_not_taken_and_stays_queued() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED), (3, QUEUED)]);
        flag(&mut state, 2, true, &[1]);
        assert_eq!(prs(&take_batch(&mut state, 8, &BTreeSet::new())), [1, 3]);
        assert_eq!(state_of(&state, 2), QUEUED);
    }

    #[test]
    fn a_merged_pr_is_removed_from_every_after_and_the_next_batch_takes_the_waiter() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED), (3, QUEUED)]);
        flag(&mut state, 2, false, &[1]);
        flag(&mut state, 3, false, &[1, 2]);
        assert_eq!(prs(&take_batch(&mut state, 8, &BTreeSet::new())), [1]);
        merged(&mut state, PrNumber(1)).unwrap();
        assert!(state.queue[1].after == [PrNumber(2)]);
        assert!(state.queue[0].after.is_empty());
        assert_eq!(prs(&take_batch(&mut state, 8, &BTreeSet::new())), [2]);
        merged(&mut state, PrNumber(2)).unwrap();
        assert_eq!(prs(&take_batch(&mut state, 8, &BTreeSet::new())), [3]);
    }

    #[test]
    fn a_dropped_pr_leaves_its_waiters_held() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED)]);
        flag(&mut state, 2, false, &[1]);
        drop_item(&mut state, PrNumber(1)).unwrap();
        assert!(take_batch(&mut state, 8, &BTreeSet::new()).is_empty());
        assert_eq!(state.queue[0].after, [PrNumber(1)]);
    }

    fn options(pr: u64, priority: bool, after: &[u64]) -> Submission {
        Submission {
            priority,
            after: after.iter().map(|n| PrNumber(*n)).collect(),
            ..submission(pr, "abc1234", Approval::Agent, false)
        }
    }

    #[test]
    fn a_new_submission_keeps_its_priority_and_after_and_drops_duplicates() {
        let mut state = queue_of(&[(1, QUEUED)]);
        let done = submit(&mut state, options(2, true, &[1, 1])).unwrap();
        assert_eq!(done.change, Change::Added);
        let item = &state.queue[1];
        assert!(item.priority);
        assert_eq!(item.after, [PrNumber(1)]);
    }

    #[test]
    fn submitting_the_same_head_again_changes_priority_and_after_and_says_so() {
        let mut state = queue_of(&[(1, QUEUED), (3, QUEUED)]);
        submit(&mut state, options(2, false, &[])).unwrap();

        let done = submit(&mut state, options(2, true, &[1])).unwrap();
        assert_eq!(done.change, Change::OptionsUpdated);
        assert!(state.queue[2].priority);
        assert_eq!(state.queue[2].after, [PrNumber(1)]);

        let same = submit(&mut state, options(2, true, &[1])).unwrap();
        assert_eq!(same.change, Change::Unchanged);

        let swapped = submit(&mut state, options(2, true, &[3])).unwrap();
        assert_eq!(swapped.change, Change::OptionsUpdated);
        assert_eq!(state.queue[2].after, [PrNumber(3)]);

        let cleared = submit(&mut state, options(2, false, &[])).unwrap();
        assert_eq!(cleared.change, Change::OptionsUpdated);
        assert!(!state.queue[2].priority);
        assert!(state.queue[2].after.is_empty());
    }

    #[test]
    fn an_after_pr_that_left_the_queue_meanwhile_refuses_the_submit() {
        let mut state = queue_of(&[(1, QUEUED)]);
        assert_eq!(
            submit(&mut state, options(2, false, &[9])).unwrap_err(),
            TransitionError::AfterGone {
                pr: PrNumber(2),
                after: PrNumber(9)
            }
        );
        assert_eq!(state.queue.len(), 1);
    }

    #[test]
    fn the_same_after_set_in_another_order_is_unchanged_and_position_follows_batch_order() {
        let mut state = queue_of(&[(1, QUEUED), (3, QUEUED)]);
        submit(&mut state, options(2, false, &[1, 3])).unwrap();
        let same = submit(&mut state, options(2, false, &[3, 1])).unwrap();
        assert_eq!(same.change, Change::Unchanged);

        let urgent = submit(&mut state, options(4, true, &[])).unwrap();
        assert_eq!(urgent.position, place(0));
    }

    #[test]
    fn a_pr_cannot_wait_for_itself_or_for_a_pr_that_waits_for_it() {
        let mut state = queue_of(&[(1, QUEUED)]);
        assert_eq!(
            submit(&mut state, options(1, false, &[1])).unwrap_err(),
            TransitionError::AfterSelf(PrNumber(1))
        );
        submit(&mut state, options(2, false, &[1])).unwrap();
        submit(&mut state, options(3, false, &[2])).unwrap();
        assert_eq!(
            submit(&mut state, options(1, false, &[3])).unwrap_err(),
            TransitionError::AfterCycle {
                pr: PrNumber(1),
                after: PrNumber(3)
            }
        );
        assert!(state.queue[0].after.is_empty());
    }

    #[test]
    fn a_refused_resubmit_leaves_the_options_as_they_were() {
        let mut state = queue_of(&[(1, QUEUED), (2, IN_BATCH)]);
        let err = submit(&mut state, options(2, true, &[1])).unwrap_err();
        assert!(matches!(err, TransitionError::NotAllowed { .. }));
        assert!(!state.queue[1].priority);
        assert!(state.queue[1].after.is_empty());
    }

    #[test]
    fn take_batch_takes_queued_prs_and_leaves_every_other_state_alone() {
        let mut state = queue_of(&[
            (1, AWAITING),
            (2, IN_BATCH),
            (3, ItemState::Bounced(BounceReason::Verify)),
            (4, QUEUED),
        ]);
        let before = state.clone();
        assert_eq!(prs(&take_all(&mut state)), [4]);
        assert_eq!(state_of(&state, 4), IN_BATCH);
        for pr in 1..=3 {
            assert_eq!(state_of(&state, pr), state_of(&before, pr), "#{pr}");
        }
        assert!(take_all(&mut state).is_empty());
    }

    #[test]
    fn take_batch_takes_only_the_due_ci_waiters_first_without_a_cap() {
        let due = |prs: &[u64]| prs.iter().copied().map(PrNumber).collect::<BTreeSet<_>>();
        let mut state = queue_of(&[(1, CI_PENDING), (2, QUEUED), (3, CI_PENDING), (4, QUEUED)]);
        assert_eq!(prs(&take_batch(&mut state, 1, &due(&[1, 3]))), [1, 3, 2]);
        assert_eq!(state_of(&state, 4), QUEUED);
        let mut state = queue_of(&[(1, CI_PENDING), (2, CI_PENDING), (3, QUEUED)]);
        assert_eq!(prs(&take_batch(&mut state, 8, &due(&[2]))), [2, 3]);
        assert_eq!(state_of(&state, 1), CI_PENDING);
    }

    #[test]
    fn a_ci_wait_can_be_set_cleared_and_set_again_without_error() {
        let mut state = queue_of(&[(1, QUEUED)]);
        set_ci_pending(&mut state, PrNumber(1)).unwrap();
        set_ci_pending(&mut state, PrNumber(1)).unwrap();
        assert_eq!(state_of(&state, 1), CI_PENDING);
        clear_ci_pending(&mut state, PrNumber(1)).unwrap();
        clear_ci_pending(&mut state, PrNumber(1)).unwrap();
        assert_eq!(state_of(&state, 1), QUEUED);
        assert_eq!(prs(&take_all(&mut state)), [1]);
        assert!(set_ci_pending(&mut state, PrNumber(1)).is_err());
    }

    fn failure(kind: FailureKind) -> FailureRecord {
        let step = || StepFailure {
            step: "test".into(),
            mode: VerifyMode::Solo,
            failed_tests: vec![],
            log: "/log".into(),
            tree: sha("abc1234"),
        };
        let failure = match kind {
            FailureKind::Conflict => Failure::Conflict {
                files: NonEmpty::new(vec![ConflictFile {
                    path: "src/a.rs".into(),
                    main_commits: vec![CommitRef {
                        sha: sha("def5678"),
                        title: "t".into(),
                    }],
                }])
                .unwrap(),
            },
            FailureKind::Verify => Failure::Verify(step()),
            FailureKind::Timeout => Failure::Timeout(step()),
            FailureKind::Ci => Failure::Ci {
                checks: NonEmpty::new(vec![FailedCheck {
                    name: "build".into(),
                    link: "https://example.invalid/1".into(),
                }])
                .unwrap(),
            },
        };
        FailureRecord {
            failure,
            summary: format!("{kind:?}"),
            report: "/report.md".into(),
            at_ms: EpochMs(1),
        }
    }

    fn fail_once(state: &mut State, pr: u64, kind: FailureKind, budget: u32) -> Recorded {
        assert!(prs(&take_all(state)).contains(&pr));
        record_failure(state, PrNumber(pr), failure(kind), budget).unwrap()
    }

    fn resubmit_same_head(state: &mut State, pr: u64) {
        submit(state, submission(pr, "abc1234", Approval::Agent, false)).unwrap();
    }

    fn attempts(state: &State, pr: u64) -> u32 {
        state
            .queue
            .iter()
            .find(|i| i.pr == PrNumber(pr))
            .unwrap()
            .attempts
    }

    #[test]
    fn the_failure_after_budget_bounces_takes_the_item_out_of_the_queue() {
        let budget = 3;
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED)]);
        for round in 1..=budget {
            let to = fail_once(&mut state, 1, FailureKind::Verify, budget);
            assert_eq!(to, Recorded::Bounced(BounceReason::Verify), "{round}번째");
            assert_eq!(attempts(&state, 1), round);
            resubmit_same_head(&mut state, 1);
        }
        let Recorded::OverBudget(gone) = fail_once(&mut state, 1, FailureKind::Verify, budget)
        else {
            panic!("한도를 넘긴 실패는 항목을 뺀다");
        };
        assert_eq!((gone.pr, gone.attempts), (PrNumber(1), budget));
        assert_eq!(gone.last_failure.as_ref().unwrap().summary, "Verify");
        assert!(state.queue.iter().all(|i| i.pr != PrNumber(1)));
        assert_eq!(state.queue.len(), 1);
    }

    #[test]
    fn a_pr_taken_out_for_budget_comes_back_as_a_new_item_with_no_bounces_counted() {
        let mut state = queue_of(&[(1, QUEUED)]);
        for _ in 0..3 {
            fail_once(&mut state, 1, FailureKind::Verify, 3);
            resubmit_same_head(&mut state, 1);
        }
        assert!(matches!(
            fail_once(&mut state, 1, FailureKind::Verify, 3),
            Recorded::OverBudget(_)
        ));

        let done = submit(&mut state, submission(1, "abc1234", Approval::Agent, false)).unwrap();

        assert_eq!((done.state, done.change), (QUEUED, Change::Added));
        assert_eq!(attempts(&state, 1), 0);
        assert!(state.queue[0].last_failure.is_none());
        let to = fail_once(&mut state, 1, FailureKind::Verify, 3);
        assert_eq!(to, Recorded::Bounced(BounceReason::Verify));
        assert_eq!(attempts(&state, 1), 1);
    }

    #[test]
    fn every_failure_kind_but_head_changed_uses_up_one_bounce() {
        for (kind, reason) in [
            (FailureKind::Conflict, BounceReason::Conflict),
            (FailureKind::Verify, BounceReason::Verify),
            (FailureKind::Timeout, BounceReason::Timeout),
            (FailureKind::Ci, BounceReason::Ci),
        ] {
            let mut state = queue_of(&[(1, QUEUED)]);
            let to = fail_once(&mut state, 1, kind, 3);
            assert_eq!(to, Recorded::Bounced(reason));
            assert_eq!(attempts(&state, 1), 1, "{kind:?}");
            assert_eq!(
                state.queue[0].last_failure.as_ref().unwrap().summary,
                format!("{kind:?}")
            );
        }

        let mut state = queue_of(&[(1, QUEUED)]);
        take_all(&mut state);
        head_changed(&mut state, PrNumber(1)).unwrap();
        assert_eq!(
            state_of(&state, 1),
            ItemState::Bounced(BounceReason::HeadChanged)
        );
        assert_eq!(attempts(&state, 1), 0);
        assert!(state.queue[0].last_failure.is_none());
    }

    #[test]
    fn head_changed_does_not_use_up_budget_even_when_it_is_gone() {
        let mut state = queue_of(&[(1, QUEUED)]);
        state.queue[0].attempts = 3;
        take_all(&mut state);
        head_changed(&mut state, PrNumber(1)).unwrap();
        assert_eq!(
            state_of(&state, 1),
            ItemState::Bounced(BounceReason::HeadChanged)
        );
        assert_eq!(attempts(&state, 1), 3);
    }

    #[test]
    fn a_budget_of_zero_takes_the_item_out_on_the_first_failure() {
        let mut state = queue_of(&[(1, QUEUED)]);
        let Recorded::OverBudget(gone) = fail_once(&mut state, 1, FailureKind::Conflict, 0) else {
            panic!("예산 0이면 첫 실패에서 뺀다");
        };
        assert_eq!(gone.attempts, 0);
        assert!(state.queue.is_empty());
    }

    #[test]
    fn a_refused_transition_changes_nothing() {
        let mut state = queue_of(&[(1, QUEUED), (2, AWAITING)]);
        let before = state.clone();
        assert!(record_failure(&mut state, PrNumber(1), failure(FailureKind::Verify), 3).is_err());
        assert!(merged(&mut state, PrNumber(1)).is_err());
        assert!(approve(&mut state, PrNumber(1), None).is_err());
        assert!(give_up(&mut state, PrNumber(3)).is_err());
        assert!(requeue(&mut state, PrNumber(2)).is_err());
        assert_eq!(state, before);
    }

    #[test]
    fn merge_leaves_no_count_behind() {
        let mut state = queue_of(&[(1, IN_BATCH)]);
        state.queue[0].attempts = 2;

        let done = merged(&mut state, PrNumber(1)).unwrap();
        assert_eq!((done.pr, done.attempts), (PrNumber(1), 2));
        assert!(state.queue.iter().all(|i| i.pr != PrNumber(1)));
        submit(&mut state, submission(1, "abc1234", Approval::Agent, false)).unwrap();
        assert_eq!(attempts(&state, 1), 0);
    }

    #[test]
    fn give_up_takes_a_waiting_bounced_or_awaiting_pr_out_and_refuses_one_in_a_batch() {
        let mut state = queue_of(&[
            (1, AWAITING),
            (2, QUEUED),
            (3, CI_PENDING),
            (4, ItemState::Bounced(BounceReason::Conflict)),
            (5, IN_BATCH),
        ]);
        state.queue[3].attempts = 2;

        for pr in 1..=4 {
            assert_eq!(give_up(&mut state, PrNumber(pr)).unwrap().pr, PrNumber(pr));
        }
        let err = give_up(&mut state, PrNumber(5)).unwrap_err();
        assert!(err.to_string().contains("batch가 끝난 뒤"), "{err}");
        assert_eq!(
            give_up(&mut state, PrNumber(1)).unwrap_err(),
            TransitionError::NotQueued(PrNumber(1))
        );
        assert_eq!(state.queue.len(), 1);

        let done = submit(&mut state, submission(4, "abc1234", Approval::Agent, false)).unwrap();
        assert_eq!((done.change, attempts(&state, 4)), (Change::Added, 0));
    }

    #[test]
    fn requeue_follows_the_table() {
        let mut state = queue_of(&[(1, IN_BATCH), (2, QUEUED)]);
        requeue(&mut state, PrNumber(1)).unwrap();
        assert_eq!(state_of(&state, 1), QUEUED);
        assert!(requeue(&mut state, PrNumber(2)).is_err());
    }

    #[test]
    fn drop_returns_the_item_and_refuses_a_pr_in_a_batch() {
        let mut state = queue_of(&[(1, QUEUED), (2, IN_BATCH)]);
        let gone = drop_item(&mut state, PrNumber(1)).unwrap();
        assert_eq!(gone.pr, PrNumber(1));
        let err = drop_item(&mut state, PrNumber(2)).unwrap_err();
        assert!(err.to_string().contains("batch가 끝난 뒤"), "{err}");
        assert_eq!(
            drop_item(&mut state, PrNumber(9)).unwrap_err(),
            TransitionError::NotQueued(PrNumber(9))
        );
    }

    #[test]
    fn refusals_say_why_in_korean() {
        let mut state = queue_of(&[
            (1, QUEUED),
            (2, ItemState::Bounced(BounceReason::Verify)),
            (3, IN_BATCH),
        ]);
        let said = |e: TransitionError| e.to_string();
        assert!(
            said(approve(&mut state, PrNumber(1), None).unwrap_err()).contains("승인을 기다리는")
        );
        let again = submission(3, "abc1234", Approval::Agent, false);
        assert!(said(submit(&mut state, again).unwrap_err()).contains("batch가 끝난 뒤"));
        assert!(said(give_up(&mut state, PrNumber(3)).unwrap_err()).contains("batch가 끝난 뒤"));
        assert_eq!(
            said(approve(&mut state, PrNumber(9), None).unwrap_err()),
            "#9: 큐에 없다"
        );
    }

    fn agent(terminal: &str) -> Agent {
        Agent {
            terminal: crate::model::TerminalId(terminal.into()),
            created_ms: EpochMs(1),
            name: Some(terminal.into()),
            workdir: "/w".into(),
            alive: true,
            machine: None,
        }
    }

    fn submission(pr: u64, head: &str, approval: Approval, touches_config: bool) -> Submission {
        Submission {
            pr: PrNumber(pr),
            title: format!("PR {pr} v2"),
            branch: format!("feat/{pr}"),
            head: sha(head),
            agent: None,
            submitter: None,
            touches_config,
            config_change: None,
            approval,
            priority: false,
            after: Vec::new(),
            repair: false,
            now: EpochMs(1000 + pr),
        }
    }

    #[test]
    fn a_resubmit_takes_the_new_submitter_for_any_state_and_keeps_the_old_one_when_none_is_given() {
        let mut state = State::default();
        let mut first = submission(1, "abc1234", Approval::Agent, false);
        first.submitter = Some(agent("t1orch"));
        submit(&mut state, first).unwrap();
        assert_eq!(
            state.queue[0].submitter.as_ref().unwrap().terminal.0,
            "t1orch"
        );

        let mut same_head = submission(1, "abc1234", Approval::Agent, false);
        same_head.submitter = Some(agent("t1other"));
        let done = submit(&mut state, same_head).unwrap();
        assert_eq!(done.change, Change::Unchanged);
        assert_eq!(
            state.queue[0].submitter.as_ref().unwrap().terminal.0,
            "t1other"
        );

        submit(&mut state, submission(1, "def5678", Approval::Agent, false)).unwrap();
        assert_eq!(
            state.queue[0].submitter.as_ref().unwrap().terminal.0,
            "t1other"
        );
    }

    fn with_config(
        mut s: Submission,
        kind: crate::model::ConfigChangeKind,
        touches_config: bool,
    ) -> Submission {
        s.touches_config = touches_config;
        s.config_change = Some(ConfigChange {
            kind,
            changes: Vec::new(),
            more: 0,
        });
        s
    }

    #[test]
    fn a_new_pr_starts_waiting_or_awaiting_approval() {
        use crate::model::ConfigChangeKind as Kind;
        let none = None;
        let kinds = [
            none,
            Some(Kind::Edited),
            Some(Kind::Added),
            Some(Kind::Unchanged),
            Some(Kind::Unreadable),
            Some(Kind::Invalid),
            Some(Kind::Removed),
        ];
        for approval in [Approval::Agent, Approval::Human] {
            for kind in kinds {
                let stops_queue = matches!(kind, Some(Kind::Invalid | Kind::Removed));
                let expected = if approval == Approval::Human || stops_queue {
                    AWAITING
                } else {
                    QUEUED
                };
                let touches_config = kind.is_some();
                let mut state = queue_of(&[(1, QUEUED)]);
                let mut s = submission(2, "def5678", approval, touches_config);
                if let Some(kind) = kind {
                    s = with_config(s, kind, true);
                }
                s.agent = Some(agent("t1rs"));
                let done = submit(&mut state, s).unwrap();
                assert_eq!(
                    done,
                    Submitted {
                        state: expected,
                        position: place(1),
                        change: Change::Added
                    },
                    "{approval:?} {kind:?}"
                );
                let added = &state.queue[1];
                assert_eq!(added.head, sha("def5678"));
                assert_eq!(added.agent, Some(agent("t1rs")));
                assert_eq!((added.attempts, added.submitted_ms), (0, EpochMs(1002)));
                assert_eq!(added.touches_config, touches_config);
                assert!(added.last_failure.is_none());
            }
        }
    }

    #[test]
    fn a_config_touch_without_a_summary_does_not_need_approval() {
        let mut state = queue_of(&[]);
        let done = submit(&mut state, submission(1, "abc1234", Approval::Agent, true)).unwrap();
        assert_eq!(done.state, QUEUED);
        assert!(state.queue[0].touches_config);
    }

    #[test]
    fn submitting_the_same_head_again_changes_nothing() {
        for state_before in [QUEUED, CI_PENDING, AWAITING] {
            let mut state = queue_of(&[(1, state_before)]);
            state.queue[0].agent = Some(agent("t1rs"));
            let before = state.clone();
            let mut s = submission(1, "abc1234", Approval::Human, true);
            s.agent = Some(agent("t1other"));
            let done = submit(&mut state, s).unwrap();
            assert_eq!(done.change, Change::Unchanged);
            assert_eq!((done.state, done.position), (state_before, place(0)));
            assert_eq!(state, before);
        }
    }

    #[test]
    fn the_same_head_again_moves_an_old_awaiting_item_to_the_queue_only_without_approval() {
        let mut state = queue_of(&[(1, AWAITING), (2, AWAITING)]);
        state.queue[0].touches_config = true;
        let released = submit(&mut state, submission(1, "abc1234", Approval::Agent, true)).unwrap();
        assert_eq!(
            released,
            Submitted {
                state: QUEUED,
                position: place(0),
                change: Change::Requeued
            }
        );
        assert_eq!(state_of(&state, 1), QUEUED);

        let stays = submit(&mut state, submission(2, "abc1234", Approval::Human, false)).unwrap();
        assert_eq!((stays.state, stays.change), (AWAITING, Change::Unchanged));
        assert_eq!(state_of(&state, 2), AWAITING);

        let removed = with_config(
            submission(2, "abc1234", Approval::Agent, true),
            crate::model::ConfigChangeKind::Removed,
            true,
        );
        let held = submit(&mut state, removed).unwrap();
        assert_eq!((held.state, held.change), (AWAITING, Change::Unchanged));
    }

    #[test]
    fn a_new_head_asks_for_approval_again_only_when_approval_is_needed() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED), (3, QUEUED), (4, QUEUED)]);
        let human = submit(&mut state, submission(1, "def5678", Approval::Human, false)).unwrap();
        assert_eq!((human.state, human.change), (AWAITING, Change::HeadUpdated));
        let trust = submit(&mut state, submission(2, "def5678", Approval::Agent, false)).unwrap();
        assert_eq!((trust.state, trust.change), (QUEUED, Change::HeadUpdated));
        let edited = with_config(
            submission(3, "def5678", Approval::Agent, true),
            crate::model::ConfigChangeKind::Edited,
            true,
        );
        let config = submit(&mut state, edited).unwrap();
        assert_eq!((config.state, config.change), (QUEUED, Change::HeadUpdated));
        assert!(state.queue[2].touches_config);
        let invalid = with_config(
            submission(4, "def5678", Approval::Agent, true),
            crate::model::ConfigChangeKind::Invalid,
            true,
        );
        assert_eq!(submit(&mut state, invalid).unwrap().state, AWAITING);
        assert_eq!(state.queue[0].head, sha("def5678"));
        assert_eq!(state.queue[0].title, "PR 1 v2");

        approve(&mut state, PrNumber(1), None).unwrap();
        let same = submit(&mut state, submission(1, "def5678", Approval::Human, false)).unwrap();
        assert_eq!((same.state, same.change), (QUEUED, Change::Unchanged));
    }

    fn summary(kind: crate::model::ConfigChangeKind) -> Option<ConfigChange> {
        Some(ConfigChange {
            kind,
            changes: Vec::new(),
            more: 0,
        })
    }

    #[test]
    fn a_new_head_replaces_the_config_summary_and_the_same_head_keeps_it() {
        use crate::model::ConfigChangeKind::{Added, Edited, Unreadable};
        let mut state = queue_of(&[]);
        let mut first = submission(1, "abc1234", Approval::Agent, true);
        first.config_change = summary(Edited);
        submit(&mut state, first).unwrap();
        assert_eq!(state.queue[0].config_change, summary(Edited));

        let mut same = submission(1, "abc1234", Approval::Agent, true);
        same.config_change = summary(Unreadable);
        submit(&mut state, same).unwrap();
        assert_eq!(state.queue[0].config_change, summary(Edited));

        let mut newer = submission(1, "def5678", Approval::Agent, true);
        newer.config_change = summary(Added);
        submit(&mut state, newer).unwrap();
        assert_eq!(state.queue[0].config_change, summary(Added));

        submit(&mut state, submission(1, "0123abc", Approval::Agent, false)).unwrap();
        assert_eq!(state.queue[0].config_change, None);
    }

    #[test]
    fn a_new_head_clears_a_ci_wait() {
        let mut state = queue_of(&[(1, CI_PENDING)]);
        submit(&mut state, submission(1, "def5678", Approval::Agent, false)).unwrap();
        assert_eq!(state_of(&state, 1), QUEUED);
    }

    #[test]
    fn a_bounced_pr_goes_back_to_waiting_keeping_its_count_and_place() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED)]);
        fail_once(&mut state, 1, FailureKind::Verify, 3);
        assert_eq!(attempts(&state, 1), 1);
        state.queue[0].agent = Some(agent("t1old"));

        let mut s = submission(1, "def5678", Approval::Agent, false);
        s.agent = Some(agent("t1new"));
        let done = submit(&mut state, s).unwrap();
        assert_eq!(
            done,
            Submitted {
                state: QUEUED,
                position: place(0),
                change: Change::Requeued
            }
        );
        assert_eq!(attempts(&state, 1), 1);
        assert_eq!(state.queue[0].agent, Some(agent("t1new")));
        assert_eq!(state.queue[0].submitted_ms, EpochMs(1));
        assert_eq!(state.queue[0].head, sha("def5678"));
        assert!(state.queue[0].last_failure.is_some());
    }

    #[test]
    fn a_bounced_pr_without_a_new_agent_keeps_the_old_one() {
        let mut state = queue_of(&[(1, ItemState::Bounced(BounceReason::HeadChanged))]);
        state.queue[0].agent = Some(agent("t1old"));
        submit(&mut state, submission(1, "def5678", Approval::Agent, false)).unwrap();
        assert_eq!(state.queue[0].agent, Some(agent("t1old")));
    }

    #[test]
    fn a_bounced_pr_needs_approval_again_only_for_a_new_head() {
        let bounced = ItemState::Bounced(BounceReason::Verify);
        let mut state = queue_of(&[(1, bounced), (2, bounced)]);
        let same = submit(&mut state, submission(1, "abc1234", Approval::Human, false)).unwrap();
        assert_eq!(same.state, QUEUED);
        let new = submit(&mut state, submission(2, "def5678", Approval::Human, false)).unwrap();
        assert_eq!(new.state, AWAITING);
    }

    #[test]
    fn submit_is_refused_while_in_a_batch() {
        let mut state = queue_of(&[(1, IN_BATCH)]);
        let before = state.clone();
        let err = submit(&mut state, submission(1, "def5678", Approval::Agent, false)).unwrap_err();
        assert!(matches!(err, TransitionError::NotAllowed { .. }));
        assert_eq!(state, before);
    }

    fn preflight(head: &str) -> PreflightRecord {
        PreflightRecord {
            head: sha(head),
            result: crate::model::PreflightResult::Pass,
            report: None,
            at_ms: EpochMs(1),
            base: None,
            with: Vec::new(),
            env: None,
        }
    }

    #[test]
    fn registering_a_head_drops_preflight_results_of_other_heads_of_that_branch() {
        let mut state = queue_of(&[(1, QUEUED)]);
        for branch in ["feat/1", "feat/2", "feat/3", "feat/other"] {
            state.preflights.insert(branch.into(), preflight("0d0d0d0"));
        }

        submit(&mut state, submission(1, "def5678", Approval::Agent, false)).unwrap();
        submit(&mut state, submission(2, "def5678", Approval::Agent, false)).unwrap();
        state
            .preflights
            .insert("feat/3".into(), preflight("def5678"));
        submit(&mut state, submission(3, "def5678", Approval::Agent, false)).unwrap();

        assert!(!state.preflights.contains_key("feat/1"));
        assert!(!state.preflights.contains_key("feat/2"));
        assert_eq!(state.preflights["feat/3"], preflight("def5678"));
        assert_eq!(state.preflights["feat/other"], preflight("0d0d0d0"));
    }

    #[test]
    fn resubmitting_the_same_head_keeps_every_preflight_result() {
        let mut state = queue_of(&[(1, QUEUED)]);
        state
            .preflights
            .insert("feat/1".into(), preflight("def5678"));
        let before = state.clone();
        submit(&mut state, submission(1, "abc1234", Approval::Agent, false)).unwrap();
        assert_eq!(state, before);
    }

    #[test]
    fn approval_applies_only_to_the_head_the_approver_saw() {
        let waiting = || queue_of(&[(1, AWAITING)]);
        let mut state = waiting();
        let before = state.clone();

        for seen in ["def5678", "abd1"] {
            let err = approve(&mut state, PrNumber(1), Some(seen)).unwrap_err();
            assert!(matches!(err, TransitionError::HeadMoved { .. }), "{seen:?}");
            assert_eq!(state, before, "{seen:?}");
        }
        let err = approve(&mut state, PrNumber(1), Some("def5678")).unwrap_err();
        assert!(
            err.to_string()
                .contains("승인하려던 HEAD def5678, 지금 HEAD abc1234"),
            "{err}"
        );

        let too_long = ["abc12340", &"a".repeat(65), &"a".repeat(100_000)];
        for given in ["", "a", "abc", "abd", "zzzz", "abc 1234", "-abc1"]
            .into_iter()
            .chain(too_long)
        {
            let err = approve(&mut state, PrNumber(1), Some(given)).unwrap_err();
            assert!(matches!(err, TransitionError::BadHead { .. }), "{given:?}");
            assert!(err.to_string().contains("4자 이상"), "{err}");
            assert!(err.to_string().chars().count() < 200, "{err}");
            assert_eq!(state, before, "{given:?}");
        }

        for seen in [
            Some("abc1234"),
            Some("abc123"),
            Some("abc1"),
            Some("ABC1234"),
            Some(" abc1234 "),
            None,
        ] {
            let mut state = waiting();
            approve(&mut state, PrNumber(1), seen).unwrap();
            assert_eq!(state_of(&state, 1), QUEUED, "{seen:?}");
        }
    }

    #[test]
    fn a_pr_that_is_not_waiting_is_refused_for_its_state_before_its_head() {
        let mut state = queue_of(&[(1, QUEUED)]);
        let err = approve(&mut state, PrNumber(1), Some("def5678")).unwrap_err();
        assert!(matches!(err, TransitionError::NotAllowed { .. }), "{err}");
    }

    #[test]
    fn labels_match_the_plain_text_status() {
        assert_eq!(label(AWAITING), "승인 대기");
        assert_eq!(label(CI_PENDING), "대기(CI 기다림)");
        assert_eq!(
            label(ItemState::Bounced(BounceReason::HeadChanged)),
            "반송(HEAD 바뀜)"
        );
    }

    fn halt(state: &mut State) {
        state.blocked = Some(crate::model::Blocked {
            reason: crate::model::BlockReason::BaseBroken,
            base: sha("abc1234"),
            since_ms: EpochMs(1),
            step: None,
            log: None,
            pushed_by_chute: false,
            rechecked: false,
        });
    }

    fn repair_options(pr: u64) -> Submission {
        Submission {
            repair: true,
            ..submission(pr, "abc1234", Approval::Agent, false)
        }
    }

    fn mark_repair(state: &mut State, pr: u64) {
        state
            .queue
            .iter_mut()
            .find(|i| i.pr == PrNumber(pr))
            .unwrap()
            .repair = true;
    }

    #[test]
    fn a_repair_submission_is_refused_while_the_queue_is_not_halted() {
        let mut state = State::default();

        let err = submit(&mut state, repair_options(1)).unwrap_err();

        assert_eq!(err, TransitionError::RepairNotHalted(PrNumber(1)));
        assert!(err.to_string().contains("멈춰 있지 않아"), "{err}");
        assert!(err.to_string().contains("`--repair` 없이"), "{err}");
        assert!(state.queue.is_empty());
    }

    #[test]
    fn a_repair_submission_is_taken_while_halted_and_marks_the_item() {
        let mut state = queue_of(&[(1, QUEUED)]);
        halt(&mut state);

        let done = submit(&mut state, repair_options(2)).unwrap();

        assert_eq!(done.change, Change::Added);
        assert!(state.queue[1].repair);
        assert!(!state.queue[0].repair);
    }

    #[test]
    fn a_second_repair_pr_is_refused_until_the_first_is_gone_and_the_same_pr_may_come_again() {
        let mut state = State::default();
        halt(&mut state);
        submit(&mut state, repair_options(2)).unwrap();

        let err = submit(&mut state, repair_options(3)).unwrap_err();

        assert_eq!(
            err,
            TransitionError::RepairTaken {
                pr: PrNumber(3),
                held: PrNumber(2),
                state: QUEUED
            }
        );
        assert!(err.to_string().contains("`chute drop 2`"), "{err}");
        assert!(err.to_string().contains("대기"), "{err}");
        assert_eq!(state.queue.len(), 1);
        let mut again = repair_options(2);
        again.head = sha("def5678");
        assert_eq!(
            submit(&mut state, again).unwrap().change,
            Change::HeadUpdated
        );
        drop_item(&mut state, PrNumber(2)).unwrap();
        submit(&mut state, repair_options(3)).unwrap();
    }

    #[test]
    fn a_repair_pr_waits_for_approval_only_when_the_config_stops_the_queue() {
        let mut state = State::default();
        halt(&mut state);
        let invalid = with_config(
            repair_options(2),
            crate::model::ConfigChangeKind::Invalid,
            true,
        );

        let done = submit(&mut state, invalid).unwrap();

        assert_eq!(done.state, ItemState::AwaitingApproval);
        assert!(state.queue[0].repair);
        assert!(!repair_ready(&state));

        let mut state = State::default();
        halt(&mut state);
        let edited = with_config(
            repair_options(2),
            crate::model::ConfigChangeKind::Edited,
            true,
        );
        assert_eq!(submit(&mut state, edited).unwrap().state, QUEUED);
        assert!(repair_ready(&state));
    }

    #[test]
    fn an_existing_item_resubmitted_as_repair_becomes_one_and_keeps_the_mark_afterwards() {
        let mut state = queue_of(&[(1, QUEUED)]);
        halt(&mut state);
        let marked = submit(&mut state, repair_options(1)).unwrap();
        assert_eq!(marked.change, Change::OptionsUpdated);
        assert!(state.queue[0].repair);

        let plain = submit(&mut state, submission(1, "abc1234", Approval::Agent, false)).unwrap();

        assert_eq!(plain.change, Change::Unchanged);
        assert!(state.queue[0].repair, "표시는 기록으로 남는다");
    }

    #[test]
    fn batch_order_puts_repair_before_priority_before_the_rest() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED), (3, QUEUED), (4, QUEUED)]);
        flag(&mut state, 2, true, &[]);
        mark_repair(&mut state, 4);

        assert_eq!(
            batch_order(&state.queue),
            [PrNumber(4), PrNumber(2), PrNumber(1), PrNumber(3)]
        );
        let taken = take_batch(&mut state, 2, &BTreeSet::new());
        assert_eq!(taken, [PrNumber(4), PrNumber(2)]);
    }

    #[test]
    fn take_batch_except_leaves_the_skipped_prs_queued_and_still_fills_the_batch() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED), (3, QUEUED), (4, QUEUED)]);
        let skip: BTreeSet<PrNumber> = [PrNumber(1), PrNumber(3)].into();

        let taken = take_batch_except(&mut state, 2, &BTreeSet::new(), &skip);

        assert_eq!(prs(&taken), [2, 4]);
        assert_eq!(state_of(&state, 1), QUEUED);
        assert_eq!(state_of(&state, 3), QUEUED);
    }

    #[test]
    fn a_repair_submission_is_taken_while_a_post_verify_failure_holds_prs() {
        let mut state = State::default();
        let err = submit(&mut state, repair_options(1)).unwrap_err();
        assert!(matches!(err, TransitionError::RepairNotHalted(_)));

        state.post.red = Some(crate::model::PostRed {
            commit: sha("abc1234"),
            step: "e2e".into(),
            tests: Vec::new(),
            log: "/log".into(),
            report: None,
            since_ms: EpochMs(1),
            last_green: None,
            culprit: None,
            holding: false,
        });
        assert!(
            submit(&mut state, repair_options(1)).is_err(),
            "잡아 두지 않으면 받지 않는다"
        );

        state.post.red.as_mut().unwrap().holding = true;
        submit(&mut state, repair_options(1)).unwrap();
        assert!(state.queue[0].repair);
    }

    #[test]
    fn take_repair_takes_only_the_repair_item_and_only_when_it_may_run() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED), (3, CI_PENDING)]);
        assert!(!repair_ready(&state));
        assert!(take_repair(&mut state, &BTreeSet::new()).is_empty());
        mark_repair(&mut state, 3);
        assert!(repair_ready(&state));

        assert!(take_repair(&mut state, &BTreeSet::new()).is_empty());
        assert_eq!(state_of(&state, 3), CI_PENDING);
        let due: BTreeSet<PrNumber> = [PrNumber(3)].into();
        assert_eq!(take_repair(&mut state, &due), [PrNumber(3)]);
        assert_eq!(state_of(&state, 3), IN_BATCH);
        assert_eq!(state_of(&state, 1), QUEUED);
        assert_eq!(state_of(&state, 2), QUEUED);
    }

    #[test]
    fn take_repair_leaves_a_repair_item_that_waits_behind_another_pr() {
        let mut state = queue_of(&[(1, QUEUED), (2, QUEUED)]);
        mark_repair(&mut state, 2);
        flag(&mut state, 2, false, &[1]);

        assert!(take_repair(&mut state, &BTreeSet::new()).is_empty());
    }
}
