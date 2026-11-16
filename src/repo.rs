use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::config;
use crate::ext::Ext;
use crate::model::{RepoInfo, Slug};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub push_url: String,
    pub slug: Slug,
}

pub type Registry<'a> = dyn Fn() -> Result<Vec<RepoInfo>> + 'a;

/// Only asks for the registered repositories when the remote cannot be told apart without
/// them, so an unreadable registration cannot stop a repository that has one remote.
fn known_repos(
    remotes: &[Remote],
    key: Option<&str>,
    registered: &Registry,
) -> Result<Vec<RepoInfo>> {
    if key.is_none() && remotes.len() > 1 {
        registered()
    } else {
        Ok(Vec::new())
    }
}

pub fn slug(ext: &Ext, dir: &Path, registered: &Registry) -> Result<Slug> {
    let tree = work_tree(ext, dir)?;
    let all = remotes(ext, dir)?;
    let key = remote_key(ext, dir, &tree, &all);
    let known = known_repos(&all, key.as_deref(), registered)?;
    Ok(managed(&all, key.as_deref(), &known)?.slug.clone())
}

pub fn discover(ext: &Ext, dir: &Path, registered: &Registry) -> Result<RepoInfo> {
    let path = work_tree(ext, dir)?;
    let all = remotes(ext, dir)?;
    let key = remote_key(ext, dir, &path, &all);
    let known = known_repos(&all, key.as_deref(), registered)?;
    let chosen = managed(&all, key.as_deref(), &known)?;
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
                push_url: url.clone(),
                url,
                slug,
            });
        }
    }
    if found.len() > 1 {
        for remote in &mut found {
            let push = ext.git(dir, ["remote", "get-url", "--push", &remote.name])?;
            if push.success() {
                remote.push_url = push.stdout_text().trim().to_owned();
            }
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
        .filter(|r| registered.iter().any(|info| info.slug.same_repo(&r.slug)))
        .collect();
    let candidates: Vec<&Remote> = if known.is_empty() {
        remotes.iter().collect()
    } else {
        known
    };
    if candidates.is_empty() {
        bail!("GitHub 저장소를 가리키는 git 원격이 없다. `git remote -v`로 확인한다");
    }
    if let Some(chosen) = pick_among_same_url(&candidates) {
        return Ok(chosen);
    }
    let named: Vec<String> = candidates
        .iter()
        .map(|r| {
            if r.push_url == r.url {
                format!("{} ({})", r.name, r.url)
            } else {
                format!("{} ({}, push {})", r.name, r.url, r.push_url)
            }
        })
        .collect();
    bail!(
        "chute가 쓸 git 원격을 하나로 정하지 못했다: {}. `git remote -v`로 확인하고 chute.toml에 `remote = \"<이름>\"`을 적는다",
        named.join(", ")
    )
}

/// Remotes whose normalized URLs are all equal are one repository: `origin` wins, then the
/// first name in order.
fn pick_among_same_url<'a>(candidates: &[&'a Remote]) -> Option<&'a Remote> {
    let first = address(candidates.first()?);
    if candidates.iter().any(|r| address(r) != first) {
        return None;
    }
    candidates
        .iter()
        .copied()
        .min_by_key(|r| (r.name != "origin", r.name.as_str()))
}

pub fn same_url_peers<'a>(remotes: &'a [Remote], chosen: &Remote) -> Vec<&'a Remote> {
    let wanted = address(chosen);
    let mut peers: Vec<&Remote> = remotes.iter().filter(|r| address(r) == wanted).collect();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    peers
}

fn address(remote: &Remote) -> (String, String) {
    (normalize_url(&remote.url), normalize_url(&remote.push_url))
}

/// Drops scheme, user, `.git`, trailing slashes and the scp-style colon, so ssh and https
/// forms of one address compare equal. Hosted addresses are case-insensitive like GitHub, local paths are not.
fn normalize_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let hosted = match trimmed.split_once("://") {
        Some((_, rest)) => rest.to_owned(),
        None if trimmed.contains(':') && !trimmed.starts_with(['/', '.']) => {
            trimmed.replacen(':', "/", 1)
        }
        None => return trimmed.to_owned(),
    };
    let (authority, path) = hosted.split_once('/').unwrap_or((hosted.as_str(), ""));
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() {
        format!("/{path}")
    } else {
        format!("{host}/{path}").to_ascii_lowercase()
    }
}

/// The `remote` key of the work tree's chute.toml. Without one there, a key committed on a
/// remote's default branch counts, so a checkout that lags behind still resolves.
pub fn remote_key(ext: &Ext, dir: &Path, tree: &Path, remotes: &[Remote]) -> Option<String> {
    if let Some(key) = local_remote_key(tree) {
        return Some(key);
    }
    if remotes.len() < 2 {
        return None;
    }
    let mut ordered: Vec<&Remote> = remotes.iter().collect();
    ordered.sort_by_key(|r| (r.name != "origin", r.name.as_str()));
    ordered
        .into_iter()
        .find_map(|r| remote_branch_key(ext, dir, &r.name))
}

fn remote_branch_key(ext: &Ext, dir: &Path, remote: &str) -> Option<String> {
    let head = ext
        .git(
            dir,
            ["symbolic-ref", "-q", &format!("refs/remotes/{remote}/HEAD")],
        )
        .ok()
        .filter(|out| out.success())
        .map(|out| out.stdout_text().trim().to_owned());
    let mut refs: Vec<String> = head.into_iter().collect();
    refs.extend(["main", "master"].map(|b| format!("refs/remotes/{remote}/{b}")));
    refs.iter().find_map(|r| {
        let out = ext
            .git(dir, ["show", &format!("{r}:{}", config::FILE_NAME)])
            .ok()?;
        out.success()
            .then(|| config::remote_key(&out.stdout_text()))?
    })
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

        let info = discover(&Ext::from_vars(|k| sb.var(k)), &linked, &|| Ok(Vec::new())).unwrap();
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

        let info = discover(&Ext::from_vars(|k| sb.var(k)), repo.path(), &|| {
            Ok(Vec::new())
        })
        .unwrap();
        assert_eq!(info.slug.to_string(), "acme/widget");
        assert_eq!(info.remote_name, "upstream");
        assert_eq!(info.default_branch, "main");
    }

    #[test]
    fn several_remotes_without_a_key_are_refused_and_the_error_names_the_fix() {
        let sb = Sandbox::new();
        let repo = fork_and_upstream(&sb);
        let ext = Ext::from_vars(|k| sb.var(k));

        let err = format!(
            "{:#}",
            discover(&ext, repo.path(), &|| Ok(Vec::new())).unwrap_err()
        );
        for needed in ["upstream", "origin", "git remote -v", "remote = "] {
            assert!(err.contains(needed), "{needed}: {err}");
        }
        assert!(slug(&ext, repo.path(), &|| Ok(Vec::new())).is_err());
    }

    #[test]
    fn the_registered_repository_picks_the_remote_when_there_is_no_key() {
        let sb = Sandbox::new();
        let repo = fork_and_upstream(&sb);
        let ext = Ext::from_vars(|k| sb.var(k));
        let known = [registered("acme/widget", "upstream")];

        assert_eq!(
            slug(&ext, repo.path(), &|| Ok(known.to_vec()))
                .unwrap()
                .to_string(),
            "acme/widget"
        );
        let info = discover(&ext, repo.path(), &|| Ok(known.to_vec())).unwrap();
        assert_eq!(info.remote_name, "upstream");
    }

    #[test]
    fn two_remotes_of_the_registered_repository_are_refused_until_the_key_picks_one() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        add_remote_clone(&sb, &repo, "mirror", "acme/widget");
        let ext = Ext::from_vars(|k| sb.var(k));
        let known = [registered("acme/widget", "origin")];

        let err = format!(
            "{:#}",
            discover(&ext, repo.path(), &|| Ok(known.to_vec())).unwrap_err()
        );
        assert!(err.contains("origin") && err.contains("mirror"), "{err}");

        repo.write("chute.toml", "remote = \"mirror\"\n");
        let info = discover(&ext, repo.path(), &|| Ok(known.to_vec())).unwrap();
        assert_eq!(info.remote_name, "mirror");

        repo.write("chute.toml", "remote = \"gone\"\n");
        let err = format!(
            "{:#}",
            discover(&ext, repo.path(), &|| Ok(known.to_vec())).unwrap_err()
        );
        assert!(err.contains("gone"), "{err}");
    }

    #[test]
    fn ssh_https_and_dot_git_forms_are_one_repository() {
        let remote = |name: &str, url: &str| Remote {
            name: name.into(),
            url: url.into(),
            push_url: url.into(),
            slug: parse_slug(url).unwrap(),
        };
        let all = [
            remote("a", "git@github.com:acme/widget.git"),
            remote("b", "https://github.com/acme/widget"),
            remote("c", "https://github.com/me/widget.git"),
        ];
        let known = [registered("acme/widget", "a")];

        assert_eq!(managed(&all, None, &known).unwrap().name, "a");
        assert_eq!(managed(&all, Some("b"), &known).unwrap().name, "b");
        let err = managed(&all, None, &[]).unwrap_err().to_string();
        assert!(err.contains("c ("), "{err}");
        assert_eq!(managed(&all[2..], None, &[]).unwrap().name, "c");
        assert!(managed(&[], None, &[]).is_err());
    }

    fn add_same_url_remote(repo: &TestRepo, name: &str) {
        let url = repo.origin().to_string_lossy();
        repo.git(&["remote", "add", name, &url]);
    }

    #[test]
    fn remotes_with_one_url_pick_origin() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        for name in ["aaa", "paseo-pr-109", "zzz"] {
            add_same_url_remote(&repo, name);
        }
        let ext = Ext::from_vars(|k| sb.var(k));

        let info = discover(&ext, repo.path(), &|| Ok(Vec::new())).unwrap();
        assert_eq!(info.remote_name, "origin");
        assert_eq!(
            slug(&ext, repo.path(), &|| Ok(Vec::new())).unwrap(),
            info.slug
        );
    }

    #[test]
    fn remotes_with_one_url_and_no_origin_pick_the_first_name() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        repo.git(&["remote", "rename", "origin", "zeta"]);
        for name in ["mid", "alpha"] {
            add_same_url_remote(&repo, name);
        }
        let ext = Ext::from_vars(|k| sb.var(k));

        let info = discover(&ext, repo.path(), &|| Ok(Vec::new())).unwrap();
        assert_eq!(info.remote_name, "alpha");
    }

    #[test]
    fn remotes_with_different_urls_are_still_refused_without_a_key() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        add_remote_clone(&sb, &repo, "mirror", "acme/widget");
        add_same_url_remote(&repo, "twin");
        let ext = Ext::from_vars(|k| sb.var(k));

        let err = format!(
            "{:#}",
            discover(&ext, repo.path(), &|| Ok(Vec::new())).unwrap_err()
        );
        for needed in ["origin", "mirror", "twin", "remote = "] {
            assert!(err.contains(needed), "{needed}: {err}");
        }
    }

    #[test]
    fn the_key_on_a_remote_default_branch_is_used_when_the_work_tree_has_no_config() {
        let sb = Sandbox::new();
        let repo = fork_and_upstream(&sb);
        repo.commit_file("chute.toml", "remote = \"upstream\"\n", "chore: 설정");
        repo.git(&["push", "--quiet", "upstream", "main"]);
        repo.git(&["reset", "--quiet", "--hard", "HEAD~1"]);
        assert!(!repo.path().join("chute.toml").exists());
        let ext = Ext::from_vars(|k| sb.var(k));

        let info = discover(&ext, repo.path(), &|| Ok(Vec::new())).unwrap();
        assert_eq!(info.remote_name, "upstream");
    }

    #[test]
    fn a_different_push_url_makes_remotes_of_one_fetch_url_differ() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        add_same_url_remote(&repo, "twin");
        let ext = Ext::from_vars(|k| sb.var(k));
        assert!(discover(&ext, repo.path(), &|| Ok(Vec::new())).is_ok());

        let elsewhere = sb.root().join("elsewhere/acme/widget.git");
        repo.git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            &elsewhere.to_string_lossy(),
        ]);
        let err = format!(
            "{:#}",
            discover(&ext, repo.path(), &|| Ok(Vec::new())).unwrap_err()
        );
        assert!(err.contains("push "), "{err}");
    }

    #[test]
    fn urls_that_differ_only_in_form_normalize_equal() {
        let forms = [
            "git@github.com:acme/widget.git",
            "https://github.com/acme/widget",
            "ssh://git@GitHub.com/Acme/Widget/",
        ];
        assert!(
            forms
                .iter()
                .all(|u| normalize_url(u) == normalize_url(forms[0]))
        );
        assert_ne!(
            normalize_url("../w/Acme/widget.git"),
            normalize_url("../w/acme/widget.git")
        );
        assert_ne!(
            normalize_url("https://github.com/acme/widget"),
            normalize_url("https://github.com/me/widget")
        );
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
            assert!(discover(&ext, dir, &|| Ok(Vec::new())).is_err());
            assert!(slug(&ext, dir, &|| Ok(Vec::new())).is_err());
        }
    }
}
