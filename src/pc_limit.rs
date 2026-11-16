use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::verify::Cancel;

pub const FILE_NAME: &str = "machine.toml";
pub const DEFAULT_MAX_VERIFY: u32 = 3;
const SLOT_DIR: &str = "machine";
const POLL: Duration = Duration::from_millis(50);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default = "default_max_verify")]
    max_verify: u32,
}

fn default_max_verify() -> u32 {
    DEFAULT_MAX_VERIFY
}

pub fn max_verify(home: &Path) -> Result<u32> {
    let path = home.join(FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(DEFAULT_MAX_VERIFY),
        Err(e) => return Err(e).with_context(|| format!("읽지 못했다: {}", path.display())),
    };
    let raw: Raw = toml::from_str(&text)
        .map_err(|e| anyhow!("{}을 읽지 못했다: {}", path.display(), e.message()))?;
    if raw.max_verify == 0 {
        bail!("{}의 max_verify는 1 이상이어야 한다", path.display());
    }
    Ok(raw.max_verify)
}

#[derive(Debug)]
pub enum Slot {
    Held(File),
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Begin { running: u32 },
    End,
}

/// Takes one of `max_verify` slot locks under `<home>/machine/`, waiting while all are held.
/// `on_wait` is told once when the wait starts and once when it ends.
pub fn acquire(home: &Path, cancel: &Cancel, on_wait: Option<&dyn Fn(Wait)>) -> Result<Slot> {
    let max = max_verify(home)?;
    let dir = home.join(SLOT_DIR);
    fs::create_dir_all(&dir).with_context(|| format!("만들지 못했다: {}", dir.display()))?;
    if let Some(file) = try_any(&dir, max)? {
        return Ok(Slot::Held(file));
    }
    if let Some(on_wait) = on_wait {
        on_wait(Wait::Begin { running: max });
    }
    let slot = loop {
        if cancel.is_cancelled() {
            break Slot::Cancelled;
        }
        thread::sleep(POLL);
        if let Some(file) = try_any(&dir, max)? {
            break Slot::Held(file);
        }
    };
    if let Some(on_wait) = on_wait {
        on_wait(Wait::End);
    }
    Ok(slot)
}

fn try_any(dir: &Path, max: u32) -> Result<Option<File>> {
    for k in 1..=max {
        let path = dir.join(format!("verify-{k}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("열지 못했다: {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("잠그지 못했다: {}", path.display()));
            }
        }
    }
    Ok(None)
}
