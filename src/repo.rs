//! Identifies the target repository from a directory inside it or one of its worktrees.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::ext::Ext;
use crate::model::{RepoInfo, Slug};

/// The `owner/name` of `origin`, from any work tree of the repository.
pub fn slug(ext: &Ext, dir: &Path) -> Result<Slug> {
    let url = origin_url(ext, dir)?;
    parse_slug(&url)
}

/// Everything `repo.json` records. Asks `origin` for its default branch.
pub fn discover(ext: &Ext, dir: &Path) -> Result<RepoInfo> {
    let remote = origin_url(ext, dir)?;
    let slug = parse_slug(&remote)?;
    let path = main_worktree(ext, dir)?;
    let default_branch = default_branch(ext, &path)?;
    Ok(RepoInfo {
        slug,
        path,
        remote,
        default_branch,
    })
}

fn origin_url(ext: &Ext, dir: &Path) -> Result<String> {
    let out = ext.git(dir, ["remote", "get-url", "origin"])?;
    if !out.success() {
        bail!(
            "git 원격 origin을 찾지 못했다: {} ({})",
            dir.display(),
            out.stderr_text().trim()
        );
    }
    Ok(out.stdout_text().trim().to_owned())
}

fn main_worktree(ext: &Ext, dir: &Path) -> Result<PathBuf> {
    let text = ext.git(dir, ["worktree", "list", "--porcelain"])?.ok()?;
    text.lines()
        .next()
        .and_then(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("git worktree list의 출력을 읽지 못했다: {text:?}"))
}

fn default_branch(ext: &Ext, repo: &Path) -> Result<String> {
    let text = ext
        .git(repo, ["ls-remote", "--symref", "origin", "HEAD"])?
        .ok()
        .context("origin의 기본 브랜치를 묻지 못했다")?;
    text.lines()
        .find_map(|line| line.strip_prefix("ref: refs/heads/"))
        .and_then(|rest| rest.split('\t').next())
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!("origin의 기본 브랜치를 찾지 못했다. origin에 브랜치를 하나 이상 올린다")
        })
}

/// The last two path segments of a remote URL: `https://github.com/o/n.git`,
/// `git@github.com:o/n`, or a local path ending in `o/n.git`.
pub fn parse_slug(url: &str) -> Result<Slug> {
    let trimmed = url.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let mut parts = trimmed.rsplit(['/', ':']).filter(|p| !p.is_empty());
    match (parts.next(), parts.next()) {
        (Some(name), Some(owner)) => Slug::new(owner, name).map_err(|e| anyhow!(e)),
        _ => bail!("원격 주소에서 owner/name을 찾지 못했다: {url}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Sandbox, TestRepo};

    #[test]
    fn slug_from_common_remote_urls() {
        for url in [
            "https://github.com/Soochol/chute.git",
            "https://github.com/Soochol/chute",
            "git@github.com:Soochol/chute.git",
            "ssh://git@github.com/Soochol/chute/",
            "/tmp/x/remotes/Soochol/chute.git",
        ] {
            assert_eq!(
                parse_slug(url).unwrap().to_string(),
                "Soochol/chute",
                "{url}"
            );
        }
        assert!(parse_slug("chute").is_err());
    }

    #[test]
    fn discovers_from_a_linked_worktree() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        let linked = sb.root().join("linked");
        repo.git(&[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "side",
            &linked.to_string_lossy(),
        ]);

        let info = discover(&Ext::from_vars(|k| sb.var(k)), &linked).unwrap();
        assert_eq!(info.slug.to_string(), "acme/widget");
        assert_eq!(info.path, repo.path().canonicalize().unwrap());
        assert_eq!(info.default_branch, "main");
        assert_eq!(Path::new(&info.remote), repo.origin());
    }
}
