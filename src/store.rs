use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::model::{BatchId, DaemonInfo, Event, PrNumber, RepoInfo, Slug, State};

#[derive(Clone, Debug)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    pub fn from_env() -> Result<Home> {
        Home::from_vars(|key| std::env::var_os(key))
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Home> {
        let var = |key: &str| var(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        let root = if let Some(dir) = var("CHUTE_HOME") {
            dir
        } else if let Some(dir) = var("XDG_STATE_HOME").filter(|d| d.is_absolute()) {
            dir.join("chute")
        } else if let Some(home) = var("HOME") {
            home.join(".local/state/chute")
        } else {
            bail!("상태 폴더를 정하지 못했다. CHUTE_HOME이나 HOME을 설정한다");
        };
        Ok(Home { root })
    }

    pub fn at(root: impl Into<PathBuf>) -> Home {
        Home { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn repo(&self, slug: &Slug) -> RepoStore {
        RepoStore {
            dir: self.root.join("repos").join(slug.dir_name()),
        }
    }

    /// Outside `repos/` because Herdr's `watchFolder`, which the mod points at the state
    /// folder, refuses folders with more than 4096 sub-folders on Linux.
    pub fn slots_dir(&self, slug: &Slug) -> PathBuf {
        self.root.join("slots").join(slug.dir_name())
    }

    pub fn repos(&self) -> Result<Vec<RepoInfo>> {
        let dir = self.root.join("repos");
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("읽지 못했다: {}", dir.display())),
        };
        let mut repos = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            if let Some(info) = read_json::<RepoInfo>(&entry.path().join(REPO_FILE))? {
                repos.push(info);
            }
        }
        repos.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(repos)
    }
}

const REPO_FILE: &str = "repo.json";
const STATE_FILE: &str = "state.json";
const LOCK_FILE: &str = "lock";
const DAEMON_LOCK_FILE: &str = "daemon.lock";
const DAEMON_FILE: &str = "daemon.json";
const HISTORY_FILE: &str = "history.jsonl";

#[derive(Clone, Debug)]
pub struct RepoStore {
    dir: PathBuf,
}

#[derive(Debug)]
pub struct FileLock {
    _file: File,
}

/// `history.jsonl` as read. A line cut short by a crash is skipped, and its number is kept
/// so the caller can warn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    pub events: Vec<Event>,
    pub broken_lines: Vec<usize>,
}

const DAEMON_LOCK_TRIES: u32 = 20;
const DAEMON_LOCK_PAUSE: Duration = Duration::from_millis(10);

impl RepoStore {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn batch_dir(&self, id: &BatchId) -> PathBuf {
        self.dir.join("batches").join(&id.0)
    }

    pub fn report_dir(&self, pr: PrNumber) -> PathBuf {
        self.dir.join("reports").join(format!("pr-{}", pr.0))
    }

    pub fn commit_report_dir(&self, short_sha: &str) -> PathBuf {
        self.dir.join("reports").join(format!("commit-{short_sha}"))
    }

    pub fn register(&self, info: &RepoInfo) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("만들지 못했다: {}", self.dir.display()))?;
        write_json_atomic(&self.dir.join(REPO_FILE), info)
    }

    pub fn info(&self) -> Result<Option<RepoInfo>> {
        read_json(&self.dir.join(REPO_FILE))
    }

    /// A snapshot of `state.json` without the lock. Writers rename into place, so a reader
    /// sees either the old or the new file. A missing file is the empty state; a corrupt one is an error.
    pub fn load(&self) -> Result<State> {
        Ok(read_json(&self.dir.join(STATE_FILE))?.unwrap_or_default())
    }

    pub fn update<T>(&self, change: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let _lock = self.lock(LOCK_FILE)?;
        let mut state = self.load()?;
        let value = change(&mut state)?;
        write_json_atomic(&self.dir.join(STATE_FILE), &state)?;
        Ok(value)
    }

    /// Appends one line. If an earlier write was cut short, the new line still starts on its own line.
    pub fn append_history(&self, event: &Event) -> Result<()> {
        let path = self.dir.join(HISTORY_FILE);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("열지 못했다: {}", path.display()))?;
        let mut line = Vec::new();
        if !ends_with_newline(&mut file)? {
            line.push(b'\n');
        }
        serde_json::to_writer(&mut line, event)?;
        line.push(b'\n');
        file.write_all(&line)
            .with_context(|| format!("쓰지 못했다: {}", path.display()))
    }

    pub fn history(&self) -> Result<History> {
        let path = self.dir.join(HISTORY_FILE);
        let text = match fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(History::default()),
            Err(e) => return Err(e).with_context(|| format!("읽지 못했다: {}", path.display())),
        };
        let mut history = History::default();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(event) => history.events.push(event),
                Err(_) => history.broken_lines.push(i + 1),
            }
        }
        Ok(history)
    }

    pub fn daemon(&self) -> Result<Option<DaemonInfo>> {
        read_json(&self.dir.join(DAEMON_FILE))
    }

    /// The daemon's exclusive lock, or `None` when another daemon holds it. `daemon_running`
    /// holds a shared lock for an instant, so a refusal is retried for a short while before it counts.
    pub fn try_lock_daemon(&self) -> Result<Option<FileLock>> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(DAEMON_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("열지 못했다: {}", path.display()))?;
        for _ in 1..DAEMON_LOCK_TRIES {
            match file.try_lock() {
                Ok(()) => return Ok(Some(FileLock { _file: file })),
                Err(fs::TryLockError::WouldBlock) => thread::sleep(DAEMON_LOCK_PAUSE),
                Err(fs::TryLockError::Error(e)) => return Err(lock_error(e, &path)),
            }
        }
        match file.try_lock() {
            Ok(()) => Ok(Some(FileLock { _file: file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(lock_error(e, &path)),
        }
    }

    /// Probes with a shared lock, so probes never see each other as a daemon.
    pub fn daemon_running(&self) -> Result<bool> {
        let path = self.dir.join(DAEMON_LOCK_FILE);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("열지 못했다: {}", path.display())),
        };
        match file.try_lock_shared() {
            Ok(()) => Ok(false),
            Err(fs::TryLockError::WouldBlock) => Ok(true),
            Err(fs::TryLockError::Error(e)) => Err(lock_error(e, &path)),
        }
    }

    fn lock(&self, name: &str) -> Result<FileLock> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("만들지 못했다: {}", self.dir.display()))?;
        let path = self.dir.join(name);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("열지 못했다: {}", path.display()))?;
        file.lock()
            .with_context(|| format!("잠그지 못했다: {}", path.display()))?;
        Ok(FileLock { _file: file })
    }
}

fn lock_error(e: io::Error, path: &Path) -> anyhow::Error {
    anyhow::Error::new(e).context(format!("잠그지 못했다: {}", path.display()))
}

fn ends_with_newline(file: &mut File) -> io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("읽지 못했다: {}", path.display())),
    };
    serde_json::from_str(&text).map(Some).with_context(|| {
        format!(
            "파일이 깨졌다: {}. 고치거나 지운 뒤 다시 실행한다",
            path.display()
        )
    })
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("상위 폴더가 없다: {}", path.display()))?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let write = || -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(&text)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    };
    write().with_context(|| format!("쓰지 못했다: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EpochMs, EventKind, ItemState, QueueItem, Sha};
    use std::sync::Arc;
    use std::thread;

    fn store() -> (tempfile::TempDir, RepoStore) {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let store = home.repo(&Slug::new("acme", "widget").unwrap());
        (dir, store)
    }

    fn item(pr: u64) -> QueueItem {
        QueueItem {
            pr: PrNumber(pr),
            title: format!("PR {pr}"),
            branch: format!("b{pr}"),
            head: Sha::try_from("abc1234".to_owned()).unwrap(),
            state: ItemState::Queued(None),
            agent: None,
            attempts: 0,
            submitted_ms: EpochMs(0),
            touches_config: false,
            last_failure: None,
        }
    }

    #[test]
    fn root_follows_chute_home_then_xdg_then_home() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.into())
        };
        let root = |pairs| Home::from_vars(vars(pairs)).unwrap().root().to_owned();
        assert_eq!(
            root(&[
                ("CHUTE_HOME", "/c"),
                ("XDG_STATE_HOME", "/x"),
                ("HOME", "/h")
            ]),
            Path::new("/c")
        );
        assert_eq!(
            root(&[("XDG_STATE_HOME", "/x"), ("HOME", "/h")]),
            Path::new("/x/chute")
        );
        assert_eq!(
            root(&[
                ("CHUTE_HOME", ""),
                ("XDG_STATE_HOME", "rel"),
                ("HOME", "/h")
            ]),
            Path::new("/h/.local/state/chute")
        );
        assert!(Home::from_vars(|_| None).is_err());
    }

    #[test]
    fn missing_state_is_empty() {
        let (_dir, store) = store();
        assert_eq!(store.load().unwrap(), State::default());
    }

    #[test]
    fn concurrent_updates_lose_nothing() {
        let (_dir, store) = store();
        let store = Arc::new(store);
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    for i in 0..25 {
                        store
                            .update(|s| {
                                s.queue.push(item(t * 100 + i));
                                Ok(())
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let mut prs: Vec<u64> = store.load().unwrap().queue.iter().map(|i| i.pr.0).collect();
        prs.sort();
        let mut expected: Vec<u64> = (0..8)
            .flat_map(|t| (0..25).map(move |i| t * 100 + i))
            .collect();
        expected.sort();
        assert_eq!(prs, expected);
    }

    #[test]
    fn corrupt_state_is_an_error_and_is_not_overwritten() {
        let (_dir, store) = store();
        fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join(STATE_FILE);
        fs::write(&path, "{\"queue\": [").unwrap();

        assert!(store.load().is_err());
        let mut ran = false;
        assert!(
            store
                .update(|s| {
                    ran = true;
                    s.queue.clear();
                    Ok(())
                })
                .is_err()
        );
        assert!(!ran);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"queue\": [");
    }

    #[test]
    fn failed_change_writes_nothing() {
        let (_dir, store) = store();
        store
            .update(|s| {
                s.queue.push(item(1));
                Ok(())
            })
            .unwrap();
        let err = store.update(|s| -> Result<()> {
            s.queue.clear();
            bail!("no")
        });
        assert!(err.is_err());
        assert_eq!(store.load().unwrap().queue.len(), 1);
    }

    #[test]
    fn history_appends_one_line_per_event() {
        let (_dir, store) = store();
        fs::create_dir_all(store.dir()).unwrap();
        let event = |pr| Event {
            at_ms: EpochMs(pr),
            kind: EventKind::Bounced {
                pr: PrNumber(pr),
                batch: None,
                reason: crate::model::BounceReason::Verify,
                attempts: 1,
            },
        };
        store.append_history(&event(1)).unwrap();
        store.append_history(&event(2)).unwrap();
        assert_eq!(store.history().unwrap().events, [event(1), event(2)]);
        let text = fs::read_to_string(store.dir().join(HISTORY_FILE)).unwrap();
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn a_torn_history_line_is_skipped_and_does_not_swallow_the_next() {
        let (_dir, store) = store();
        fs::create_dir_all(store.dir()).unwrap();
        let event = |pr| Event {
            at_ms: EpochMs(pr),
            kind: EventKind::Bounced {
                pr: PrNumber(pr),
                batch: None,
                reason: crate::model::BounceReason::Verify,
                attempts: 1,
            },
        };
        store.append_history(&event(1)).unwrap();
        let path = store.dir().join(HISTORY_FILE);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"at_ms\":2,\"event\":\"bou").unwrap();
        store.append_history(&event(3)).unwrap();

        let history = store.history().unwrap();
        assert_eq!(history.events, [event(1), event(3)]);
        assert_eq!(history.broken_lines, [2]);
    }

    #[test]
    fn repos_skips_stray_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        fs::create_dir_all(dir.path().join("repos")).unwrap();
        fs::write(dir.path().join("repos/.DS_Store"), "").unwrap();
        assert!(home.repos().unwrap().is_empty());

        let slug = Slug::new("acme", "a").unwrap();
        let elsewhere = dir.path().join("elsewhere");
        Home::at(&elsewhere)
            .repo(&slug)
            .register(&RepoInfo {
                slug: slug.clone(),
                path: "/w/a".into(),
                remote: "git@github.com:acme/a.git".into(),
                default_branch: "main".into(),
            })
            .unwrap();
        std::os::unix::fs::symlink(
            elsewhere.join("repos/acme__a"),
            dir.path().join("repos/acme__a"),
        )
        .unwrap();
        assert_eq!(home.repos().unwrap().len(), 1);
    }

    #[test]
    fn repos_lists_registered_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        assert!(home.repos().unwrap().is_empty());
        for (owner, name) in [("zed", "b"), ("acme", "a")] {
            let slug = Slug::new(owner, name).unwrap();
            home.repo(&slug)
                .register(&RepoInfo {
                    slug: slug.clone(),
                    path: format!("/w/{name}").into(),
                    remote: format!("git@github.com:{slug}.git"),
                    default_branch: "main".into(),
                })
                .unwrap();
        }
        let slugs: Vec<String> = home
            .repos()
            .unwrap()
            .iter()
            .map(|r| r.slug.to_string())
            .collect();
        assert_eq!(slugs, ["acme/a", "zed/b"]);
    }
}
