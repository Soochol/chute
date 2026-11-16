//! Slots borrow the user's object store and never change the user's HEAD, index, branches or
//! work folder.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};

use crate::ext::{Ext, Output};
use crate::model::{CommitRef, ConflictFile, NonEmpty, PrNumber, RepoInfo, Sha};
use crate::store::Home;

const TRACKING_ATTEMPTS: u32 = 3;

const SLOT_POLL: Duration = Duration::from_millis(200);

pub const FAILED_REF_PREFIX: &str = "refs/chute/failed/";

/// Hooks are for the user's own use of git. Run for chute, a hook can refuse the push of a
/// verified commit.
const NO_HOOKS: [&str; 2] = ["-c", "core.hooksPath=/dev/null"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SlotName {
    Batch,
    /// Numbered from 1.
    Preflight(u32),
    Revert,
}

impl SlotName {
    /// The inverse of `Display`: a slot folder's name to its slot. `None` for anything else,
    /// so `preflight-0` and `preflight-01` are not slots.
    pub fn parse(text: &str) -> Option<SlotName> {
        match text {
            "batch" => Some(SlotName::Batch),
            "revert" => Some(SlotName::Revert),
            _ => {
                let k: u32 = text.strip_prefix("preflight-")?.parse().ok()?;
                let slot = SlotName::Preflight(k);
                (k >= 1 && slot.to_string() == text).then_some(slot)
            }
        }
    }
}

impl fmt::Display for SlotName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlotName::Batch => f.write_str("batch"),
            SlotName::Preflight(k) => write!(f, "preflight-{k}"),
            SlotName::Revert => f.write_str("revert"),
        }
    }
}

/// A PR is merged by its registered head SHA, never by branch name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeItem {
    Pr {
        pr: PrNumber,
        branch: String,
        title: String,
        head: Sha,
    },
    Local {
        head: Sha,
    },
}

impl MergeItem {
    pub fn head(&self) -> &Sha {
        match self {
            MergeItem::Pr { head, .. } | MergeItem::Local { head } => head,
        }
    }

    fn github_merge_message(&self, owner: &str) -> String {
        match self {
            MergeItem::Pr {
                pr, branch, title, ..
            } => format!(
                "Merge pull request #{} from {owner}/{branch}\n\n{title}",
                pr.0
            ),
            MergeItem::Local { head } => format!("Merge commit '{head}'"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeResult {
    /// The slot's HEAD after the run: the base when nothing merged.
    pub tip: Sha,
    /// One entry per input item, in input order.
    pub items: Vec<ItemOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemOutcome {
    pub item: MergeItem,
    pub result: ItemResult,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemResult {
    Merged {
        commit: Sha,
    },
    /// The head is already part of what the slot held when its turn came.
    AlreadyMerged,
    /// The merge was undone. Paths are as git records them, so a file/directory clash is
    /// `foo~HEAD` or `foo~<PR head SHA>`. Each file lists the commits on the target side, since
    /// the merge base, that changed it, newest first: main, plus the earlier PRs of the same run.
    /// Merge commits are not listed, so a change made only inside one is missed, and a file the
    /// PR renamed has none.
    Conflict(NonEmpty<ConflictFile>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrHead {
    Unchanged,
    Changed { actual: Sha },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    Pushed,
    /// The remote branch is no longer an ancestor of the commit, so no fast-forward push is possible.
    MainMoved,
}

#[derive(Clone, Debug)]
pub struct Gitops {
    ext: Ext,
    repo: PathBuf,
    slots_dir: PathBuf,
    owner: String,
    remote: String,
    default_branch: String,
    cache_seed: Vec<String>,
}

impl Gitops {
    pub fn new(ext: &Ext, home: &Home, info: &RepoInfo) -> Gitops {
        let slots_dir = home.slots_dir(&info.slug);
        Gitops {
            ext: ext.clone(),
            repo: info.path.clone(),
            slots_dir: std::path::absolute(&slots_dir).unwrap_or(slots_dir),
            owner: info.slug.owner().to_owned(),
            remote: info.remote_name.clone(),
            default_branch: info.default_branch.clone(),
            cache_seed: Vec::new(),
        }
    }

    /// Fetches the default branch and moves its tracking ref to it without opening a slot.
    /// The tip comes from a private ref, so the caller's `FETCH_HEAD` and branches stay as they are.
    pub fn fetch_base_tip(&self) -> Result<Sha> {
        let branch = &self.default_branch;
        let tracking = format!("refs/remotes/{}/{branch}", self.remote);
        let private = format!("refs/chute/base-{}", std::process::id());
        let refspec = format!("+refs/heads/{branch}:{private}");
        let fetched = self.repo_git(&[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--refmap=",
            &self.remote,
            &refspec,
        ])?;
        let tip = if fetched.success() {
            self.repo_git(&["rev-parse", "--verify", &format!("{private}^{{commit}}")])
        } else {
            Ok(fetched)
        };
        let _ = self.repo_git(&["update-ref", "-d", &private]);
        let tip = Sha::try_from(tip?.ok()?.trim().to_owned()).map_err(|e| anyhow!(e))?;
        let before = self.repo_git(&["rev-parse", "--verify", "--quiet", &tracking])?;
        let old = if before.success() {
            before.stdout_text().trim().to_owned()
        } else {
            String::new()
        };
        self.repo_git(&["update-ref", &tracking, tip.as_str(), &old])?;
        Ok(tip)
    }

    fn repo_git(&self, args: &[&str]) -> Result<Output> {
        Ok(self.ext.git(&self.repo, NO_HOOKS.iter().chain(args))?)
    }

    /// Paths, relative to a slot, that a new or rebuilt slot copies from the slot used last.
    pub fn with_cache_seed(mut self, seed: Vec<String>) -> Gitops {
        self.cache_seed = seed;
        self
    }

    /// One folder per repository for tools that many slots may write at once. Created when a
    /// verification starts.
    pub fn cache_dir(&self) -> PathBuf {
        self.slots_dir.join("cache")
    }

    /// Whether nobody holds the slot right now. The answer can be stale when it is read.
    pub fn slot_is_free(&self, name: SlotName) -> Result<bool> {
        Ok(self.lock_slot(name)?.is_some())
    }

    /// `None` when another holder has the slot. The lock file sits beside the worktree:
    /// inside it, `git clean` would delete it.
    pub fn lock_slot(&self, name: SlotName) -> Result<Option<SlotLock>> {
        fs::create_dir_all(&self.slots_dir)
            .with_context(|| format!("만들지 못했다: {}", self.slots_dir.display()))?;
        let path = self.slots_dir.join(format!("{name}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("열지 못했다: {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(SlotLock {
                git: self.clone(),
                name,
                _file: file,
            })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => {
                Err(anyhow::Error::new(e).context(format!("잠그지 못했다: {}", path.display())))
            }
        }
    }

    /// The first of slots `1..=max_parallel` nobody holds. When all are held, waits for one and
    /// says so once on stderr.
    pub fn lock_preflight_slot(&self, max_parallel: u32) -> Result<SlotLock> {
        let mut told = false;
        loop {
            for k in 1..=max_parallel {
                if let Some(lock) = self.lock_slot(SlotName::Preflight(k))? {
                    return Ok(lock);
                }
            }
            if !told {
                eprintln!("다른 preflight {max_parallel}개가 돌고 있어 기다린다");
                told = true;
            }
            thread::sleep(SLOT_POLL);
        }
    }

    /// Locks every existing preflight slot numbered above `max_parallel` that nobody holds.
    /// The caller decides whether to `remove` them; the locks keep a preflight from taking one
    /// meanwhile. A slot someone holds is left out.
    pub fn lock_surplus_preflight_slots(&self, max_parallel: u32) -> Result<Vec<SlotLock>> {
        let entries = match fs::read_dir(&self.slots_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("읽지 못했다: {}", self.slots_dir.display()));
            }
        };
        let mut surplus = Vec::new();
        for entry in entries {
            let entry =
                entry.with_context(|| format!("읽지 못했다: {}", self.slots_dir.display()))?;
            let slot = entry.file_name().to_str().and_then(SlotName::parse);
            if let Some(slot @ SlotName::Preflight(k)) = slot
                && k > max_parallel
                && entry.file_type().is_ok_and(|t| t.is_dir())
                && let Some(lock) = self.lock_slot(slot)?
            {
                surplus.push(lock);
            }
        }
        surplus.sort_by_key(|lock| lock.name);
        Ok(surplus)
    }

    /// Overwrites an existing ref.
    pub fn set_failed_ref(&self, name: &str, commit: &Sha) -> Result<()> {
        let full = failed_ref(name)?;
        self.ext
            .git(
                &self.repo,
                ["update-ref", full.as_str(), &format!("{commit}^{{commit}}")],
            )?
            .ok()
            .with_context(|| format!("참조를 만들지 못했다: {full}"))?;
        Ok(())
    }

    /// Succeeds when the ref is already gone.
    pub fn delete_failed_ref(&self, name: &str) -> Result<()> {
        let full = failed_ref(name)?;
        self.ext
            .git(&self.repo, ["update-ref", "-d", full.as_str()])?
            .ok()
            .with_context(|| format!("참조를 지우지 못했다: {full}"))?;
        Ok(())
    }

    pub fn slot_path(&self, name: SlotName) -> PathBuf {
        self.slots_dir.join(name.to_string())
    }

    fn ensure_worktree(&self, name: SlotName) -> Result<()> {
        if self.is_worktree_of_repo(&self.slot_path(name))? {
            return Ok(());
        }
        self.rebuild_worktree(name)
    }

    fn rebuild_worktree(&self, name: SlotName) -> Result<()> {
        let path = self.slot_path(name);
        remove_tree(&path).with_context(|| format!("지우지 못했다: {}", path.display()))?;
        self.add_worktree(&path)?;
        self.seed_cache(name);
        Ok(())
    }

    fn add_worktree(&self, path: &Path) -> Result<()> {
        // `worktree add` rewrites `.git/worktrees`, and two of them racing there can fail.
        let _admin = self.lock_worktree_admin()?;
        // Without a commit-ish git starts at the user's HEAD, which an unborn branch lacks.
        let any_commit = self
            .ext
            .git(&self.repo, ["rev-list", "-n1", "--all"])?
            .ok()?
            .trim()
            .to_owned();
        if any_commit.is_empty() {
            bail!("저장소에 커밋이 하나도 없다: {}", self.repo.display());
        }
        let args = [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--quiet"),
            // Twice, so a registration someone locked for the missing folder is overridden too.
            OsStr::new("--force"),
            OsStr::new("--force"),
            OsStr::new("--detach"),
            OsStr::new("--no-checkout"),
            path.as_os_str(),
            OsStr::new(&any_commit),
        ];
        self.ext
            .git(&self.repo, args)?
            .ok()
            .with_context(|| format!("슬롯을 만들지 못했다: {}", path.display()))?;
        Ok(())
    }

    fn seed_cache(&self, name: SlotName) {
        if self.cache_seed.is_empty() {
            return;
        }
        let donors = self.used_slots_newest_first(name);
        let to = self.slot_path(name);
        for rel in &self.cache_seed {
            let Some(from) = donors
                .iter()
                .find(|from| fs::symlink_metadata(from.join(rel)).is_ok())
            else {
                continue;
            };
            if let Err(e) = self.copy_seed(from, &to, rel) {
                eprintln!(
                    "경고: 캐시 {rel}를 {}에서 복사하지 못했다: {e:#}",
                    from.display()
                );
            }
        }
    }

    fn used_slots_newest_first(&self, except: SlotName) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.slots_dir) else {
            return Vec::new();
        };
        let mut found: Vec<(SystemTime, PathBuf)> = entries
            .filter_map(|entry| {
                let file_name = entry.ok()?.file_name();
                let slot = SlotName::parse(file_name.to_str()?)?;
                let path = self.slot_path(slot);
                if slot == except || !path.join(".git").is_file() {
                    return None;
                }
                let used = fs::metadata(self.slots_dir.join(format!("{slot}.lock")))
                    .ok()?
                    .modified()
                    .ok()?;
                Some((used, path))
            })
            .collect();
        found.sort_by(|a, b| b.cmp(a));
        found.into_iter().map(|(_, path)| path).collect()
    }

    fn copy_seed(&self, from: &Path, to: &Path, rel: &str) -> Result<()> {
        let rel = Path::new(rel);
        if rel.as_os_str().is_empty()
            || !rel.components().all(|c| matches!(c, Component::Normal(_)))
        {
            bail!("슬롯 안의 상대 경로가 아니다");
        }
        let source = from.join(rel);
        let Ok(canonical) = fs::canonicalize(&source) else {
            return Ok(());
        };
        if !canonical.starts_with(fs::canonicalize(from)?) {
            bail!("슬롯 밖을 가리킨다");
        }
        let ignored = self
            .ext
            .git(
                from,
                [
                    OsStr::new("check-ignore"),
                    OsStr::new("-q"),
                    OsStr::new("--"),
                    rel.as_os_str(),
                ],
            )?
            .success();
        if !ignored {
            bail!("git이 무시하는 경로가 아니다");
        }
        let target = to.join(rel);
        if fs::symlink_metadata(&target).is_ok() {
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
            if !fs::canonicalize(parent)?.starts_with(fs::canonicalize(to)?) {
                bail!("슬롯 밖으로 복사하게 된다");
            }
        }
        let copied = Command::new("cp")
            .args(["-a", "--reflink=auto", "--"])
            .arg(&source)
            .arg(&target)
            .output()
            .context("cp를 실행하지 못했다")?;
        if !copied.status.success() {
            let _ = remove_tree(&target);
            bail!("{}", String::from_utf8_lossy(&copied.stderr).trim());
        }
        if let Err(e) = drop_links_leaving(&target, &fs::canonicalize(to)?) {
            let _ = remove_tree(&target);
            return Err(anyhow::Error::new(e).context("복사한 링크를 검사하지 못했다"));
        }
        Ok(())
    }

    fn lock_worktree_admin(&self) -> Result<File> {
        fs::create_dir_all(&self.slots_dir)
            .with_context(|| format!("만들지 못했다: {}", self.slots_dir.display()))?;
        let path = self.slots_dir.join("worktree-admin.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("열지 못했다: {}", path.display()))?;
        file.lock()
            .with_context(|| format!("잠그지 못했다: {}", path.display()))?;
        Ok(file)
    }

    fn is_worktree_of_repo(&self, path: &Path) -> Result<bool> {
        if !path.join(".git").is_file() {
            return Ok(false);
        }
        let common_dir = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
        let theirs = self.ext.git(path, common_dir)?;
        if !theirs.success() {
            return Ok(false);
        }
        let ours = self.ext.git(&self.repo, common_dir)?.ok()?;
        let same = fs::canonicalize(theirs.stdout_text().trim())
            .ok()
            .zip(fs::canonicalize(ours.trim()).ok())
            .is_some_and(|(a, b)| a == b);
        Ok(same)
    }
}

fn failed_ref(name: &str) -> Result<String> {
    let plain = |c: char| c.is_ascii_alphanumeric() || "._-".contains(c);
    let valid = name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name.chars().all(plain)
        && !name.contains("..")
        && !name.ends_with(".lock")
        && !name.ends_with('.');
    if !valid {
        bail!("실패 트리 참조 이름으로 쓸 수 없다: {name:?}");
    }
    Ok(format!("{FAILED_REF_PREFIX}{name}"))
}

/// Deletes every symlink at or under `entry` whose target is not inside `slot`: a copied
/// link that still points into the donor slot or elsewhere would let the new slot write there.
fn drop_links_leaving(entry: &Path, slot: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(entry)?;
    if meta.file_type().is_symlink() {
        // The filesystem resolves the whole chain, `..` after a link included. A link that
        // does not resolve at all is dropped too.
        let inside = fs::canonicalize(entry).is_ok_and(|real| real.starts_with(slot));
        if !inside {
            eprintln!(
                "경고: 슬롯 밖을 가리키는 링크를 지웠다: {}",
                entry.display()
            );
            fs::remove_file(entry)?;
        }
    } else if meta.is_dir() {
        for child in fs::read_dir(entry)? {
            drop_links_leaving(&child?.path(), slot)?;
        }
    }
    Ok(())
}

/// `remove_dir_all` cannot empty a directory it may not write to, and a build can leave one.
fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            make_writable(path)?;
            fs::remove_dir_all(path)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn make_writable(dir: &Path) -> io::Result<()> {
    let mode = fs::symlink_metadata(dir)?.permissions().mode();
    fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o700))?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            make_writable(&entry.path())?;
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct SlotLock {
    git: Gitops,
    name: SlotName,
    _file: File,
}

impl SlotLock {
    fn mark_used(&self) {
        let _ = self._file.set_modified(SystemTime::now());
    }

    pub fn name(&self) -> SlotName {
        self.name
    }

    pub fn path(&self) -> PathBuf {
        self.git.slot_path(self.name)
    }

    /// Deletes the slot's worktree and its registration in the repository. The lock file
    /// stays: a holder that already opened it would otherwise lock a file nobody else sees.
    pub fn remove(self) -> Result<()> {
        let path = self.path();
        let _admin = self.git.lock_worktree_admin()?;
        let args = [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            // Twice, so a registration someone locked is removed too.
            OsStr::new("--force"),
            OsStr::new("--force"),
            path.as_os_str(),
        ];
        let removed = self.git.ext.git(&self.git.repo, args)?;
        if !removed.success() || path.exists() {
            remove_tree(&path).with_context(|| format!("지우지 못했다: {}", path.display()))?;
            self.git
                .ext
                .git(&self.git.repo, ["worktree", "prune"])?
                .ok()
                .context("슬롯의 등록을 정리하지 못했다")?;
        }
        Ok(())
    }

    /// Makes sure the worktree exists, creating or repairing it. Its content is whatever the
    /// last user left; `Slot::checkout` and `Slot::merge` set it.
    pub fn open(self) -> Result<Slot> {
        self.git.ensure_worktree(self.name)?;
        self.mark_used();
        let path = self.git.slot_path(self.name);
        Ok(Slot { path, lock: self })
    }
}

#[derive(Debug)]
pub struct Slot {
    path: PathBuf,
    lock: SlotLock,
}

impl Slot {
    pub fn name(&self) -> SlotName {
        self.lock.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn head(&self) -> Result<Sha> {
        self.sha("HEAD")
    }

    pub fn tree_of(&self, commit: &Sha) -> Result<Sha> {
        let out = self.run(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])?;
        Sha::try_from(out.trim().to_owned()).map_err(|e| anyhow!(e))
    }

    /// The files whose content differs between `base` and the slot's HEAD, as the `paths` of a
    /// verification step see them. Renames are listed as a delete and an add, so both the old and
    /// the new path count.
    pub fn changed_files(&self, base: &Sha) -> Result<Vec<String>> {
        let out = self
            .git(&[
                "diff",
                "--name-only",
                "--no-renames",
                "--no-ext-diff",
                "--ignore-submodules=none",
                "-z",
                base.as_str(),
                "HEAD",
            ])?
            .ok()
            .with_context(|| format!("{base}와 HEAD의 바뀐 파일을 읽지 못했다"))?;
        Ok(out
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Fetches the default branch and returns its tip. `<remote>/<branch>` is moved to the same
    /// commit.
    pub fn fetch_base(&self) -> Result<Sha> {
        let git = &self.lock.git;
        let branch = &git.default_branch;
        let tracking = format!("refs/remotes/{}/{branch}", git.remote);
        let before = self.read_ref(&tracking)?;
        let tip = self
            .fetch(&format!("refs/heads/{branch}"))
            .with_context(|| format!("{}/{branch}를 가져오지 못했다", git.remote))?;
        self.advance_tracking_ref(&tracking, before.as_ref(), &tip)?;
        Ok(tip)
    }

    /// Fetches `refs/pull/<n>/head` and compares it with the head registered at submit, which
    /// must be a full SHA.
    pub fn fetch_pr_head(&self, pr: PrNumber, expected: &Sha) -> Result<PrHead> {
        let actual = self
            .fetch(&format!("refs/pull/{}/head", pr.0))
            .with_context(|| format!("PR {pr}의 HEAD를 가져오지 못했다"))?;
        if expected.as_str().len() != actual.as_str().len() {
            bail!("기대한 HEAD {expected}가 전체 SHA가 아니다");
        }
        Ok(if actual == *expected {
            PrHead::Unchanged
        } else {
            PrHead::Changed { actual }
        })
    }

    /// Sets the worktree to `commit`: tracked files are restored, untracked files are
    /// deleted, ignored files (build caches) stay. Also clears what a killed git left behind.
    /// A slot that cannot be set, say with a corrupt index, is rebuilt from nothing.
    pub fn checkout(&self, commit: &Sha) -> Result<()> {
        let context = || format!("슬롯 {}을 {commit}으로 맞추지 못했다", self.name());
        self.sha(commit.as_str()).with_context(context)?;
        if self.set_to(commit).is_err() {
            self.lock
                .git
                .rebuild_worktree(self.name())
                .with_context(context)?;
            self.set_to(commit).with_context(context)?;
        }
        Ok(())
    }

    /// Sets the slot to `base`, then merges the items in order with `--no-ff`. An item that
    /// conflicts is undone and left out; the next one still merges.
    pub fn merge(&self, base: &Sha, items: &[MergeItem]) -> Result<MergeResult> {
        self.checkout(base)?;
        let mut tip = base.clone();
        let mut outcomes = Vec::with_capacity(items.len());
        for item in items {
            let result = self.merge_one(&tip, item)?;
            if let ItemResult::Merged { commit } = &result {
                tip = commit.clone();
            }
            outcomes.push(ItemOutcome {
                item: item.clone(),
                result,
            });
        }
        Ok(MergeResult {
            tip,
            items: outcomes,
        })
    }

    pub fn push(&self, commit: &Sha) -> Result<PushOutcome> {
        let git = &self.lock.git;
        let refspec = format!("{commit}:refs/heads/{}", git.default_branch);
        let out = self.git(&["push", &git.remote, &refspec])?;
        if out.success() {
            return Ok(PushOutcome::Pushed);
        }
        let refused = || {
            anyhow!(
                "{}에 push하지 못했다: {}",
                git.default_branch,
                out.stderr_text().trim()
            )
        };
        // Servers word a lost race each their own way, so ask whether main can still be
        // fast-forwarded to the commit.
        let tip = self.fetch(&format!("refs/heads/{}", git.default_branch));
        let Ok(tip) = tip else {
            return Err(refused());
        };
        if tip != *commit && !self.is_ancestor(&tip, commit)? {
            Ok(PushOutcome::MainMoved)
        } else {
            Err(refused())
        }
    }

    fn merge_one(&self, tip: &Sha, item: &MergeItem) -> Result<ItemResult> {
        let head = item.head();
        if self.is_ancestor(head, tip)? {
            return Ok(ItemResult::AlreadyMerged);
        }
        // An earlier item can stop ignoring a path, which turns a cache file there into an
        // untracked file that git refuses to overwrite.
        self.run(&["clean", "--quiet", "-ffd"])?;
        let message = item.github_merge_message(&self.lock.git.owner);
        // With `rerere.autoupdate`, rerere stages a recorded resolution and so hides the conflict.
        let out = self.git(&[
            "-c",
            "rerere.enabled=false",
            "-c",
            "i18n.commitEncoding=UTF-8",
            "merge",
            "--no-ff",
            "--no-edit",
            "--no-log",
            "--cleanup=whitespace",
            "-m",
            &message,
            head.as_str(),
        ])?;
        if out.success() {
            return Ok(ItemResult::Merged {
                commit: self.head()?,
            });
        }
        let files = self.conflict_files(tip, head)?;
        self.checkout(tip)?;
        match NonEmpty::new(files) {
            Some(files) => Ok(ItemResult::Conflict(files)),
            None => bail!(
                "{head}를 합치지 못했다(충돌이 아닌 실패): {}",
                out.stderr_text().trim()
            ),
        }
    }

    fn is_ancestor(&self, commit: &Sha, of: &Sha) -> Result<bool> {
        let out = self.git(&["merge-base", "--is-ancestor", commit.as_str(), of.as_str()])?;
        match out.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!(
                "{commit}가 {of}의 조상인지 알지 못했다: {}",
                out.stderr_text().trim()
            ),
        }
    }

    fn conflict_files(&self, ours: &Sha, theirs: &Sha) -> Result<Vec<ConflictFile>> {
        let listed = self.git(&["diff", "--name-only", "--diff-filter=U", "-z"])?;
        if !listed.success() {
            bail!("충돌한 파일을 읽지 못했다: {}", listed.stderr_text().trim());
        }
        let mut paths: Vec<&[u8]> = listed.stdout.split(|b| *b == 0).collect();
        paths.retain(|p| !p.is_empty());
        paths.dedup();
        let range = format!("{theirs}..{ours}");
        paths
            .into_iter()
            .map(|path| {
                Ok(ConflictFile {
                    path: String::from_utf8_lossy(path).into_owned(),
                    main_commits: self.commits_touching(&range, path)?,
                })
            })
            .collect()
    }

    fn commits_touching(&self, range: &str, path: &[u8]) -> Result<Vec<CommitRef>> {
        let args = [
            OsStr::new("--literal-pathspecs"),
            OsStr::new("log"),
            OsStr::new("--no-merges"),
            OsStr::new("--no-show-signature"),
            OsStr::new("--encoding=UTF-8"),
            OsStr::new("--format=%H %s"),
            OsStr::new(range),
            OsStr::new("--"),
            OsStr::from_bytes(path),
        ];
        let log = self.lock.git.ext.git(&self.path, args)?.ok()?;
        log.lines()
            .map(|line| {
                let (sha, title) = line.split_once(' ').unwrap_or((line, ""));
                Ok(CommitRef {
                    sha: Sha::try_from(sha.to_owned()).map_err(|e| anyhow!(e))?,
                    title: title.to_owned(),
                })
            })
            .collect()
    }

    fn fetch(&self, refspec: &str) -> Result<Sha> {
        // An empty refmap stops git from updating the tracking ref itself, which fails with
        // "incorrect old value provided" when two slots do it at once.
        self.run(&[
            "fetch",
            "--quiet",
            "--no-tags",
            "--refmap=",
            &self.lock.git.remote,
            refspec,
        ])?;
        self.sha("FETCH_HEAD")
    }

    fn set_to(&self, commit: &Sha) -> Result<()> {
        self.clear_stale_locks()?;
        // Detach first, so a branch someone checked out in here by hand cannot be moved.
        self.run(&["update-ref", "--no-deref", "HEAD", commit.as_str()])?;
        self.run(&["reset", "--quiet", "--hard"])?;
        self.run(&["clean", "--quiet", "-ffd"])?;
        Ok(())
    }

    fn read_ref(&self, name: &str) -> Result<Option<Sha>> {
        let out = self.git(&["rev-parse", "--verify", "--quiet", name])?;
        if !out.success() {
            return Ok(None);
        }
        Sha::try_from(out.stdout_text().trim().to_owned())
            .map(Some)
            .map_err(|e| anyhow!(e))
    }

    /// Best effort: after `TRACKING_ATTEMPTS` lost races the ref is left for the next fetch.
    fn advance_tracking_ref(&self, tracking: &str, before: Option<&Sha>, tip: &Sha) -> Result<()> {
        let mut expected = before.cloned();
        for _ in 0..TRACKING_ATTEMPTS {
            // An empty old value makes git require that the ref does not exist.
            let old = expected.as_ref().map_or("", Sha::as_str);
            let out = self.git(&["update-ref", tracking, tip.as_str(), old])?;
            if out.success() {
                return Ok(());
            }
            let now = self.read_ref(tracking)?;
            if now == expected {
                bail!("{tracking}을 옮기지 못했다: {}", out.stderr_text().trim());
            }
            if let Some(now) = &now
                && !self.is_ancestor(now, tip)?
            {
                return Ok(());
            }
            expected = now;
        }
        Ok(())
    }

    fn clear_stale_locks(&self) -> Result<()> {
        let git_dir = PathBuf::from(self.run(&["rev-parse", "--absolute-git-dir"])?.trim());
        for entry in
            fs::read_dir(&git_dir).with_context(|| format!("읽지 못했다: {}", git_dir.display()))?
        {
            let entry = entry?;
            if entry.file_name().to_string_lossy().ends_with(".lock")
                && entry.file_type()?.is_file()
            {
                match fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("지우지 못했다: {}", entry.path().display()));
                    }
                }
            }
        }
        Ok(())
    }

    fn sha(&self, rev: &str) -> Result<Sha> {
        let out = self.run(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")])?;
        Sha::try_from(out.trim().to_owned()).map_err(|e| anyhow!(e))
    }

    fn git(&self, args: &[&str]) -> Result<Output> {
        let args = NO_HOOKS.iter().chain(args);
        Ok(self.lock.git.ext.git(&self.path, args)?)
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        Ok(self.git(args)?.ok()?)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::repo;
    use crate::testkit::{Sandbox, TestRepo};

    fn hermetic_ext(sb: &Sandbox) -> Ext {
        let mut body = String::new();
        for (key, value) in sb.envs() {
            let key = key.to_string_lossy();
            match value {
                Some(value) => {
                    body.push_str(&format!("export {key}='{}'\n", value.to_string_lossy()))
                }
                None => body.push_str(&format!("unset {key}\n")),
            }
        }
        body.push_str("exec git \"$@\"\n");
        sb.script("git", &body);
        Ext::from_vars(|k| sb.var(k))
    }

    fn git_output(sb: &Sandbox, dir: &Path, args: &[&str]) -> std::process::Output {
        sb.command("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap()
    }

    fn git(sb: &Sandbox, dir: &Path, args: &[&str]) -> String {
        let out = git_output(sb, dir, args);
        assert!(
            out.status.success(),
            "git {args:?} 실패: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn git_succeeds(sb: &Sandbox, dir: &Path, args: &[&str]) -> bool {
        git_output(sb, dir, args).status.success()
    }

    fn sha(text: &str) -> Sha {
        Sha::try_from(text.trim().to_owned()).unwrap()
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Snapshot {
        head: String,
        symbolic_head: String,
        branches: String,
        status: String,
        index: String,
        fetch_head: bool,
    }

    impl Snapshot {
        fn of(repo: &TestRepo) -> Snapshot {
            Snapshot {
                head: repo.git(&["rev-parse", "HEAD"]),
                symbolic_head: repo.git(&["symbolic-ref", "HEAD"]),
                branches: repo.git(&[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ]),
                status: repo.git(&["status", "--porcelain", "--untracked-files=all"]),
                index: repo.git(&["ls-files", "-s"]),
                fetch_head: repo.path().join(".git/FETCH_HEAD").exists(),
            }
        }
    }

    struct World {
        sb: Sandbox,
        repo: TestRepo,
        gitops: Gitops,
        peer: PathBuf,
        before: Snapshot,
    }

    impl World {
        fn new() -> World {
            let sb = Sandbox::new();
            let repo = TestRepo::new(&sb, "acme/widget");
            // With merge.log set, git appends a shortlog to a merge message.
            repo.git(&["config", "merge.log", "true"]);
            repo.write(".gitignore", "target/\n");
            repo.write("shared.txt", "one\ntwo\nthree\n");
            repo.write("docs/x.md", "doc\n");
            repo.git(&["add", "."]);
            repo.git(&["commit", "--quiet", "-m", "chore: 기준 파일"]);
            repo.git(&["push", "--quiet", "origin", "main"]);
            let ext = hermetic_ext(&sb);
            let info = repo::discover(&ext, repo.path(), &|| Ok(Vec::new())).unwrap();
            let gitops = Gitops::new(&ext, &Home::at(sb.chute_home()), &info);
            let peer = sb.root().join("peer");
            let origin = repo.origin().to_string_lossy().into_owned();
            git(
                &sb,
                sb.root(),
                &["clone", "--quiet", &origin, &peer.to_string_lossy()],
            );
            let before = Snapshot::of(&repo);
            World {
                sb,
                repo,
                gitops,
                peer,
                before,
            }
        }

        fn peer_git(&self, args: &[&str]) -> String {
            git(&self.sb, &self.peer, args)
        }

        fn commit_in_peer(&self, files: &[(&str, &str)], message: &str) -> Sha {
            for (path, content) in files {
                let full = self.peer.join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, content).unwrap();
            }
            self.peer_git(&["add", "."]);
            self.peer_git(&["commit", "--quiet", "-m", message]);
            sha(&self.peer_git(&["rev-parse", "HEAD"]))
        }

        fn open_pr(&self, n: u64, files: &[(&str, &str)]) -> MergeItem {
            self.peer_git(&["fetch", "--quiet", "origin"]);
            let branch = format!("pr-{n}");
            self.peer_git(&["checkout", "--quiet", "-B", &branch, "origin/main"]);
            let title = format!("feat: change {n}");
            let head = self.commit_in_peer(files, &title);
            self.peer_git(&[
                "push",
                "--quiet",
                "origin",
                &format!("{branch}:refs/heads/{branch}"),
                &format!("{branch}:refs/pull/{n}/head"),
            ]);
            MergeItem::Pr {
                pr: PrNumber(n),
                branch,
                title,
                head,
            }
        }

        fn advance_main(&self, files: &[(&str, &str)], message: &str) -> Sha {
            self.peer_git(&["fetch", "--quiet", "origin"]);
            self.peer_git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
            let head = self.commit_in_peer(files, message);
            self.peer_git(&["push", "--quiet", "origin", "main"]);
            head
        }

        fn origin_main(&self) -> Sha {
            sha(&git(
                &self.sb,
                self.repo.origin(),
                &["rev-parse", "refs/heads/main"],
            ))
        }

        /// A slot another test thread's fork may briefly make look held, so this retries.
        fn slot(&self, name: SlotName) -> Slot {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(lock) = self.gitops.lock_slot(name).unwrap() {
                    return lock.open().unwrap();
                }
                assert!(Instant::now() < until, "슬롯 {name}을 잡지 못했다");
                thread::sleep(Duration::from_millis(10));
            }
        }

        fn merge(&self, slot: &Slot, items: &[MergeItem]) -> MergeResult {
            let base = slot.fetch_base().unwrap();
            for item in items {
                if let MergeItem::Pr { pr, head, .. } = item {
                    assert_eq!(slot.fetch_pr_head(*pr, head).unwrap(), PrHead::Unchanged);
                }
            }
            slot.merge(&base, items).unwrap()
        }

        fn in_slot(&self, slot: &Slot, args: &[&str]) -> String {
            git(&self.sb, slot.path(), args)
        }

        fn message(&self, commit: &Sha) -> String {
            self.repo
                .git(&[
                    "-c",
                    "i18n.logOutputEncoding=UTF-8",
                    "log",
                    "-1",
                    "--format=%B",
                    commit.as_str(),
                ])
                .trim()
                .to_owned()
        }

        fn parents(&self, commit: &Sha) -> Vec<Sha> {
            self.repo
                .git(&["rev-list", "--parents", "-n1", commit.as_str()])
                .split_whitespace()
                .skip(1)
                .map(sha)
                .collect()
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            if !thread::panicking() {
                assert_eq!(
                    Snapshot::of(&self.repo),
                    self.before,
                    "사용자의 체크아웃이 바뀌었다"
                );
            }
        }
    }

    fn merged_commit(outcome: &ItemOutcome) -> Sha {
        match &outcome.result {
            ItemResult::Merged { commit } => commit.clone(),
            other => panic!("합쳐져야 한다: {other:?}"),
        }
    }

    #[test]
    fn each_pr_gets_a_merge_commit_in_github_form() {
        let w = World::new();
        let items = [
            w.open_pr(1, &[("a.txt", "a\n")]),
            w.open_pr(2, &[("b.txt", "b\n")]),
            w.open_pr(3, &[("c.txt", "c\n")]),
        ];
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();

        let merged = w.merge(&slot, &items);

        assert_eq!(merged.items.len(), 3);
        let mut previous = base;
        for (n, outcome) in merged.items.iter().enumerate() {
            let commit = merged_commit(outcome);
            assert_eq!(outcome.item, items[n]);
            assert_eq!(
                w.parents(&commit),
                [previous.clone(), items[n].head().clone()],
                "PR {}의 merge commit은 부모가 이전 tip과 PR HEAD여야 한다",
                n + 1
            );
            assert_eq!(
                w.message(&commit),
                format!(
                    "Merge pull request #{0} from acme/pr-{0}\n\nfeat: change {0}",
                    n + 1
                )
            );
            previous = commit;
        }
        assert_eq!(merged.tip, previous);
        assert_eq!(slot.head().unwrap(), merged.tip);
        for file in ["a.txt", "b.txt", "c.txt"] {
            assert!(slot.path().join(file).exists(), "{file}");
        }
    }

    #[test]
    fn a_conflicting_pr_drops_out_and_the_others_still_merge() {
        let w = World::new();
        let items = [
            w.open_pr(
                1,
                &[("shared.txt", "ONE\ntwo\nthree\n"), ("docs/x.md", "doc1\n")],
            ),
            w.open_pr(
                2,
                &[
                    ("shared.txt", "uno\ntwo\nthree\n"),
                    ("docs/x.md", "doc2\n"),
                    ("extra.txt", "extra\n"),
                ],
            ),
            w.open_pr(3, &[("c.txt", "c\n")]),
        ];
        let slot = w.slot(SlotName::Batch);

        let merged = w.merge(&slot, &items);

        let first = merged_commit(&merged.items[0]);
        let third = merged_commit(&merged.items[2]);
        let ItemResult::Conflict(files) = &merged.items[1].result else {
            panic!("둘째 PR은 충돌해야 한다: {:?}", merged.items[1].result);
        };
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["docs/x.md", "shared.txt"]);
        assert_eq!(w.parents(&third), [first.clone(), items[2].head().clone()]);
        assert_eq!(merged.tip, third);

        assert_eq!(slot.head().unwrap(), third);
        assert_eq!(w.in_slot(&slot, &["status", "--porcelain"]), "");
        assert!(!git_succeeds(
            &w.sb,
            slot.path(),
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"]
        ));
        let shared = fs::read_to_string(slot.path().join("shared.txt")).unwrap();
        assert_eq!(shared, "ONE\ntwo\nthree\n");
        assert!(!slot.path().join("extra.txt").exists());
        assert!(slot.path().join("c.txt").exists());
    }

    #[test]
    fn a_conflict_names_the_main_commits_that_changed_the_file() {
        let w = World::new();
        let item = w.open_pr(1, &[("shared.txt", "mine\ntwo\nthree\n")]);
        let first = w.advance_main(&[("shared.txt", "m1\ntwo\nthree\n")], "fix: 첫째 변경");
        w.advance_main(&[("other.txt", "x\n")], "chore: 관련 없는 변경");
        let second = w.advance_main(&[("shared.txt", "m2\ntwo\nthree\n")], "fix: 둘째 변경");
        let slot = w.slot(SlotName::Batch);

        let merged = w.merge(&slot, &[item]);

        let ItemResult::Conflict(files) = &merged.items[0].result else {
            panic!("충돌해야 한다: {:?}", merged.items[0].result);
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "shared.txt");
        assert_eq!(
            files[0].main_commits,
            [
                CommitRef {
                    sha: second,
                    title: "fix: 둘째 변경".into()
                },
                CommitRef {
                    sha: first,
                    title: "fix: 첫째 변경".into()
                },
            ]
        );
    }

    #[test]
    fn a_conflict_with_an_earlier_pr_of_the_run_names_that_prs_commit() {
        let w = World::new();
        let items = [
            w.open_pr(1, &[("shared.txt", "ONE\ntwo\nthree\n")]),
            w.open_pr(2, &[("shared.txt", "uno\ntwo\nthree\n")]),
        ];
        let slot = w.slot(SlotName::Batch);

        let merged = w.merge(&slot, &items);

        let ItemResult::Conflict(files) = &merged.items[1].result else {
            panic!("충돌해야 한다: {:?}", merged.items[1].result);
        };
        assert_eq!(
            files[0].main_commits,
            [CommitRef {
                sha: items[0].head().clone(),
                title: "feat: change 1".into()
            }]
        );
    }

    #[test]
    fn a_pr_already_in_the_base_is_reported_and_makes_no_commit() {
        let w = World::new();
        let items = [
            w.open_pr(1, &[("a.txt", "a\n")]),
            w.open_pr(2, &[("b.txt", "b\n")]),
        ];
        w.peer_git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
        w.peer_git(&[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "Merge pull request #1",
            "pr-1",
        ]);
        w.peer_git(&["push", "--quiet", "origin", "main"]);
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();

        let merged = w.merge(&slot, &items);

        assert_eq!(merged.items[0].result, ItemResult::AlreadyMerged);
        let second = merged_commit(&merged.items[1]);
        assert_eq!(w.parents(&second), [base.clone(), items[1].head().clone()]);

        let only = slot.merge(&base, &items[..1]).unwrap();
        assert_eq!(only.items[0].result, ItemResult::AlreadyMerged);
        assert_eq!(only.tip, base);
        assert_eq!(slot.head().unwrap(), base);
    }

    #[test]
    fn a_pr_head_that_moved_is_told_apart_from_an_error() {
        let w = World::new();
        let MergeItem::Pr { pr, head, .. } = w.open_pr(1, &[("a.txt", "a\n")]) else {
            unreachable!()
        };
        let slot = w.slot(SlotName::Batch);
        assert_eq!(slot.fetch_pr_head(pr, &head).unwrap(), PrHead::Unchanged);

        w.peer_git(&["checkout", "--quiet", "pr-1"]);
        let newer = w.commit_in_peer(&[("a.txt", "a2\n")], "feat: 더 고쳤다");
        w.peer_git(&[
            "push",
            "--quiet",
            "--force",
            "origin",
            "pr-1:refs/pull/1/head",
        ]);

        assert_eq!(
            slot.fetch_pr_head(pr, &head).unwrap(),
            PrHead::Changed {
                actual: newer.clone()
            }
        );
        assert!(git_succeeds(
            &w.sb,
            slot.path(),
            &["cat-file", "-e", &format!("{newer}^{{commit}}")]
        ));
        assert!(slot.fetch_pr_head(PrNumber(99), &head).is_err());
    }

    #[test]
    fn push_sets_origin_main_to_the_commit() {
        let w = World::new();
        let items = [w.open_pr(1, &[("a.txt", "a\n")])];
        let slot = w.slot(SlotName::Batch);
        let merged = w.merge(&slot, &items);

        assert_eq!(slot.push(&merged.tip).unwrap(), PushOutcome::Pushed);

        assert_eq!(w.origin_main(), merged.tip);
        assert_eq!(slot.push(&merged.tip).unwrap(), PushOutcome::Pushed);
    }

    #[test]
    fn push_is_refused_when_origin_main_moved_and_leaves_it_alone() {
        let w = World::new();
        let items = [w.open_pr(1, &[("a.txt", "a\n")])];
        let slot = w.slot(SlotName::Batch);
        let merged = w.merge(&slot, &items);
        let theirs = w.advance_main(&[("n.txt", "n\n")], "feat: 그사이 머지된 변경");

        // The new tip is not in this repository yet, so git says "fetch first".
        assert_eq!(slot.push(&merged.tip).unwrap(), PushOutcome::MainMoved);
        assert_eq!(w.origin_main(), theirs);

        // Once the slot has fetched it, git says "non-fast-forward".
        assert_eq!(slot.fetch_base().unwrap(), theirs);
        assert_eq!(slot.push(&merged.tip).unwrap(), PushOutcome::MainMoved);
        assert_eq!(w.origin_main(), theirs);
    }

    #[test]
    fn a_rejection_that_is_not_a_moved_main_is_an_error() {
        let w = World::new();
        let items = [w.open_pr(1, &[("a.txt", "a\n")])];
        let slot = w.slot(SlotName::Batch);
        let merged = w.merge(&slot, &items);
        let hook = w.repo.origin().join("hooks/pre-receive");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, "#!/bin/sh\necho 'no direct pushes' >&2\nexit 1\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let before = w.origin_main();

        let err = slot.push(&merged.tip).unwrap_err();

        assert!(format!("{err:#}").contains("no direct pushes"), "{err:#}");
        assert_eq!(w.origin_main(), before);
    }

    #[test]
    fn a_locked_tracking_ref_does_not_turn_a_moved_main_into_an_error() {
        let w = World::new();
        let items = [w.open_pr(1, &[("a.txt", "a\n")])];
        let slot = w.slot(SlotName::Batch);
        let merged = w.merge(&slot, &items);
        let theirs = w.advance_main(&[("n.txt", "n\n")], "feat: 그사이 머지된 변경");
        let lock = w.repo.path().join(".git/refs/remotes/origin/main.lock");
        fs::write(&lock, "").unwrap();

        assert_eq!(slot.push(&merged.tip).unwrap(), PushOutcome::MainMoved);

        assert_eq!(w.origin_main(), theirs);
        fs::remove_file(&lock).unwrap();
    }

    #[test]
    fn a_submodule_bump_counts_as_changed_even_when_the_pr_sets_ignore_all() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        let gl = base.as_str().to_owned();
        w.in_slot(
            &slot,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{gl},vendor/lib"),
            ],
        );
        w.in_slot(
            &slot,
            &[
                "-c",
                "user.name=a",
                "-c",
                "user.email=a@a",
                "commit",
                "-qm",
                "add sub",
            ],
        );
        let b2 = sha(&w.in_slot(&slot, &["rev-parse", "HEAD"]));
        fs::write(
            slot.path().join(".gitmodules"),
            "[submodule \"vendor/lib\"]\n\tpath = vendor/lib\n\turl = ../x\n\tignore = all\n",
        )
        .unwrap();
        w.in_slot(&slot, &["add", ".gitmodules"]);
        w.in_slot(
            &slot,
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{},vendor/lib", "1".repeat(40)),
            ],
        );
        w.in_slot(
            &slot,
            &[
                "-c",
                "user.name=a",
                "-c",
                "user.email=a@a",
                "commit",
                "-qm",
                "bump + ignore",
            ],
        );
        assert_eq!(
            slot.changed_files(&b2).unwrap(),
            [".gitmodules", "vendor/lib"]
        );
    }

    #[test]
    fn fetch_base_returns_origin_main_and_keeps_the_tracking_ref_current() {
        let w = World::new();
        let moved = w.advance_main(&[("n.txt", "n\n")], "feat: 새 변경");
        let slot = w.slot(SlotName::Batch);

        assert_eq!(slot.fetch_base().unwrap(), moved);

        assert_eq!(sha(&w.repo.git(&["rev-parse", "origin/main"])), moved);
    }

    #[test]
    fn checkout_restores_tracked_files_and_deletes_untracked_ones_but_keeps_ignored() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        let path = slot.path().to_owned();
        fs::create_dir_all(path.join("target")).unwrap();
        fs::write(path.join("target/cache.bin"), "cache").unwrap();
        fs::write(path.join("junk.txt"), "junk").unwrap();
        fs::create_dir_all(path.join("dir")).unwrap();
        fs::write(path.join("dir/junk.txt"), "junk").unwrap();
        git(&w.sb, &path, &["init", "--quiet", "nested"]);
        fs::write(path.join("shared.txt"), "edited\n").unwrap();

        slot.checkout(&base).unwrap();

        let shared = fs::read_to_string(path.join("shared.txt")).unwrap();
        assert_eq!(shared, "one\ntwo\nthree\n");
        for gone in ["junk.txt", "dir", "nested"] {
            assert!(!path.join(gone).exists(), "{gone}");
        }
        assert!(path.join("target/cache.bin").exists());
        assert_eq!(w.in_slot(&slot, &["status", "--porcelain"]), "");
    }

    fn used_slot(w: &World, name: SlotName, files: &[(&str, &str)]) -> Slot {
        let slot = w.slot(name);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        for (path, content) in files {
            let full = slot.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }
        slot
    }

    fn open_seeded(w: &World, seed: &[&str], name: SlotName) -> Slot {
        let git = w
            .gitops
            .clone()
            .with_cache_seed(seed.iter().map(|s| (*s).to_owned()).collect());
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(lock) = git.lock_slot(name).unwrap() {
                return lock.open().unwrap();
            }
            assert!(Instant::now() < until, "슬롯 {name}을 잡지 못했다");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn seed_gives_a_new_slot_the_ignored_target_of_the_slot_used_last() {
        let w = World::new();
        let _older = used_slot(&w, SlotName::Preflight(1), &[("target/old.bin", "old")]);
        thread::sleep(Duration::from_millis(20));
        let _newer = used_slot(&w, SlotName::Preflight(2), &[("target/new.bin", "new")]);

        let fresh = open_seeded(&w, &["target"], SlotName::Preflight(3));

        let target = fresh.path().join("target");
        assert_eq!(fs::read_to_string(target.join("new.bin")).unwrap(), "new");
        assert!(!target.join("old.bin").exists());
    }

    #[test]
    fn seed_reaches_a_slot_that_is_rebuilt() {
        let w = World::new();
        let _donor = used_slot(&w, SlotName::Preflight(1), &[("target/a.bin", "a")]);
        let slot = open_seeded(&w, &["target"], SlotName::Preflight(2));
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        fs::remove_dir_all(slot.path().join("target")).unwrap();
        let pointer = fs::read_to_string(slot.path().join(".git")).unwrap();
        let gitdir = pointer.trim().strip_prefix("gitdir: ").unwrap().to_owned();
        fs::write(Path::new(&gitdir).join("index"), "corrupt").unwrap();

        slot.checkout(&base).unwrap();

        assert!(slot.path().join("target/a.bin").exists());
    }

    #[test]
    fn seed_does_not_touch_a_slot_that_already_exists() {
        let w = World::new();
        let _donor = used_slot(&w, SlotName::Preflight(1), &[("target/a.bin", "a")]);
        drop(w.slot(SlotName::Preflight(2)));

        let again = open_seeded(&w, &["target"], SlotName::Preflight(2));

        assert!(!again.path().join("target").exists());
    }

    #[test]
    fn seed_skips_tracked_paths_and_paths_outside_the_slot() {
        let w = World::new();
        let donor = used_slot(&w, SlotName::Preflight(1), &[("target/a.bin", "a")]);
        let outside = w.sb.root().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, donor.path().join("target/escape")).unwrap();
        let outside_path = outside.to_string_lossy().into_owned();

        let fresh = open_seeded(
            &w,
            &[
                "shared.txt",
                "docs",
                "../outside",
                outside_path.as_str(),
                "target/escape",
                "",
            ],
            SlotName::Preflight(2),
        );

        for kept_out in ["shared.txt", "docs", "target"] {
            assert!(!fresh.path().join(kept_out).exists(), "{kept_out}");
        }
        assert_eq!(fs::read_to_string(outside.join("secret")).unwrap(), "s");
    }

    #[test]
    fn seed_failure_only_warns_and_the_slot_still_opens() {
        let w = World::new();
        let donor = used_slot(&w, SlotName::Preflight(1), &[("target/locked.bin", "x")]);
        let locked = donor.path().join("target/locked.bin");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            return; // root reads anything, so cp cannot fail here
        }

        let fresh = open_seeded(&w, &["target", "cache"], SlotName::Preflight(2));

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(fresh.path().join(".git").is_file());
        assert!(!fresh.path().join("target/locked.bin").exists());
    }

    #[test]
    fn seed_drops_links_that_point_out_of_the_new_slot() {
        let w = World::new();
        let donor = used_slot(&w, SlotName::Preflight(1), &[("target/a.bin", "a")]);
        let outside = w.sb.root().join("outside");
        fs::create_dir_all(outside.join("real")).unwrap();
        std::os::unix::fs::symlink(&outside, donor.path().join("target/escape")).unwrap();
        std::os::unix::fs::symlink("a.bin", donor.path().join("target/inside")).unwrap();
        let into_donor = donor.path().join("target");
        std::os::unix::fs::symlink(&into_donor, donor.path().join("target/to-donor")).unwrap();

        let fresh = open_seeded(&w, &["target"], SlotName::Preflight(2));

        let target = fresh.path().join("target");
        assert!(target.join("a.bin").exists());
        assert!(fs::symlink_metadata(target.join("inside")).is_ok());
        for gone in ["escape", "to-donor"] {
            assert!(fs::symlink_metadata(target.join(gone)).is_err(), "{gone}");
        }
    }

    #[test]
    fn seed_drops_a_link_that_leaves_the_slot_through_dot_dot_after_another_link() {
        let w = World::new();
        let donor = used_slot(&w, SlotName::Preflight(1), &[("target/a.bin", "a")]);
        let slots = donor.path().parent().unwrap().to_owned();
        fs::write(slots.join("marker"), "m").unwrap();
        std::os::unix::fs::symlink("..", donor.path().join("target/d")).unwrap();
        std::os::unix::fs::symlink("d/../marker", donor.path().join("target/L")).unwrap();
        std::os::unix::fs::symlink("missing", donor.path().join("target/dangling")).unwrap();

        let fresh = open_seeded(&w, &["target"], SlotName::Preflight(2));

        let target = fresh.path().join("target");
        assert!(target.join("a.bin").exists());
        for gone in ["L", "dangling"] {
            assert!(fs::symlink_metadata(target.join(gone)).is_err(), "{gone}");
        }
    }

    #[test]
    fn seed_path_that_is_a_link_into_the_donor_is_not_copied_as_a_link() {
        let w = World::new();
        let donor = used_slot(&w, SlotName::Preflight(1), &[("real/x", "x")]);
        let exclude = fs::read_to_string(donor.path().join(".gitignore")).unwrap_or_default();
        fs::write(
            donor.path().join(".gitignore"),
            format!("{exclude}cache\nreal\n"),
        )
        .unwrap();
        std::os::unix::fs::symlink(donor.path().join("real"), donor.path().join("cache")).unwrap();

        let fresh = open_seeded(&w, &["cache"], SlotName::Preflight(2));

        assert!(fs::symlink_metadata(fresh.path().join("cache")).is_err());
    }

    #[test]
    fn seed_falls_back_to_an_older_slot_when_the_newest_has_no_such_path() {
        let w = World::new();
        let _warm = used_slot(&w, SlotName::Preflight(1), &[("target/a.bin", "a")]);
        thread::sleep(Duration::from_millis(20));
        let _empty = used_slot(&w, SlotName::Preflight(2), &[]);

        let fresh = open_seeded(&w, &["target"], SlotName::Preflight(3));

        assert!(fresh.path().join("target/a.bin").exists());
    }

    #[test]
    fn seed_prefers_the_slot_opened_last_not_the_one_created_last() {
        let w = World::new();
        let one = used_slot(&w, SlotName::Preflight(1), &[("target/one", "1")]);
        thread::sleep(Duration::from_millis(20));
        let _two = used_slot(&w, SlotName::Preflight(2), &[("target/two", "2")]);
        thread::sleep(Duration::from_millis(20));
        drop(one);
        let _one_again = open_seeded(&w, &[], SlotName::Preflight(1));

        let fresh = open_seeded(&w, &["target"], SlotName::Preflight(3));

        assert!(fresh.path().join("target/one").exists());
        assert!(!fresh.path().join("target/two").exists());
    }

    #[test]
    fn seed_failure_of_one_path_does_not_stop_the_next() {
        let w = World::new();
        let donor = used_slot(
            &w,
            SlotName::Preflight(1),
            &[("target/locked.bin", "x"), ("cache/ok.bin", "ok")],
        );
        fs::write(donor.path().join(".gitignore"), "target/\ncache/\n").unwrap();
        let locked = donor.path().join("target/locked.bin");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            return; // root reads anything, so cp cannot fail here
        }

        let fresh = open_seeded(&w, &["target", "cache"], SlotName::Preflight(2));

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(fresh.path().join("cache/ok.bin").exists());
        assert!(!fresh.path().join("target").exists());
    }

    #[test]
    fn a_deleted_or_unlinked_slot_folder_is_rebuilt_without_stale_registrations() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        let path = slot.path().to_owned();
        let registered = || -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(w.repo.path().join(".git/worktrees"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        drop(slot);

        fs::remove_dir_all(&path).unwrap();
        let slot = w.slot(SlotName::Batch);
        slot.checkout(&base).unwrap();
        let shared = fs::read_to_string(path.join("shared.txt")).unwrap();
        assert_eq!(shared, "one\ntwo\nthree\n");
        assert_eq!(registered(), ["batch"]);
        drop(slot);

        fs::remove_file(path.join(".git")).unwrap();
        let slot = w.slot(SlotName::Batch);
        slot.checkout(&base).unwrap();
        assert_eq!(w.in_slot(&slot, &["status", "--porcelain"]), "");
        assert_eq!(registered(), ["batch"]);
    }

    #[test]
    fn a_slot_left_mid_merge_or_with_lock_files_can_be_set_again() {
        let w = World::new();
        let items = [
            w.open_pr(1, &[("shared.txt", "ONE\ntwo\nthree\n")]),
            w.open_pr(2, &[("shared.txt", "uno\ntwo\nthree\n")]),
        ];
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        w.merge(&slot, &items[..1]);
        let MergeItem::Pr { pr, head, .. } = &items[1] else {
            unreachable!()
        };
        slot.fetch_pr_head(*pr, head).unwrap();
        let out = git_output(&w.sb, slot.path(), &["merge", "--no-edit", head.as_str()]);
        assert!(!out.status.success());
        assert!(git_succeeds(
            &w.sb,
            slot.path(),
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"]
        ));
        let git_dir = PathBuf::from(w.in_slot(&slot, &["rev-parse", "--absolute-git-dir"]));
        for lock in [
            "index.lock",
            "HEAD.lock",
            "ORIG_HEAD.lock",
            "AUTO_MERGE.lock",
        ] {
            fs::write(git_dir.join(lock), "").unwrap();
        }

        slot.checkout(&base).unwrap();

        assert_eq!(slot.head().unwrap(), base);
        assert_eq!(w.in_slot(&slot, &["status", "--porcelain"]), "");
        assert!(!git_succeeds(
            &w.sb,
            slot.path(),
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"]
        ));
        fs::write(git_dir.join("ORIG_HEAD.lock"), "").unwrap();
        let merged = slot.merge(&base, &items[..1]).unwrap();
        assert!(matches!(merged.items[0].result, ItemResult::Merged { .. }));
    }

    #[test]
    fn a_slot_whose_index_is_corrupt_is_rebuilt_when_it_is_set() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        let git_dir = PathBuf::from(w.in_slot(&slot, &["rev-parse", "--absolute-git-dir"]));
        fs::write(git_dir.join("index"), "not an index").unwrap();

        slot.checkout(&base).unwrap();

        assert_eq!(slot.head().unwrap(), base);
        assert_eq!(w.in_slot(&slot, &["status", "--porcelain"]), "");
        let shared = fs::read_to_string(slot.path().join("shared.txt")).unwrap();
        assert_eq!(shared, "one\ntwo\nthree\n");
    }

    #[test]
    fn a_read_only_directory_a_build_left_does_not_wedge_the_slot() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        let out = slot.path().join("out/sub");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("f"), "x").unwrap();
        fs::set_permissions(&out, fs::Permissions::from_mode(0o555)).unwrap();

        slot.checkout(&base).unwrap();

        assert!(!slot.path().join("out").exists());
        assert_eq!(w.in_slot(&slot, &["status", "--porcelain"]), "");
    }

    #[test]
    fn a_locked_registration_of_a_deleted_slot_is_overridden() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        let path = slot.path().to_owned();
        drop(slot);
        w.repo.git(&["worktree", "lock", &path.to_string_lossy()]);
        fs::remove_dir_all(&path).unwrap();

        let slot = w.slot(SlotName::Batch);

        slot.checkout(&base).unwrap();
        assert!(slot.path().join("shared.txt").exists());
    }

    #[test]
    fn a_slot_that_fetched_earlier_does_not_move_the_tracking_ref_back() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let first = slot.fetch_base().unwrap();
        w.advance_main(&[("n.txt", "1\n")], "feat: 첫째");
        let newest = w.advance_main(&[("n.txt", "2\n")], "feat: 둘째");
        assert_eq!(slot.fetch_base().unwrap(), newest);

        slot.advance_tracking_ref("refs/remotes/origin/main", Some(&first), &first)
            .unwrap();

        assert_eq!(sha(&w.repo.git(&["rev-parse", "origin/main"])), newest);
    }

    #[test]
    fn a_newer_tip_replaces_a_tracking_ref_another_slot_left_behind() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let stale = slot.fetch_base().unwrap();
        let middle = w.advance_main(&[("n.txt", "1\n")], "feat: 첫째");
        slot.fetch_base().unwrap();
        let newest = w.advance_main(&[("n.txt", "2\n")], "feat: 둘째");
        assert_eq!(slot.fetch_base().unwrap(), newest);
        w.repo
            .git(&["update-ref", "refs/remotes/origin/main", middle.as_str()]);

        slot.advance_tracking_ref("refs/remotes/origin/main", Some(&stale), &newest)
            .unwrap();

        assert_eq!(sha(&w.repo.git(&["rev-parse", "origin/main"])), newest);
    }

    #[test]
    fn a_tracking_ref_that_cannot_be_moved_is_an_error_not_silence() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        slot.fetch_base().unwrap();
        w.advance_main(&[("n.txt", "n\n")], "feat: 새 변경");
        let lock = w.repo.path().join(".git/refs/remotes/origin/main.lock");
        fs::write(&lock, "").unwrap();

        let err = slot.fetch_base().unwrap_err();

        assert!(format!("{err:#}").contains("옮기지 못했다"), "{err:#}");
        fs::remove_file(&lock).unwrap();
    }

    #[test]
    fn a_relative_state_folder_still_puts_slots_outside_the_users_checkout() {
        let w = World::new();
        let info = repo::discover(&hermetic_ext(&w.sb), w.repo.path(), &|| Ok(Vec::new())).unwrap();

        let gitops = Gitops::new(
            &Ext::from_vars(|_| None),
            &Home::at("relative-state"),
            &info,
        );

        assert!(
            gitops.slots_dir.is_absolute(),
            "{}",
            gitops.slots_dir.display()
        );
        assert!(!gitops.slots_dir.starts_with(w.repo.path()));
    }

    #[test]
    fn a_local_commit_merges_like_a_pr() {
        let w = World::new();
        let agent = w.sb.root().join("agent");
        w.repo.git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &agent.to_string_lossy(),
        ]);
        fs::write(agent.join("mine.txt"), "mine\n").unwrap();
        git(&w.sb, &agent, &["add", "."]);
        git(
            &w.sb,
            &agent,
            &["commit", "--quiet", "-m", "feat: 아직 올리지 않은 변경"],
        );
        let head = sha(&git(&w.sb, &agent, &["rev-parse", "HEAD"]));
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();

        let merged = slot
            .merge(&base, &[MergeItem::Local { head: head.clone() }])
            .unwrap();

        let commit = merged_commit(&merged.items[0]);
        assert_eq!(w.parents(&commit), [base, head.clone()]);
        assert_eq!(w.message(&commit), format!("Merge commit '{head}'"));
        assert!(slot.path().join("mine.txt").exists());
    }

    #[test]
    fn a_failed_tree_ref_lets_another_worktree_check_the_commit_out_until_it_is_deleted() {
        let w = World::new();
        let items = [w.open_pr(1, &[("a.txt", "a\n")])];
        let slot = w.slot(SlotName::Batch);
        let merged = w.merge(&slot, &items);
        w.gitops.set_failed_ref("pr-1", &merged.tip).unwrap();
        slot.checkout(&slot.fetch_base().unwrap()).unwrap();

        let agent = w.sb.root().join("agent");
        let full = format!("{FAILED_REF_PREFIX}pr-1");
        w.repo.git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &agent.to_string_lossy(),
            &full,
        ]);
        assert_eq!(fs::read_to_string(agent.join("a.txt")).unwrap(), "a\n");
        assert_eq!(sha(&git(&w.sb, &agent, &["rev-parse", "HEAD"])), merged.tip);

        w.gitops.delete_failed_ref("pr-1").unwrap();
        assert!(!git_succeeds(
            &w.sb,
            &agent,
            &["rev-parse", "--verify", "-q", &full]
        ));
        w.gitops.delete_failed_ref("pr-1").unwrap();
    }

    #[test]
    fn a_failed_tree_ref_name_cannot_escape_its_folder() {
        let w = World::new();
        let tip = sha(&w.repo.git(&["rev-parse", "HEAD"]));
        for name in [
            "", "../x", "a/b", "a b", ".hidden", "x.lock", "x.", "a..b", "-x",
        ] {
            assert!(w.gitops.set_failed_ref(name, &tip).is_err(), "{name:?}");
            assert!(w.gitops.delete_failed_ref(name).is_err(), "{name:?}");
        }
        w.gitops
            .set_failed_ref("b20261115-153000.pr_12", &tip)
            .unwrap();
        w.gitops
            .delete_failed_ref("b20261115-153000.pr_12")
            .unwrap();
    }

    #[test]
    fn preflight_slots_are_separate_worktrees() {
        let w = World::new();
        let first = w.gitops.lock_preflight_slot(2).unwrap();
        let second = w.gitops.lock_preflight_slot(2).unwrap();
        assert_eq!(first.name(), SlotName::Preflight(1));
        assert_eq!(second.name(), SlotName::Preflight(2));
        let (first, second) = (first.open().unwrap(), second.open().unwrap());
        assert_ne!(first.path(), second.path());

        let item = w.open_pr(1, &[("a.txt", "a\n")]);
        let base = first.fetch_base().unwrap();
        let MergeItem::Pr { pr, head, .. } = &item else {
            unreachable!()
        };
        first.fetch_pr_head(*pr, head).unwrap();
        first.checkout(&base).unwrap();
        let merged = second.merge(&base, &[item]).unwrap();

        assert_eq!(first.head().unwrap(), base);
        assert_eq!(second.head().unwrap(), merged.tip);
        assert!(!first.path().join("a.txt").exists());
        assert!(second.path().join("a.txt").exists());
    }

    fn install_hook(w: &World, name: &str, script: &str) {
        let hook = w.repo.path().join(".git/hooks").join(name);
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn the_users_hooks_and_commit_config_do_not_change_a_merge() {
        let w = World::new();
        let marker = w.sb.root().join("post-merge-ran");
        install_hook(&w, "pre-merge-commit", "exit 1");
        install_hook(&w, "commit-msg", "exit 1");
        install_hook(
            &w,
            "prepare-commit-msg",
            "{ printf '[PROJ-1] '; cat \"$1\"; } > \"$1.new\" && mv \"$1.new\" \"$1\"",
        );
        install_hook(&w, "post-merge", &format!("touch '{}'", marker.display()));
        install_hook(&w, "pre-push", "echo 'main is protected' >&2; exit 1");
        w.repo.git(&["config", "commit.cleanup", "strip"]);
        w.repo.git(&["config", "i18n.commitEncoding", "EUC-KR"]);
        let MergeItem::Pr {
            pr, branch, head, ..
        } = w.open_pr(1, &[("a.txt", "a\n")])
        else {
            unreachable!()
        };
        let item = MergeItem::Pr {
            pr,
            branch,
            title: "#42 고친 것".into(),
            head,
        };
        let slot = w.slot(SlotName::Batch);

        let merged = w.merge(&slot, &[item]);

        let commit = merged_commit(&merged.items[0]);
        assert_eq!(
            w.message(&commit),
            "Merge pull request #1 from acme/pr-1\n\n#42 고친 것"
        );
        assert!(!marker.exists(), "post-merge 훅이 돌았다");
        let raw = w.repo.git(&["cat-file", "commit", commit.as_str()]);
        assert!(!raw.contains("\nencoding "), "{raw}");
        assert_eq!(slot.push(&merged.tip).unwrap(), PushOutcome::Pushed);
        assert_eq!(w.origin_main(), merged.tip);
    }

    #[test]
    fn a_recorded_rerere_resolution_does_not_hide_a_conflict() {
        let w = World::new();
        w.repo.git(&["config", "rerere.enabled", "true"]);
        w.repo.git(&["config", "rerere.autoupdate", "true"]);
        let items = [
            w.open_pr(1, &[("shared.txt", "ONE\ntwo\nthree\n")]),
            w.open_pr(2, &[("shared.txt", "uno\ntwo\nthree\n")]),
            w.open_pr(3, &[("c.txt", "c\n")]),
        ];
        let slot = w.slot(SlotName::Batch);
        let first = w.merge(&slot, &items[..1]);
        let MergeItem::Pr { pr, head, .. } = &items[1] else {
            unreachable!()
        };
        slot.fetch_pr_head(*pr, head).unwrap();

        let agent = w.sb.root().join("agent");
        w.repo.git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &agent.to_string_lossy(),
            first.tip.as_str(),
        ]);
        assert!(!git_succeeds(
            &w.sb,
            &agent,
            &["merge", "--no-edit", head.as_str()]
        ));
        fs::write(agent.join("shared.txt"), "resolved\ntwo\nthree\n").unwrap();
        git(&w.sb, &agent, &["add", "shared.txt"]);
        git(&w.sb, &agent, &["commit", "--quiet", "--no-edit"]);

        let merged = w.merge(&slot, &items);

        assert!(matches!(merged.items[0].result, ItemResult::Merged { .. }));
        assert!(matches!(merged.items[1].result, ItemResult::Conflict(_)));
        assert!(matches!(merged.items[2].result, ItemResult::Merged { .. }));
    }

    #[test]
    fn a_push_that_loses_a_race_inside_the_push_is_main_moved() {
        let w = World::new();
        let items = [w.open_pr(1, &[("a.txt", "a\n")])];
        w.peer_git(&["checkout", "--quiet", "-B", "theirs", "origin/main"]);
        let theirs = w.commit_in_peer(&[("n.txt", "n\n")], "feat: 그사이 머지된 변경");
        w.peer_git(&["push", "--quiet", "origin", "theirs:refs/heads/theirs"]);
        let slot = w.slot(SlotName::Batch);
        let merged = w.merge(&slot, &items);
        // A pre-receive hook cannot update refs inside git's quarantine, so the hook leaves it.
        let hook = w.repo.origin().join("hooks/pre-receive");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\nunset GIT_QUARANTINE_PATH GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES\n\
                 git update-ref refs/heads/main {theirs}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(slot.push(&merged.tip).unwrap(), PushOutcome::MainMoved);

        assert_eq!(w.origin_main(), theirs);
    }

    #[test]
    fn a_conflict_against_a_main_made_of_merge_commits_lists_each_change_once() {
        let w = World::new();
        let item = w.open_pr(1, &[("shared.txt", "mine\ntwo\nthree\n")]);
        let mut landed = Vec::new();
        for n in [2, 3] {
            w.peer_git(&["fetch", "--quiet", "origin"]);
            w.peer_git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
            w.peer_git(&["checkout", "--quiet", "-B", &format!("land-{n}")]);
            let head = w.commit_in_peer(
                &[("shared.txt", &format!("p{n}\ntwo\nthree\n"))],
                &format!("feat: landed {n}"),
            );
            w.peer_git(&["checkout", "--quiet", "main"]);
            let merge_message = format!("Merge pull request #{n} from acme/land-{n}");
            w.peer_git(&[
                "merge",
                "--quiet",
                "--no-ff",
                "-m",
                &merge_message,
                &format!("land-{n}"),
            ]);
            w.peer_git(&["push", "--quiet", "origin", "main"]);
            landed.push(head);
        }
        let slot = w.slot(SlotName::Batch);

        let merged = w.merge(&slot, &[item]);

        let ItemResult::Conflict(files) = &merged.items[0].result else {
            panic!("충돌해야 한다: {:?}", merged.items[0].result);
        };
        assert_eq!(
            files[0].main_commits,
            [
                CommitRef {
                    sha: landed[1].clone(),
                    title: "feat: landed 3".into()
                },
                CommitRef {
                    sha: landed[0].clone(),
                    title: "feat: landed 2".into()
                },
            ]
        );
    }

    #[test]
    fn a_conflict_in_a_file_whose_name_is_not_utf8_still_names_the_main_commit() {
        let w = World::new();
        let name = OsStr::from_bytes(b"caf\xe9.txt");
        let write = |content: &str| fs::write(w.peer.join(name), content).unwrap();
        write("one\ntwo\nthree\n");
        w.peer_git(&["add", "."]);
        w.peer_git(&["commit", "--quiet", "-m", "chore: 파일 추가"]);
        w.peer_git(&["push", "--quiet", "origin", "main"]);
        w.peer_git(&["checkout", "--quiet", "-B", "pr-1", "origin/main"]);
        write("mine\ntwo\nthree\n");
        w.peer_git(&["commit", "--quiet", "-am", "feat: 내 변경"]);
        let head = sha(&w.peer_git(&["rev-parse", "HEAD"]));
        w.peer_git(&["push", "--quiet", "origin", "pr-1:refs/pull/1/head"]);
        w.peer_git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
        write("main\ntwo\nthree\n");
        w.peer_git(&["commit", "--quiet", "-am", "fix: main 변경"]);
        let theirs = sha(&w.peer_git(&["rev-parse", "HEAD"]));
        w.peer_git(&["push", "--quiet", "origin", "main"]);
        let item = MergeItem::Pr {
            pr: PrNumber(1),
            branch: "pr-1".into(),
            title: "feat: 내 변경".into(),
            head,
        };
        let slot = w.slot(SlotName::Batch);

        let merged = w.merge(&slot, &[item]);

        let ItemResult::Conflict(files) = &merged.items[0].result else {
            panic!("충돌해야 한다: {:?}", merged.items[0].result);
        };
        assert_eq!(files[0].path, "caf\u{fffd}.txt");
        assert_eq!(
            files[0].main_commits,
            [CommitRef {
                sha: theirs,
                title: "fix: main 변경".into()
            }]
        );
    }

    #[test]
    fn a_users_log_encoding_does_not_garble_commit_titles() {
        let w = World::new();
        w.repo.git(&["config", "i18n.logOutputEncoding", "EUC-KR"]);
        let item = w.open_pr(1, &[("shared.txt", "mine\ntwo\nthree\n")]);
        let theirs = w.advance_main(&[("shared.txt", "main\ntwo\nthree\n")], "fix: 첫째 변경");
        let slot = w.slot(SlotName::Batch);

        let merged = w.merge(&slot, &[item]);

        let ItemResult::Conflict(files) = &merged.items[0].result else {
            panic!("충돌해야 한다: {:?}", merged.items[0].result);
        };
        assert_eq!(
            files[0].main_commits,
            [CommitRef {
                sha: theirs,
                title: "fix: 첫째 변경".into()
            }]
        );
    }

    #[test]
    fn a_registered_head_that_is_not_a_full_sha_is_refused() {
        let w = World::new();
        let MergeItem::Pr { pr, head, .. } = w.open_pr(1, &[("a.txt", "a\n")]) else {
            unreachable!()
        };
        let slot = w.slot(SlotName::Batch);
        let short = Sha::try_from(head.as_str()[..12].to_owned()).unwrap();

        let err = slot.fetch_pr_head(pr, &short).unwrap_err();

        assert!(format!("{err:#}").contains("전체 SHA"), "{err:#}");
    }

    #[test]
    fn a_failed_tree_ref_must_point_at_a_commit() {
        let w = World::new();
        let tree = sha(&w.repo.git(&["rev-parse", "HEAD^{tree}"]));

        assert!(w.gitops.set_failed_ref("t", &tree).is_err());
        assert!(!git_succeeds(
            &w.sb,
            w.repo.path(),
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("{FAILED_REF_PREFIX}t")
            ]
        ));
    }

    const HOOK_CHILD_REPO: &str = "CHUTE_TEST_HOOK_CHILD_REPO";
    const HOOK_CHILD_HEAD: &str = "CHUTE_TEST_HOOK_CHILD_HEAD";

    #[test]
    #[ignore = "run as a child process by slot_work_ignores_the_git_environment_a_hook_passes_on"]
    fn hook_child() {
        let (Some(repo), Some(head)) = (
            std::env::var_os(HOOK_CHILD_REPO),
            std::env::var_os(HOOK_CHILD_HEAD),
        ) else {
            return;
        };
        let ext = Ext::from_vars(|_| None);
        let info = repo::discover(&ext, Path::new(&repo), &|| Ok(Vec::new())).unwrap();
        let gitops = Gitops::new(&ext, &Home::from_env().unwrap(), &info);
        let slot = gitops.lock_preflight_slot(2).unwrap().open().unwrap();
        let base = slot.fetch_base().unwrap();
        let head = sha(&head.to_string_lossy());
        slot.merge(&base, &[MergeItem::Local { head }]).unwrap();
    }

    #[test]
    fn slot_work_ignores_the_git_environment_a_hook_passes_on() {
        let w = World::new();
        let agent = w.sb.root().join("agent");
        w.repo.git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &agent.to_string_lossy(),
        ]);
        fs::write(agent.join("mine.txt"), "mine\n").unwrap();
        git(&w.sb, &agent, &["add", "."]);
        git(
            &w.sb,
            &agent,
            &["commit", "--quiet", "-m", "feat: 에이전트 변경"],
        );
        let state = || {
            (
                git(&w.sb, &agent, &["rev-parse", "HEAD"]),
                git(&w.sb, &agent, &["status", "--porcelain"]),
            )
        };
        let before = state();
        let hook_git_dir = git(&w.sb, &agent, &["rev-parse", "--absolute-git-dir"]);

        let out =
            w.sb.command(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "gitops::tests::hook_child",
                    "--test-threads=1",
                ])
                .env("GIT_DIR", hook_git_dir)
                .env(HOOK_CHILD_REPO, w.repo.path())
                .env(HOOK_CHILD_HEAD, &before.0)
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "user.name")
                .env("GIT_CONFIG_VALUE_0", "Env Person")
                .env_remove("GIT_COMMITTER_NAME")
                .env_remove("GIT_AUTHOR_NAME")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .current_dir(&agent)
                .output()
                .unwrap();

        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
        assert_eq!(state(), before, "에이전트의 worktree가 바뀌었다");
        let slot = w.sb.chute_home().join("slots/acme__widget/preflight-1");
        assert_eq!(
            git(&w.sb, &slot, &["log", "-1", "--format=%cn"]),
            "Env Person"
        );
    }

    #[test]
    fn a_pr_that_stops_an_earlier_one_ignoring_a_cache_path_still_merges() {
        let w = World::new();
        w.peer_git(&["fetch", "--quiet", "origin"]);
        w.peer_git(&["checkout", "--quiet", "-B", "pr-1", "origin/main"]);
        let first = w.commit_in_peer(&[(".gitignore", "\n")], "feat: change 1");
        w.peer_git(&[
            "push",
            "--quiet",
            "origin",
            "pr-1:refs/heads/pr-1",
            "pr-1:refs/pull/1/head",
        ]);
        w.peer_git(&["checkout", "--quiet", "-B", "pr-2", "pr-1"]);
        let second = w.commit_in_peer(&[("target/cache.bin", "tracked\n")], "feat: change 2");
        w.peer_git(&[
            "push",
            "--quiet",
            "origin",
            "pr-2:refs/heads/pr-2",
            "pr-2:refs/pull/2/head",
        ]);
        let item = |n: u64, head: Sha| MergeItem::Pr {
            pr: PrNumber(n),
            branch: format!("pr-{n}"),
            title: format!("feat: change {n}"),
            head,
        };
        let items = [item(1, first), item(2, second)];
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        fs::create_dir_all(slot.path().join("target")).unwrap();
        fs::write(slot.path().join("target/cache.bin"), "cache").unwrap();

        let merged = w.merge(&slot, &items);

        assert!(
            merged
                .items
                .iter()
                .all(|o| matches!(o.result, ItemResult::Merged { .. }))
        );
        let cache = fs::read_to_string(slot.path().join("target/cache.bin")).unwrap();
        assert_eq!(cache, "tracked\n");
    }

    #[test]
    fn a_slot_can_be_made_while_the_users_checkout_is_on_an_unborn_branch() {
        let w = World::new();
        w.repo.git(&["checkout", "--quiet", "--orphan", "scratch"]);

        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();

        assert!(slot.path().join("shared.txt").exists());
        w.repo.git(&["checkout", "--quiet", "--force", "main"]);
    }

    #[test]
    fn a_branch_checked_out_by_hand_in_a_slot_is_not_moved() {
        let w = World::new();
        let slot = w.slot(SlotName::Batch);
        let base = slot.fetch_base().unwrap();
        slot.checkout(&base).unwrap();
        w.in_slot(&slot, &["checkout", "--quiet", "-b", "by-hand"]);
        let newer = w.advance_main(&[("n.txt", "n\n")], "feat: 새 변경");
        assert_eq!(slot.fetch_base().unwrap(), newer);

        slot.checkout(&newer).unwrap();

        assert_eq!(slot.head().unwrap(), newer);
        assert_eq!(sha(&w.repo.git(&["rev-parse", "refs/heads/by-hand"])), base);
        w.repo.git(&["branch", "--quiet", "-D", "by-hand"]);
    }

    const LOCK_CHILD_HOME: &str = "CHUTE_TEST_LOCK_CHILD_HOME";

    fn lock_only_gitops(home: &Path) -> Gitops {
        let info = RepoInfo {
            slug: crate::model::Slug::new("acme", "widget").unwrap(),
            path: home.join("unused-checkout"),
            remote: "unused".into(),
            remote_name: "origin".into(),
            default_branch: "main".into(),
        };
        Gitops::new(&Ext::from_vars(|_| None), &Home::at(home), &info)
    }

    #[test]
    #[ignore = "run as a child process by a_slot_held_by_another_process_cannot_be_taken"]
    fn lock_child() {
        let Some(home) = std::env::var_os(LOCK_CHILD_HOME) else {
            return;
        };
        let lock = lock_only_gitops(Path::new(&home))
            .lock_slot(SlotName::Batch)
            .unwrap();
        println!("lock-child holds={}", lock.is_some());
        std::io::stdin().read_line(&mut String::new()).unwrap();
    }

    struct LockChild {
        child: std::process::Child,
        stdout: std::io::BufReader<std::process::ChildStdout>,
    }

    impl LockChild {
        fn start(home: &Path) -> LockChild {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "gitops::tests::lock_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(LOCK_CHILD_HOME, home)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let stdout = std::io::BufReader::new(child.stdout.take().unwrap());
            LockChild { child, stdout }
        }

        fn got_the_slot(&mut self) -> bool {
            use std::io::BufRead;
            loop {
                let mut line = String::new();
                assert!(
                    self.stdout.read_line(&mut line).unwrap() > 0,
                    "자식이 답하지 않고 끝났다"
                );
                if let Some(answer) = line.trim().split("lock-child holds=").nth(1) {
                    return answer == "true";
                }
            }
        }

        fn release(mut self) {
            drop(self.child.stdin.take());
            self.child.wait().unwrap();
        }
    }

    #[test]
    fn a_slot_held_by_another_process_cannot_be_taken() {
        let dir = tempfile::tempdir().unwrap();
        let git = lock_only_gitops(dir.path());

        let mut holder = LockChild::start(dir.path());
        assert!(holder.got_the_slot());
        assert!(git.lock_slot(SlotName::Batch).unwrap().is_none());
        let mut late = LockChild::start(dir.path());
        assert!(!late.got_the_slot());
        late.release();

        holder.release();
        let until = Instant::now() + Duration::from_secs(5);
        while git.lock_slot(SlotName::Batch).unwrap().is_none() {
            assert!(Instant::now() < until, "슬롯이 풀리지 않았다");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
