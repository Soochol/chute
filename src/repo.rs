use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::ext::Ext;
use crate::model::{RepoInfo, Slug};

pub fn slug(ext: &Ext, dir: &Path) -> Result<Slug> {
    let url = origin_url(ext, dir)?;
    parse_slug(&url)
}

pub fn discover(ext: &Ext, dir: &Path) -> Result<RepoInfo> {
    let remote = origin_url(ext, dir)?;
    let slug = parse_slug(&remote)?;
    let path = work_tree(ext, dir)?;
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

/// The main work tree, or the current one when the repository itself is bare.
fn work_tree(ext: &Ext, dir: &Path) -> Result<PathBuf> {
    let text = ext.git(dir, ["worktree", "list", "--porcelain"])?.ok()?;
    let main = text.split("\n\n").next().unwrap_or_default();
    let path = main
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .ok_or_else(|| anyhow!("git worktree list의 출력을 읽지 못했다: {text:?}"))?;
    if !main.lines().any(|line| line == "bare") {
        return Ok(PathBuf::from(path));
    }
    let top = ext.git(dir, ["rev-parse", "--show-toplevel"])?;
    if !top.success() {
        bail!(
            "bare 저장소 안이다. worktree 안에서 실행한다: {}",
            dir.display()
        );
    }
    Ok(PathBuf::from(top.stdout_text().trim()))
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

    #[test]
    fn a_bare_repository_uses_the_worktree_it_runs_in() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        let bare = sb.root().join("bare.git");
        let wt = sb.root().join("bare-wt");
        repo.git(&[
            "clone",
            "--quiet",
            "--bare",
            &repo.origin().to_string_lossy(),
            &bare.to_string_lossy(),
        ]);
        repo.git(&[
            "-C",
            &bare.to_string_lossy(),
            "worktree",
            "add",
            "--quiet",
            &wt.to_string_lossy(),
            "main",
        ]);
        let ext = Ext::from_vars(|k| sb.var(k));

        assert_eq!(
            discover(&ext, &wt).unwrap().path,
            wt.canonicalize().unwrap()
        );
        assert!(discover(&ext, &bare).is_err());
    }
}
