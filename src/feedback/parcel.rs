use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::{Feedback, Notice, Unreachable, Written, plain_ref_name, short, strip_terminal_noise};
use crate::gitops::FAILED_REF_PREFIX;
use crate::hub::{base64_decode, base64_encode};
use crate::model::{Failure, Partner, QueueItem, Sha, VerifyMode};

/// What `hc exec -i` accepts on stdin.
pub const PARCEL_LIMIT: usize = 8 * 1024 * 1024;
const LOG_LIMIT: usize = 1024 * 1024;
const DIFF_LIMIT: usize = 1024 * 1024;
pub(super) const FRAMING: usize = 16 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub total: usize,
    pub log: usize,
    pub diffs: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            total: PARCEL_LIMIT,
            log: LOG_LIMIT,
            diffs: DIFF_LIMIT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parcel {
    pub(super) item: QueueItem,
    pub(super) notice: ParcelNotice,
    pub(super) files: Vec<ParcelFile>,
    pub(super) bundle: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParcelNotice {
    Bounced {
        failure: Failure,
        failed_ref: Option<String>,
        rebuild: Option<Rebuild>,
    },
    HeadChanged {
        registered: Sha,
        current: Sha,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rebuild {
    pub base: Sha,
    pub heads: Vec<Sha>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParcelFile {
    pub name: String,
    data: String,
}

impl ParcelFile {
    fn new(name: String, bytes: &[u8]) -> ParcelFile {
        ParcelFile {
            name,
            data: base64_encode(bytes),
        }
    }

    pub(super) fn bytes(&self) -> Result<Vec<u8>> {
        base64_decode(&self.data).ok_or_else(|| anyhow!("묶음의 파일 {}을 읽지 못했다", self.name))
    }
}

impl Parcel {
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Reads what a hub sent. Everything that later becomes a path or a git ref is checked here.
    pub fn decode(bytes: &[u8]) -> Result<Parcel> {
        let parcel: Parcel = serde_json::from_slice(bytes).context("받은 묶음을 읽지 못했다")?;
        parcel.check()?;
        Ok(parcel)
    }

    pub fn pr(&self) -> crate::model::PrNumber {
        self.item.pr
    }

    fn check(&self) -> Result<()> {
        let mut seen: Vec<&str> = Vec::new();
        for file in &self.files {
            if !plain_file_name(&file.name) || seen.contains(&file.name.as_str()) {
                bail!("묶음의 파일 이름이 올바르지 않다: {:?}", file.name);
            }
            seen.push(&file.name);
        }
        if let Some(agent) = &self.item.agent
            && !plain_terminal(&agent.terminal.0)
        {
            bail!("묶음의 터미널 주소가 올바르지 않다: {:?}", agent.terminal.0);
        }
        if let ParcelNotice::Bounced {
            failure,
            failed_ref,
            ..
        } = &self.notice
        {
            if let Some(name) = failed_ref
                && !name
                    .strip_prefix(FAILED_REF_PREFIX)
                    .is_some_and(plain_ref_name)
            {
                bail!("묶음의 실패한 트리 참조 이름이 올바르지 않다: {name:?}");
            }
            for path in linked_files(failure) {
                let named = path.to_str().is_some_and(plain_file_name);
                if !named {
                    bail!("묶음의 보고서가 폴더 밖을 가리킨다: {}", path.display());
                }
            }
        }
        Ok(())
    }
}

fn plain_terminal(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
}

fn plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with(['-', '.'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn linked_files(failure: &Failure) -> Vec<&Path> {
    match failure {
        Failure::Verify(step) | Failure::Timeout(step) => {
            let mut files = vec![step.log.as_path()];
            if let VerifyMode::Combo { partners } = &step.mode {
                files.extend(partners.iter().map(|p| p.diff.as_path()));
            }
            files
        }
        Failure::Conflict { .. } | Failure::Ci { .. } => Vec::new(),
    }
}

impl Feedback<'_> {
    pub(super) fn parcel(
        &self,
        item: &QueueItem,
        notice: &Notice<'_>,
        limits: &Limits,
    ) -> Result<Parcel> {
        let mut notice_data = match notice {
            Notice::HeadChanged {
                registered,
                current,
            } => ParcelNotice::HeadChanged {
                registered: (*registered).clone(),
                current: (*current).clone(),
            },
            Notice::Bounced(written) => self.bounced_notice(written),
        };
        let files = match &mut notice_data {
            ParcelNotice::Bounced { failure, .. } => self.attach_files(failure, limits),
            ParcelNotice::HeadChanged { .. } => Vec::new(),
        };
        let mut parcel = Parcel {
            item: item.clone(),
            notice: notice_data,
            files,
            bundle: None,
        };
        let used = parcel.encode()?.len();
        if used + FRAMING > limits.total {
            bail!("묶음이 한도({}바이트)를 넘는다", limits.total);
        }
        if let Some(written) = bounced(notice) {
            parcel.bundle = self.bundle_within(written, limits.total - used - FRAMING);
        }
        Ok(parcel)
    }

    fn bounced_notice(&self, written: &Written) -> ParcelNotice {
        let failed_ref = written.doc.step.as_ref().and_then(|s| s.git_ref.clone());
        let rebuild = match &written.doc.failure {
            Failure::Verify(step) | Failure::Timeout(step) => self.rebuild(&step.tree).ok(),
            Failure::Conflict { .. } | Failure::Ci { .. } => None,
        };
        ParcelNotice::Bounced {
            failure: written.doc.failure.clone(),
            failed_ref,
            rebuild,
        }
    }

    fn attach_files(&self, failure: &mut Failure, limits: &Limits) -> Vec<ParcelFile> {
        let (Failure::Verify(step) | Failure::Timeout(step)) = failure else {
            return Vec::new();
        };
        let tree = short(&step.tree).to_owned();
        let mut files = Vec::new();
        let name = format!("log-{tree}.log");
        let text = log_end(&step.log, limits.log);
        files.push(ParcelFile::new(name.clone(), text.as_bytes()));
        step.log = PathBuf::from(name);

        if let VerifyMode::Combo { partners } = &mut step.mode {
            let mut room = limits.diffs;
            let mut each: Vec<Partner> = partners.to_vec();
            for partner in &mut each {
                let name = format!("partner-pr{}-{tree}.diff", partner.pr.0);
                let text = diff_start(&partner.diff, room);
                room = room.saturating_sub(text.len());
                files.push(ParcelFile::new(name.clone(), text.as_bytes()));
                partner.diff = PathBuf::from(name);
            }
            if let Some(each) = crate::model::NonEmpty::new(each) {
                *partners = each;
            }
        }
        files
    }

    fn bundle_within(&self, written: &Written, room: usize) -> Option<String> {
        let git_ref = written.doc.step.as_ref()?.git_ref.as_deref()?;
        let tree = match &written.doc.failure {
            Failure::Verify(step) | Failure::Timeout(step) => &step.tree,
            Failure::Conflict { .. } | Failure::Ci { .. } => return None,
        };
        let base = self.base_of(tree).ok()?;
        let out = self
            .ext
            .git(
                &self.repo.path,
                ["bundle", "create", "-", git_ref, &format!("^{base}")],
            )
            .ok()?;
        (out.success() && out.stdout.len().div_ceil(3) * 4 <= room)
            .then(|| base64_encode(&out.stdout))
    }

    fn base_of(&self, tree: &Sha) -> Result<Sha> {
        let origin = format!("refs/remotes/origin/{}", self.repo.default_branch);
        let out = self
            .ext
            .git(&self.repo.path, ["merge-base", tree.as_str(), &origin])?
            .ok()?;
        Sha::try_from(out.trim().to_owned()).map_err(|e| anyhow!(e))
    }

    fn rebuild(&self, tree: &Sha) -> Result<Rebuild> {
        let base = self.base_of(tree)?;
        let listing = self
            .ext
            .git(
                &self.repo.path,
                [
                    "rev-list",
                    "--first-parent",
                    "--merges",
                    "--parents",
                    "--reverse",
                    &format!("{base}..{tree}"),
                ],
            )?
            .ok()?;
        let heads = listing
            .lines()
            .map(|line| {
                let second = line.split_whitespace().nth(2);
                Sha::try_from(second.unwrap_or_default().to_owned()).map_err(|e| anyhow!(e))
            })
            .collect::<Result<Vec<Sha>>>()?;
        if heads.is_empty() {
            bail!("{tree}에서 합쳐진 PR 머지를 찾지 못했다");
        }
        Ok(Rebuild { base, heads })
    }

    pub(super) fn answer_of(
        &self,
        machine: &str,
        answer: &crate::hub::Forwarded,
    ) -> Result<super::Delivery, Unreachable> {
        if answer.code != 0 {
            return Err(Unreachable::Failed(format!(
                "머신 `{machine}`의 chute가 반송을 받지 못했다(종료 코드 {}): {}",
                answer.code,
                strip_terminal_noise(&String::from_utf8_lossy(&answer.stderr)).trim()
            )));
        }
        let text = String::from_utf8_lossy(&answer.stdout);
        text.lines()
            .rev()
            .find_map(|line| serde_json::from_str(line.trim()).ok())
            .ok_or_else(|| {
                Unreachable::Failed(format!(
                    "머신 `{machine}`의 chute가 보낸 결과를 읽지 못했다. 보고서가 전달됐는지 알 수 없다. 그 머신의 에이전트를 확인한다"
                ))
            })
    }
}

fn bounced<'a>(notice: &'a Notice<'a>) -> Option<&'a Written> {
    match notice {
        Notice::Bounced(written) => Some(written),
        Notice::HeadChanged { .. } => None,
    }
}

fn log_end(path: &Path, limit: usize) -> String {
    let read = || -> io::Result<(Vec<u8>, u64)> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let start = len.saturating_sub(limit as u64);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if start > 0
            && let Some(nl) = bytes.iter().position(|b| *b == b'\n')
        {
            bytes.drain(..=nl);
        }
        Ok((bytes, len))
    };
    match read() {
        Ok((bytes, len)) => {
            let cut = len - bytes.len() as u64;
            let lossy = String::from_utf8_lossy(&bytes);
            let text = tail_of(&lossy, limit);
            let cut = cut + (lossy.len() - text.len()) as u64;
            if cut == 0 {
                text.to_owned()
            } else {
                format!("[chute: 로그가 길어 앞 {cut}바이트를 잘랐다]\n{text}")
            }
        }
        Err(e) => format!("(로그를 읽지 못했다: {e})\n"),
    }
}

fn diff_start(path: &Path, limit: usize) -> String {
    let read = || -> io::Result<(Vec<u8>, u64)> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut bytes = Vec::new();
        file.take(limit as u64).read_to_end(&mut bytes)?;
        if len > bytes.len() as u64 {
            match bytes.iter().rposition(|b| *b == b'\n') {
                Some(nl) => bytes.truncate(nl + 1),
                None => bytes.clear(),
            }
        }
        Ok((bytes, len))
    };
    match read() {
        Ok((bytes, len)) => {
            let cut = len - bytes.len() as u64;
            let lossy = String::from_utf8_lossy(&bytes);
            let text = head_of(&lossy, limit);
            let cut = cut + (lossy.len() - text.len()) as u64;
            if cut == 0 {
                text.to_owned()
            } else {
                format!("{text}[chute: diff가 길어 뒤 {cut}바이트를 잘랐다]\n")
            }
        }
        Err(e) => format!("diff를 읽지 못했다: {e}\n"),
    }
}

/// Invalid bytes grow to three when read as text, so the limit is applied again to the text.
fn tail_of(text: &str, limit: usize) -> &str {
    let mut from = text.len().saturating_sub(limit);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    &text[from..]
}

fn head_of(text: &str, limit: usize) -> &str {
    let mut to = text.len().min(limit);
    while !text.is_char_boundary(to) {
        to -= 1;
    }
    &text[..to]
}
