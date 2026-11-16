use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::config;
use crate::ext::Ext;
use crate::model::{RepoInfo, Slug};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub slug: Slug,
}

pub fn slug(ext: &Ext, dir: &Path, registered: &[RepoInfo]) -> Result<Slug> {
    let tree = work_tree(ext, dir)?;
    let all = remotes(ext, dir)?;
    Ok(
        managed(&all, local_remote_key(&tree).as_deref(), registered)?
            .slug
            .clone(),
    )
}

pub fn discover(ext: &Ext, dir: &Path, registered: &[RepoInfo]) -> Result<RepoInfo> {
    let path = work_tree(ext, dir)?;
    let all = remotes(ext, dir)?;
    let chosen = managed(&all, local_remote_key(&path).as_deref(), registered)?;
    let default_branch = default_branch(ext, &path, &chosen.name)?;
    Ok(RepoInfo {
        slug: chosen.slug.clone(),
        path,
        remote: chosen.url.clone(),
        remote_name: chosen.name.clone(),
        default_branch,
    })
}

pub fn remotes(ext: &Ext, dir: &Path) -> Result<Vec<Remote>> {
    let names = ext.git(dir, ["remote"])?.ok()?;
    let mut found = Vec::new();
    for name in names.lines().map(str::trim).filter(|n| !n.is_empty()) {
        let out = ext.git(dir, ["remote", "get-url", name])?;
        if !out.success() {
            continue;
        }
        let url = out.stdout_text().trim().to_owned();
        if let Ok(slug) = parse_slug(&url) {
            found.push(Remote {
                name: name.to_owned(),
                url,
                slug,
            });
        }
    }
    Ok(found)
}

pub fn managed<'a>(
    remotes: &'a [Remote],
    key: Option<&str>,
    registered: &[RepoInfo],
) -> Result<&'a Remote> {
    if let Some(name) = key {
        return remotes.iter().find(|r| r.name == name).ok_or_else(|| {
            anyhow!(
                "chute.toml의 remote {name:?}에 해당하는 git 원격이 없다. `git remote -v`로 확인하고 remote 키를 고친다"
            )
        });
    }
    let known: Vec<&Remote> = remotes
        .iter()
        .filter(|r| registered.iter().any(|info| info.slug == r.slug))
        .collect();
    match (known.as_slice(), remotes) {
        ([one], _) => Ok(one),
        ([], [one]) => Ok(one),
        ([], []) => bail!("GitHub 저장소를 가리키는 git 원격이 없다. `git remote -v`로 확인한다"),
        (candidates, all) => {
            let candidates: Vec<&Remote> = if candidates.is_empty() {
                all.iter().collect()
            } else {
                candidates.to_vec()
            };
            let named: Vec<String> = candidates
                .iter()
                .map(|r| format!("{} ({})", r.name, r.url))
                .collect();
            bail!(
                "chute가 쓸 git 원격을 하나로 정하지 못했다: {}. `git remote -v`로 확인하고 chute.toml에 `remote = \"<이름>\"`을 적는다",
                named.join(", ")
            )
        }
    }
}

pub fn local_remote_key(tree: &Path) -> Option<String> {
    let text = std::fs::read_to_string(tree.join(config::FILE_NAME)).ok()?;
    config::remote_key(&text)
}

/// The main work tree. A bare repository has none and no remote-tracking refs to read the
/// config from, so it is refused.
fn work_tree(ext: &Ext, dir: &Path) -> Result<PathBuf> {
    let text = ext.git(dir, ["worktree", "list", "--porcelain"])?.ok()?;
    let main = text.split("\n\n").next().unwrap_or_default();
    if main.lines().any(|line| line == "bare") {
        bail!(
            "bare 저장소는 지원하지 않는다. 일반 clone에서 실행한다: {}",
            dir.display()
        );
    }
    main.lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("git worktree list의 출력을 읽지 못했다: {text:?}"))
}

fn default_branch(ext: &Ext, repo: &Path, remote: &str) -> Result<String> {
    let text = ext
        .git(repo, ["ls-remote", "--symref", remote, "HEAD"])?
        .ok()
        .with_context(|| format!("{remote}의 기본 브랜치를 묻지 못했다"))?;
    text.lines()
        .find_map(|line| line.strip_prefix("ref: refs/heads/"))
        .and_then(|rest| rest.split('\t').next())
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!("{remote}의 기본 브랜치를 찾지 못했다. {remote}에 브랜치를 하나 이상 올린다")
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

        let info = discover(&Ext::from_vars(|k| sb.var(k)), &linked, &[]).unwrap();
        assert_eq!(info.slug.to_string(), "acme/widget");
        assert_eq!(info.path, repo.path().canonicalize().unwrap());
        assert_eq!(info.default_branch, "main");
        assert_eq!(Path::new(&info.remote), repo.origin());
    }

    fn fork_and_upstream(sb: &Sandbox) -> TestRepo {
        let repo = TestRepo::new(sb, "acme/widget");
        add_remote_clone(sb, &repo, "fork", "me/widget");
        repo.git(&["remote", "rename", "origin", "upstream"]);
        repo.git(&["remote", "rename", "fork", "origin"]);
        repo
    }

    fn add_remote_clone(sb: &Sandbox, repo: &TestRepo, name: &str, slug: &str) {
        let at = sb.root().join("clones").join(format!("{slug}.git"));
        let at_s = at.to_string_lossy();
        let from = repo.origin().to_string_lossy();
        repo.git(&["clone", "--quiet", "--bare", &from, &at_s]);
        repo.git(&["remote", "add", name, &at_s]);
        repo.git(&["fetch", "--quiet", name]);
    }

    fn registered(slug: &str, remote_name: &str) -> RepoInfo {
        RepoInfo {
            slug: parse_slug(slug).unwrap(),
            path: PathBuf::from("/unused"),
            remote: "unused".into(),
            remote_name: remote_name.into(),
            default_branch: "main".into(),
        }
    }

    #[test]
    fn the_remote_key_picks_the_remote_when_several_exist() {
        let sb = Sandbox::new();
        let repo = fork_and_upstream(&sb);
        repo.write("chute.toml", "remote = \"upstream\"\n");

        let info = discover(&Ext::from_vars(|k| sb.var(k)), repo.path(), &[]).unwrap();
        assert_eq!(info.slug.to_string(), "acme/widget");
        assert_eq!(info.remote_name, "upstream");
        assert_eq!(info.default_branch, "main");
    }

    #[test]
    fn several_remotes_without_a_key_are_refused_and_the_error_names_the_fix() {
        let sb = Sandbox::new();
        let repo = fork_and_upstream(&sb);
        let ext = Ext::from_vars(|k| sb.var(k));

        let err = format!("{:#}", discover(&ext, repo.path(), &[]).unwrap_err());
        for needed in ["upstream", "origin", "git remote -v", "remote = "] {
            assert!(err.contains(needed), "{needed}: {err}");
        }
        assert!(slug(&ext, repo.path(), &[]).is_err());
    }

    #[test]
    fn the_registered_repository_picks_the_remote_when_there_is_no_key() {
        let sb = Sandbox::new();
        let repo = fork_and_upstream(&sb);
        let ext = Ext::from_vars(|k| sb.var(k));
        let known = [registered("acme/widget", "upstream")];

        assert_eq!(
            slug(&ext, repo.path(), &known).unwrap().to_string(),
            "acme/widget"
        );
        let info = discover(&ext, repo.path(), &known).unwrap();
        assert_eq!(info.remote_name, "upstream");
    }

    #[test]
    fn two_remotes_of_the_registered_repository_are_refused_until_the_key_picks_one() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        add_remote_clone(&sb, &repo, "mirror", "acme/widget");
        let ext = Ext::from_vars(|k| sb.var(k));
        let known = [registered("acme/widget", "origin")];

        let err = format!("{:#}", discover(&ext, repo.path(), &known).unwrap_err());
        assert!(err.contains("origin") && err.contains("mirror"), "{err}");

        repo.write("chute.toml", "remote = \"mirror\"\n");
        let info = discover(&ext, repo.path(), &known).unwrap();
        assert_eq!(info.remote_name, "mirror");

        repo.write("chute.toml", "remote = \"gone\"\n");
        let err = format!("{:#}", discover(&ext, repo.path(), &known).unwrap_err());
        assert!(err.contains("gone"), "{err}");
    }

    #[test]
    fn ssh_https_and_dot_git_forms_are_one_repository() {
        let remote = |name: &str, url: &str| Remote {
            name: name.into(),
            url: url.into(),
            slug: parse_slug(url).unwrap(),
        };
        let all = [
            remote("a", "git@github.com:acme/widget.git"),
            remote("b", "https://github.com/acme/widget"),
            remote("c", "https://github.com/me/widget.git"),
        ];
        let known = [registered("acme/widget", "a")];

        let err = managed(&all, None, &known).unwrap_err().to_string();
        assert!(err.contains("a (") && err.contains("b ("), "{err}");
        assert!(!err.contains("c ("), "{err}");
        assert_eq!(managed(&all, Some("b"), &known).unwrap().name, "b");
        assert_eq!(managed(&all[2..], None, &[]).unwrap().name, "c");
        assert!(managed(&[], None, &[]).is_err());
    }

    #[test]
    fn a_bare_repository_is_refused() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        let bare = sb.root().join("bare.git");
        let wt = sb.root().join("bare-wt");
        let (bare_s, wt_s) = (bare.to_string_lossy(), wt.to_string_lossy());
        let origin = repo.origin().to_string_lossy();
        repo.git(&["clone", "--quiet", "--bare", &origin, &bare_s]);
        repo.git(&["-C", &bare_s, "worktree", "add", "--quiet", &wt_s, "main"]);
        let ext = Ext::from_vars(|k| sb.var(k));

        for dir in [&wt, &bare] {
            assert!(discover(&ext, dir, &[]).is_err());
            assert!(slug(&ext, dir, &[]).is_err());
        }
    }
}
