//! Slots borrow the user's object store and never change the user's HEAD, index, branches or
//! work folder.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::ext::{Ext, Output};
use crate::model::{CommitRef, ConflictFile, NonEmpty, PrNumber, RepoInfo, Sha};
use crate::store::Home;

const TRACKING_ATTEMPTS: u32 = 3;

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
    default_branch: String,
}

impl Gitops {
    pub fn new(ext: &Ext, home: &Home, info: &RepoInfo) -> Gitops {
        let slots_dir = home.slots_dir(&info.slug);
        Gitops {
            ext: ext.clone(),
            repo: info.path.clone(),
            slots_dir: std::path::absolute(&slots_dir).unwrap_or(slots_dir),
            owner: info.slug.owner().to_owned(),
            default_branch: info.default_branch.clone(),
        }
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

    /// The first preflight slot nobody holds. When all are held, the next number is new.
    pub fn lock_preflight_slot(&self) -> Result<SlotLock> {
        for k in 1..=u32::MAX {
            if let Some(lock) = self.lock_slot(SlotName::Preflight(k))? {
                return Ok(lock);
            }
        }
        bail!("preflight 슬롯을 잡지 못했다")
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

    fn slot_path(&self, name: SlotName) -> PathBuf {
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
    pub fn name(&self) -> SlotName {
        self.name
    }

    /// Makes sure the worktree exists, creating or repairing it. Its content is whatever the
    /// last user left; `Slot::checkout` and `Slot::merge` set it.
    pub fn open(self) -> Result<Slot> {
        self.git.ensure_worktree(self.name)?;
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

    /// Fetches the default branch and returns its tip. `origin/<branch>` is moved to the same
    /// commit.
    pub fn fetch_base(&self) -> Result<Sha> {
        let branch = &self.lock.git.default_branch;
        let tracking = format!("refs/remotes/origin/{branch}");
        let before = self.read_ref(&tracking)?;
        let tip = self
            .fetch(&format!("refs/heads/{branch}"))
            .with_context(|| format!("origin/{branch}를 가져오지 못했다"))?;
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
        let out = self.git(&["push", "origin", &refspec])?;
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
            "origin",
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
            let info = repo::discover(&ext, repo.path()).unwrap();
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
                .git(&["log", "-1", "--format=%B", commit.as_str()])
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
        let info = repo::discover(&hermetic_ext(&w.sb), w.repo.path()).unwrap();

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
        let first = w.gitops.lock_preflight_slot().unwrap();
        let second = w.gitops.lock_preflight_slot().unwrap();
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
        let info = repo::discover(&ext, Path::new(&repo)).unwrap();
        let gitops = Gitops::new(&ext, &Home::from_env().unwrap(), &info);
        let slot = gitops.lock_preflight_slot().unwrap().open().unwrap();
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
