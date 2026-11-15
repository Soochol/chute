//! Git work for the merge queue: reusable slot worktrees, merging PR heads the way GitHub
//! does, and pushing the result. Slots borrow the user's object store and never change the
//! user's HEAD, index, branches or work folder. Fetch, checkout, merge and push run inside a
//! slot; only `worktree add`, the failed-tree refs and one read of the git directory run in
//! the user's checkout.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::ext::{Ext, Output};
use crate::model::{CommitRef, ConflictFile, NonEmpty, PrNumber, RepoInfo, Sha};
use crate::store::Home;

pub const FAILED_REF_PREFIX: &str = "refs/chute/failed/";

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

/// What a merge takes in. A PR is merged by its registered head SHA, never by branch name,
/// so the tree that was verified is the tree that is pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeItem {
    Pr {
        pr: PrNumber,
        branch: String,
        title: String,
        head: Sha,
    },
    /// A commit that is not a PR, such as the head of the worktree running preflight.
    Local { head: Sha },
}

impl MergeItem {
    pub fn head(&self) -> &Sha {
        match self {
            MergeItem::Pr { head, .. } | MergeItem::Local { head } => head,
        }
    }

    /// GitHub's merge commit message: the subject names the PR and branch, the body is the title.
    fn message(&self, owner: &str) -> String {
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
    /// The merge was undone. Each file lists the commits on the target side, since the merge
    /// base, that changed it: main, plus the earlier PRs of the same run.
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
    /// The remote branch moved, so git refused a push that would not be a fast-forward.
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
        Gitops {
            ext: ext.clone(),
            repo: info.path.clone(),
            slots_dir: home.slots_dir(&info.slug),
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

    /// Keeps the commit alive for gc and lets an agent check it out. Overwrites an existing ref.
    pub fn set_failed_ref(&self, name: &str, commit: &Sha) -> Result<()> {
        let full = failed_ref(name)?;
        self.ext
            .git(&self.repo, ["update-ref", full.as_str(), commit.as_str()])?
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

    /// A deleted folder, or one no longer linked to this repository, is rebuilt. The slots
    /// folder is chute's own, so clearing what is left there is safe.
    fn ensure_worktree(&self, path: &Path) -> Result<()> {
        if self.is_worktree_of_repo(path)? {
            return Ok(());
        }
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("지우지 못했다: {}", path.display())),
        }
        let args = [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--quiet"),
            OsStr::new("--force"),
            OsStr::new("--detach"),
            OsStr::new("--no-checkout"),
            path.as_os_str(),
        ];
        self.ext
            .git(&self.repo, args)?
            .ok()
            .with_context(|| format!("슬롯을 만들지 못했다: {}", path.display()))?;
        Ok(())
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

/// Exclusive claim on a slot. It touches no worktree, so holding one starts no process.
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
        let path = self.git.slot_path(self.name);
        self.git.ensure_worktree(&path)?;
        Ok(Slot { path, lock: self })
    }
}

/// A claimed slot with an existing worktree, detached from every branch. Only the holder
/// of the lock can reach these methods.
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
    /// commit, because `chute.toml` is read from it. The tip is read from this slot's own
    /// `FETCH_HEAD`: a fetch that updated the shared ref could fail when two slots fetch at once.
    pub fn fetch_base(&self) -> Result<Sha> {
        let branch = &self.lock.git.default_branch;
        let tip = self
            .fetch(&format!("refs/heads/{branch}"))
            .with_context(|| format!("origin/{branch}를 가져오지 못했다"))?;
        let tracking = format!("refs/remotes/origin/{branch}");
        self.run(&["update-ref", &tracking, tip.as_str()])?;
        Ok(tip)
    }

    /// Fetches `refs/pull/<n>/head` and compares it with the head registered at submit.
    /// `expected` must be a full SHA.
    pub fn fetch_pr_head(&self, pr: PrNumber, expected: &Sha) -> Result<PrHead> {
        let actual = self
            .fetch(&format!("refs/pull/{}/head", pr.0))
            .with_context(|| format!("PR {pr}의 HEAD를 가져오지 못했다"))?;
        Ok(if actual == *expected {
            PrHead::Unchanged
        } else {
            PrHead::Changed { actual }
        })
    }

    /// Sets the worktree to `commit`: tracked files are restored, untracked files are
    /// deleted, ignored files (build caches) stay. Also clears what a killed git left behind.
    pub fn checkout(&self, commit: &Sha) -> Result<()> {
        self.clear_stale_locks()?;
        // Detach first, so a branch someone checked out in here by hand cannot be moved.
        self.run(&["update-ref", "--no-deref", "HEAD", commit.as_str()])
            .and_then(|_| self.run(&["reset", "--quiet", "--hard"]))
            .and_then(|_| self.run(&["clean", "--quiet", "-ffd"]))
            .with_context(|| format!("슬롯 {}을 {commit}으로 맞추지 못했다", self.name()))?;
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

    /// Pushes with no force, so a branch that moved is refused and left as it is.
    pub fn push(&self, commit: &Sha) -> Result<PushOutcome> {
        let git = &self.lock.git;
        let refspec = format!("{commit}:refs/heads/{}", git.default_branch);
        let out = self.git(&["push", "--porcelain", "origin", &refspec])?;
        if out.success() {
            return Ok(PushOutcome::Pushed);
        }
        if main_moved(&out.stdout_text()) {
            return Ok(PushOutcome::MainMoved);
        }
        bail!(
            "{}에 push하지 못했다: {}",
            git.default_branch,
            out.stderr_text().trim()
        )
    }

    fn merge_one(&self, tip: &Sha, item: &MergeItem) -> Result<ItemResult> {
        let head = item.head();
        if self.is_ancestor(head, tip)? {
            return Ok(ItemResult::AlreadyMerged);
        }
        let message = item.message(&self.lock.git.owner);
        let out = self.git(&[
            "merge",
            "--no-ff",
            "--no-edit",
            "--no-log",
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
                "{head}를 합치지 못했다. 충돌한 파일이 없다: {}",
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
        let listed = self.run(&["diff", "--name-only", "--diff-filter=U", "-z"])?;
        let mut paths: Vec<&str> = listed.split('\0').filter(|p| !p.is_empty()).collect();
        paths.dedup();
        let range = format!("{theirs}..{ours}");
        paths
            .into_iter()
            .map(|path| {
                Ok(ConflictFile {
                    path: path.to_owned(),
                    main_commits: self.commits_touching(&range, path)?,
                })
            })
            .collect()
    }

    /// Newest first. Merge commits are left out: they carry no change of their own.
    fn commits_touching(&self, range: &str, path: &str) -> Result<Vec<CommitRef>> {
        let log = self.run(&[
            "--literal-pathspecs",
            "log",
            "--no-merges",
            "--format=%H %s",
            range,
            "--",
            path,
        ])?;
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
        // An empty refmap stops git from updating tracking refs on the side.
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

    /// This process holds the slot lock, so a lock file in the slot's git directory is left
    /// by a git that was killed.
    fn clear_stale_locks(&self) -> Result<()> {
        let git_dir = PathBuf::from(self.run(&["rev-parse", "--absolute-git-dir"])?.trim());
        for name in ["index.lock", "HEAD.lock"] {
            match fs::remove_file(git_dir.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("지우지 못했다: {}", git_dir.join(name).display())
                    });
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
        Ok(self.lock.git.ext.git(&self.path, args)?)
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        Ok(self.lock.git.ext.git(&self.path, args)?.ok()?)
    }
}

/// `git push --porcelain` marks a refused ref with `!` and a reason that is not translated.
fn main_moved(porcelain: &str) -> bool {
    porcelain.lines().any(|line| {
        let mut fields = line.split('\t');
        let (flag, summary) = (fields.next(), fields.nth(1));
        flag == Some("!")
            && summary.is_some_and(|s| {
                s.starts_with("[rejected]")
                    && (s.contains("(non-fast-forward)") || s.contains("(fetch first)"))
            })
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::repo;
    use crate::testkit::{Sandbox, TestRepo};

    /// `Ext` hands git the test process's own environment, which a test must not change.
    /// A wrapper script gives every call the sandbox's HOME, git config and identity instead.
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

    /// What must be the same before and after anything chute does: the user's HEAD, branches,
    /// index and work folder.
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

    /// A sandboxed repository with a bare origin and the user's checkout of it, a second
    /// clone (`peer`) that plays everyone else, and a `Gitops` over the user's checkout.
    /// The user's checkout is never used to make test commits, and every test checks on the
    /// way out that chute left it alone.
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
            // With merge.log set, git appends a shortlog to a merge message. The commit must
            // still read like GitHub's.
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

        /// A branch cut from origin's main with one commit, published as `refs/pull/<n>/head`.
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
        /// The lock semantics themselves are checked in `tests/locks.rs`.
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

        /// What the engine does: fetch the base and each PR's head, then merge.
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
        fs::write(git_dir.join("index.lock"), "").unwrap();
        fs::write(git_dir.join("HEAD.lock"), "").unwrap();

        slot.checkout(&base).unwrap();

        assert_eq!(slot.head().unwrap(), base);
        assert_eq!(w.in_slot(&slot, &["status", "--porcelain"]), "");
        assert!(!git_succeeds(
            &w.sb,
            slot.path(),
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"]
        ));
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
}
