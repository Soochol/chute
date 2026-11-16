use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

use super::Ctx;
use crate::config::Config;
use crate::ext::Ext;
use crate::model::{RepoInfo, Slug};
use crate::repo::{self, Remote};

/// Claude Code reads exit code 2 from a PreToolUse hook as "block the tool call and show stderr
/// to the model". Any other non-zero code only shows stderr to the user and lets the call run.
const BLOCK: u8 = 2;
const NOT_JUDGED: u8 = 1;

#[derive(clap::Args, Debug)]
pub struct Args {}

#[derive(Deserialize)]
struct HookInput {
    tool_name: Option<String>,
    tool_input: Option<ToolInput>,
    cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct ToolInput {
    command: Option<String>,
}

pub fn run(_args: Args, ctx: &Ctx) -> Result<ExitCode> {
    match judge(ctx) {
        Ok(None) => Ok(ExitCode::SUCCESS),
        Ok(Some(reason)) => {
            eprintln!("{reason}");
            Ok(ExitCode::from(BLOCK))
        }
        Err(e) => {
            eprintln!("chute guard: 판정하지 못해 막지 않았다: {e:#}");
            Ok(ExitCode::from(NOT_JUDGED))
        }
    }
}

fn judge(ctx: &Ctx) -> Result<Option<String>> {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .context("훅 입력을 읽지 못했다")?;
    let cwd = std::env::current_dir().context("지금 폴더를 알지 못했다")?;
    decide(
        &ctx.ext,
        &raw,
        &cwd,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        &|| ctx.home.repos().unwrap_or_default(),
    )
}

fn decide(
    ext: &Ext,
    raw: &str,
    fallback_cwd: &Path,
    home: Option<&Path>,
    registry: &dyn Fn() -> Vec<RepoInfo>,
) -> Result<Option<String>> {
    let input: HookInput = serde_json::from_str(raw).context("훅 입력이 JSON이 아니다")?;
    if input.tool_name.as_deref() != Some("Bash") {
        return Ok(None);
    }
    let Some(command) = input.tool_input.and_then(|t| t.command) else {
        return Ok(None);
    };
    let cwd = input.cwd.unwrap_or_else(|| fallback_cwd.to_owned());
    for action in actions(&command) {
        let dir = action.dir.resolve(&cwd, home);
        let Some(repo) = Repo::open(ext, &dir, registry) else {
            continue;
        };
        if let Some(reason) = action.kind.verdict(&repo) {
            return Ok(Some(reason));
        }
    }
    Ok(None)
}

#[derive(Debug, PartialEq, Eq)]
struct Action {
    dir: Dir,
    kind: Kind,
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    PrMerge,
    Push(Push),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Push {
    remote: Option<String>,
    all_branches: bool,
    tags_only: bool,
    destinations: Vec<Destination>,
}

#[derive(Debug, PartialEq, Eq)]
enum Destination {
    Branch(String),
    CurrentBranch,
    Pattern,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Dir(Vec<String>);

impl Dir {
    fn then(&self, path: &str) -> Dir {
        let mut steps = self.0.clone();
        steps.push(path.to_owned());
        Dir(steps)
    }

    fn resolve(&self, cwd: &Path, home: Option<&Path>) -> PathBuf {
        self.0.iter().fold(cwd.to_owned(), |dir, step| {
            let at_home = home.filter(|_| step == "~" || step.starts_with("~/"));
            match at_home {
                Some(home) => home.join(step[1..].trim_start_matches('/')),
                None => dir.join(step),
            }
        })
    }
}

struct Repo<'a> {
    ext: &'a Ext,
    dir: PathBuf,
    remotes: Vec<Remote>,
    managed: Option<Remote>,
}

struct Target {
    name: Option<String>,
    slugs: Vec<Slug>,
}

impl<'a> Repo<'a> {
    fn open(ext: &'a Ext, dir: &Path, registry: &dyn Fn() -> Vec<RepoInfo>) -> Option<Repo<'a>> {
        let top = ext.git(dir, ["rev-parse", "--show-toplevel"]).ok()?;
        let top_dir = PathBuf::from(top.stdout_text().trim());
        if !top.success() || !top_dir.join("chute.toml").is_file() {
            return None;
        }
        let remotes = repo::remotes(ext, dir).unwrap_or_default();
        let managed = repo::managed(
            &remotes,
            repo::remote_key(ext, dir, &top_dir, &remotes).as_deref(),
            &registry(),
        )
        .ok()
        .cloned();
        Some(Repo {
            ext,
            dir: dir.to_owned(),
            remotes,
            managed,
        })
    }

    fn config_value(&self, key: &str) -> Option<String> {
        let out = self.ext.git(&self.dir, ["config", "--get", key]).ok()?;
        let value = out.stdout_text().trim().to_owned();
        (out.success() && !value.is_empty()).then_some(value)
    }

    fn current_branch(&self) -> Option<String> {
        let out = self
            .ext
            .git(&self.dir, ["symbolic-ref", "-q", "HEAD"])
            .ok()?;
        let head = out.stdout_text();
        out.success()
            .then(|| head.trim().strip_prefix("refs/heads/").map(str::to_owned))?
    }

    fn push_target_branch(&self) -> Option<String> {
        let out = self
            .ext
            .git(&self.dir, ["rev-parse", "--symbolic-full-name", "@{push}"])
            .ok()?;
        let target = out.stdout_text();
        let tracked = target.trim().strip_prefix("refs/remotes/")?;
        if !out.success() {
            return None;
        }
        let names = self.ext.git(&self.dir, ["remote"]).ok()?.stdout_text();
        names
            .lines()
            .filter_map(|name| tracked.strip_prefix(name)?.strip_prefix('/'))
            .min_by_key(|branch| branch.len())
            .map(str::to_owned)
    }

    fn push_remote(&self, push: &Push) -> Option<String> {
        if let Some(named) = &push.remote {
            return Some(named.clone());
        }
        let branch = self.current_branch();
        let per_branch = |setting: &str| {
            branch
                .as_ref()
                .and_then(|b| self.config_value(&format!("branch.{b}.{setting}")))
        };
        per_branch("pushRemote")
            .or_else(|| self.config_value("remote.pushDefault"))
            .or_else(|| per_branch("remote"))
            .or_else(|| self.has_remote("origin").then(|| "origin".to_owned()))
    }

    fn has_remote(&self, name: &str) -> bool {
        self.ext
            .git(&self.dir, ["remote", "get-url", "--push", name])
            .is_ok_and(|out| out.success())
    }

    fn target(&self, push: &Push) -> Option<Target> {
        let word = self.push_remote(push)?;
        if self.has_remote(&word) {
            let out = self
                .ext
                .git(&self.dir, ["remote", "get-url", "--push", "--all", &word])
                .ok()?;
            let slugs: Vec<Slug> = out
                .stdout_text()
                .lines()
                .filter_map(|url| repo::parse_slug(url.trim()).ok())
                .collect();
            return (!slugs.is_empty()).then_some(Target {
                name: Some(word),
                slugs,
            });
        }
        let slug = repo::parse_slug(&word).ok()?;
        Some(Target {
            name: None,
            slugs: vec![slug],
        })
    }

    fn is_managed(&self, target: &Target) -> bool {
        self.managed
            .as_ref()
            .is_none_or(|m| target.slugs.iter().any(|s| s.same_repo(&m.slug)))
    }

    fn default_remote<'t>(&'t self, target: &'t Target) -> Option<&'t str> {
        target
            .name
            .as_deref()
            .or_else(|| self.managed.as_ref().map(|m| m.name.as_str()))
    }

    fn default_branches(&self, remote: Option<&str>) -> Vec<String> {
        let head = remote.and_then(|name| {
            let out = self
                .ext
                .git(
                    &self.dir,
                    ["symbolic-ref", "-q", &format!("refs/remotes/{name}/HEAD")],
                )
                .ok()?;
            let head = out.stdout_text();
            out.success().then(|| {
                head.trim()
                    .strip_prefix(&format!("refs/remotes/{name}/"))
                    .map(str::to_owned)
            })?
        });
        head.map_or_else(|| vec!["main".to_owned(), "master".to_owned()], |b| vec![b])
    }

    fn requires_head(&self, remotes: &[&str]) -> bool {
        remotes.iter().any(|remote| {
            self.default_branches(Some(remote)).iter().any(|branch| {
                Config::load_remote_branch(self.ext, &self.dir, remote, branch)
                    .ok()
                    .flatten()
                    .is_some_and(|config| config.require_head)
            })
        })
    }

    fn hand_in(&self, remote: Option<&str>) -> &'static str {
        let names: Vec<&str> = match (remote, &self.managed) {
            (Some(name), _) => vec![name],
            (None, Some(managed)) => vec![managed.name.as_str()],
            (None, None) => self.remotes.iter().map(|r| r.name.as_str()).collect(),
        };
        if self.requires_head(&names) {
            "PR 번호와 HEAD를 보고한다(이 저장소는 `require_head = true`라 submit하지 않는다)"
        } else {
            "`chute submit`으로 큐에 넣는다"
        }
    }
}

impl Kind {
    fn verdict(&self, repo: &Repo) -> Option<String> {
        match self {
            Kind::PrMerge => Some(format!(
                "chute: 이 저장소는 chute가 머지한다. `gh pr merge`는 쓰지 않는다. \
                 PR을 올린 뒤 `chute preflight`로 검증하고 {}. \
                 절차는 `chute --skill`에 있다.",
                repo.hand_in(None)
            )),
            Kind::Push(push) => {
                let Some(target) = repo.target(push) else {
                    return Some(
                        "chute: 이 push가 어느 원격으로 가는지 알 수 없어 막는다. \
                         `git push <원격> <브랜치>`처럼 원격 이름을 적는다. \
                         원격 목록은 `git remote -v`로 본다."
                            .to_owned(),
                    );
                };
                if !repo.is_managed(&target) {
                    return None;
                }
                push.hits_default_branch(repo, &target).map(|branch| {
                    format!(
                        "chute: 기본 브랜치({branch})로 직접 push하지 않는다. \
                         PR 브랜치에 push하고 `chute preflight`로 검증한 뒤 {}. \
                         절차는 `chute --skill`에 있다.",
                        repo.hand_in(target.name.as_deref())
                    )
                })
            }
        }
    }
}

impl Push {
    fn hits_default_branch(&self, repo: &Repo, target: &Target) -> Option<String> {
        let defaults = repo.default_branches(repo.default_remote(target));
        if self.all_branches {
            return defaults.first().cloned();
        }
        if self.destinations.is_empty() {
            if self.tags_only {
                return None;
            }
            return [repo.current_branch(), repo.push_target_branch()]
                .into_iter()
                .flatten()
                .find(|b| defaults.contains(b));
        }
        self.destinations.iter().find_map(|destination| {
            let branch = match destination {
                Destination::Branch(name) => Some(name.clone()),
                Destination::CurrentBranch => repo.current_branch(),
                Destination::Pattern => return defaults.first().cloned(),
            }?;
            defaults.contains(&branch).then_some(branch)
        })
    }
}

fn actions(command: &str) -> Vec<Action> {
    let mut found = Vec::new();
    collect(command, &Dir::default(), &mut found);
    found
}

fn collect(command: &str, start: &Dir, found: &mut Vec<Action>) {
    let substitution = Regex::new(r"\$\(([^()]*)\)").unwrap();
    for inner in substitution.captures_iter(command) {
        collect(&inner[1], start, found);
    }
    let mut dir = start.clone();
    let mut outer: Vec<Dir> = Vec::new();
    for words in segments(command) {
        match words.as_slice() {
            [mark] if mark == "(" => {
                outer.push(dir.clone());
                continue;
            }
            [mark] if mark == ")" => {
                dir = outer.pop().unwrap_or(dir);
                continue;
            }
            _ => {}
        }
        let words = strip_wrappers(&words);
        let Some((program, args)) = words.split_first() else {
            continue;
        };
        match basename(program) {
            "cd" => {
                let target = args.iter().find(|a| *a == "-" || !a.starts_with('-'));
                dir = match target.map(String::as_str) {
                    Some("-") => dir,
                    Some(to) => dir.then(to),
                    None => start.clone(),
                };
            }
            "sh" | "bash" | "zsh" | "dash" => {
                if let Some(script) = args
                    .iter()
                    .position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))
                    .and_then(|at| args.get(at + 1))
                {
                    collect(script, &dir, found);
                }
            }
            "eval" => collect(&args.join(" "), &dir, found),
            "gh" => {
                if gh_merges(args) {
                    found.push(Action {
                        dir: dir.clone(),
                        kind: Kind::PrMerge,
                    });
                }
            }
            "git" => {
                if let Some((git_dir, push)) = git_push(args, &dir) {
                    found.push(Action {
                        dir: git_dir,
                        kind: Kind::Push(push),
                    });
                }
            }
            _ => {}
        }
    }
}

fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

const KEYWORDS: [&str; 9] = [
    "if", "then", "else", "elif", "do", "while", "until", "{", "!",
];
const WRAPPERS: [&str; 9] = [
    "env", "command", "exec", "time", "nohup", "sudo", "nice", "timeout", "xargs",
];

fn flag_takes_value(wrapper: &str, flag: &str) -> bool {
    match wrapper {
        "env" => matches!(flag, "-u" | "-C" | "-S"),
        "sudo" => matches!(flag, "-u" | "-g" | "-C" | "-h" | "-p" | "-U"),
        "nice" => flag == "-n",
        "timeout" => matches!(flag, "-s" | "-k"),
        "xargs" => matches!(flag, "-n" | "-I" | "-P" | "-L" | "-d" | "-E" | "-s"),
        _ => false,
    }
}

fn strip_wrappers(words: &[String]) -> &[String] {
    let mut rest = words;
    while let Some(first) = rest.first() {
        let is_assignment = first.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        let wrapper = basename(first);
        if KEYWORDS.contains(&first.as_str()) || is_assignment {
            rest = &rest[1..];
        } else if WRAPPERS.contains(&wrapper) {
            rest = &rest[1..];
            while let Some(flag) = rest.first().filter(|w| w.starts_with('-')) {
                let skip = if flag_takes_value(wrapper, flag) {
                    2
                } else {
                    1
                };
                rest = rest.get(skip..).unwrap_or_default();
            }
            if wrapper == "timeout" {
                rest = rest.get(1..).unwrap_or_default();
            }
        } else {
            break;
        }
    }
    rest
}

fn gh_merges(args: &[String]) -> bool {
    let mut positional = Vec::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "-R" | "--repo" | "--hostname" | "-X" | "--method" | "-H" | "--header" | "-f"
            | "-F" | "--field" | "--raw-field" | "-q" | "--jq" | "-t" | "--template"
            | "--input" => {
                words.next();
            }
            _ if word.starts_with('-') => {}
            _ => positional.push(word.as_str()),
        }
    }
    match positional.as_slice() {
        ["pr", "merge", ..] => true,
        ["api", endpoint, ..] => is_merge_endpoint(endpoint),
        _ => false,
    }
}

fn is_merge_endpoint(endpoint: &str) -> bool {
    let path = endpoint.split('?').next().unwrap_or(endpoint);
    path.contains("/pulls/") && path.trim_end_matches('/').ends_with("/merge")
}

fn git_push(args: &[String], start: &Dir) -> Option<(Dir, Push)> {
    let mut dir = start.clone();
    let mut words = args.iter();
    loop {
        let word = words.next()?;
        match word.as_str() {
            "-C" | "--work-tree" => dir = dir.then(words.next()?),
            "--git-dir" => dir = dir.then(&format!("{}/..", words.next()?)),
            "-c" | "--namespace" | "--super-prefix" | "--config-env" => {
                words.next();
            }
            _ if word.starts_with("--work-tree=") => {
                dir = dir.then(&word["--work-tree=".len()..]);
            }
            _ if word.starts_with("--git-dir=") => {
                dir = dir.then(&format!("{}/..", &word["--git-dir=".len()..]));
            }
            "push" => break,
            _ if word.starts_with('-') => {}
            _ => return None,
        }
    }
    Some((dir, parse_push(words.map(String::as_str))))
}

fn parse_push<'a>(args: impl Iterator<Item = &'a str>) -> Push {
    let mut push = Push::default();
    let mut tags = false;
    let mut repo_flag = None;
    let mut positional = Vec::new();
    let mut args = args;
    let mut options_done = false;
    while let Some(word) = args.next() {
        if options_done || !word.starts_with('-') || word == "-" {
            positional.push(word);
            continue;
        }
        match word {
            "--" => options_done = true,
            "--all" | "--mirror" | "--branches" => push.all_branches = true,
            "--tags" => tags = true,
            "--repo" => repo_flag = args.next().map(str::to_owned),
            "-o" | "--push-option" | "--receive-pack" | "--exec" => {
                args.next();
            }
            _ => {
                if let Some(repo) = word.strip_prefix("--repo=") {
                    repo_flag = Some(repo.to_owned());
                }
            }
        }
    }
    let mut positional = positional.into_iter();
    push.remote = positional.next().map(str::to_owned).or(repo_flag);
    let mut refspecs = positional.peekable();
    push.tags_only = tags && refspecs.peek().is_none();
    push.destinations = refspecs.filter_map(destination).collect();
    push
}

fn destination(refspec: &str) -> Option<Destination> {
    let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
    let target = refspec.split_once(':').map_or(refspec, |(_, dst)| dst);
    let target = target.strip_prefix("refs/").unwrap_or(target);
    let target = target.strip_prefix("heads/").unwrap_or(target);
    match target {
        "" => None,
        "HEAD" => Some(Destination::CurrentBranch),
        t if t.contains(['*', '?', '[']) => Some(Destination::Pattern),
        t => Some(Destination::Branch(t.to_owned())),
    }
}

fn segments(command: &str) -> Vec<Vec<String>> {
    let command = strip_heredocs(command);
    let mut all = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    let end_word = |words: &mut Vec<String>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => word.extend(chars.next()),
                        _ => word.push(q),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(escaped) => {
                    in_word = true;
                    word.push(escaped);
                }
            },
            '#' if !in_word => {
                for rest in chars.by_ref() {
                    if rest == '\n' {
                        break;
                    }
                }
                end_word(&mut words, &mut word, &mut in_word);
                all.push(std::mem::take(&mut words));
            }
            c if c.is_whitespace() && c != '\n' => end_word(&mut words, &mut word, &mut in_word),
            '&' if word.ends_with(['<', '>']) => word.push('&'),
            '(' | ')' => {
                end_word(&mut words, &mut word, &mut in_word);
                all.push(std::mem::take(&mut words));
                all.push(vec![c.to_string()]);
            }
            ';' | '&' | '|' | '`' | '\n' => {
                end_word(&mut words, &mut word, &mut in_word);
                all.push(std::mem::take(&mut words));
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut words, &mut word, &mut in_word);
    all.push(words);
    all.into_iter()
        .map(drop_redirections)
        .filter(|words| !words.is_empty())
        .collect()
}

fn strip_heredocs(command: &str) -> String {
    const LONGEST_BODY: usize = 2000;
    const SHELLS: [&str; 7] = ["sh", "bash", "zsh", "dash", "eval", "source", "xargs"];
    let opener =
        Regex::new(r#"(?:^|[^<])<<-?\s*(?:'([A-Za-z_]\w*)'|"([A-Za-z_]\w*)"|([A-Za-z_]\w*))"#)
            .unwrap();
    let mut kept = Vec::new();
    let mut lines = command.lines();
    while let Some(line) = lines.next() {
        kept.push(line);
        for opened in opener.captures_iter(line) {
            let Some(end) = (1..=3)
                .find_map(|group| opened.get(group))
                .map(|m| m.as_str())
            else {
                continue;
            };
            let whole = opened.get(0).unwrap();
            let prefix = &line[..whole.start() + whole.as_str().find("<<").unwrap_or(0)];
            let quoted_or_commented = line.trim_start().starts_with('#')
                || prefix.matches(['\'', '"']).count() % 2 == 1
                || prefix.contains("((");
            let feeds_a_shell = line
                .split(|c: char| c.is_whitespace() || "|;&()".contains(c))
                .any(|word| SHELLS.contains(&basename(word)));
            let Some(at) = lines
                .clone()
                .take(LONGEST_BODY)
                .position(|l| l.trim() == end)
            else {
                continue;
            };
            let expands = opened.get(3).is_some()
                && lines
                    .clone()
                    .take(at)
                    .any(|l| l.contains("$(") || l.contains('`'));
            if !quoted_or_commented && !feeds_a_shell && !expands {
                lines.nth(at);
            }
        }
    }
    kept.join("\n")
}

fn drop_redirections(words: Vec<String>) -> Vec<String> {
    let is_redirect = |w: &str| {
        w.trim_start_matches(|c: char| c.is_ascii_digit())
            .starts_with(['<', '>'])
    };
    let mut kept = Vec::new();
    let mut words = words.into_iter();
    while let Some(word) = words.next() {
        if is_redirect(&word) {
            let bare = word.trim_start_matches(|c: char| c.is_ascii_digit());
            if bare.chars().all(|c| c == '<' || c == '>') {
                words.next();
            }
        } else {
            kept.push(word);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Sandbox, TestRepo};

    fn thread_cpu_time() -> std::time::Duration {
        let t = nix::time::clock_gettime(nix::time::ClockId::CLOCK_THREAD_CPUTIME_ID).unwrap();
        std::time::Duration::new(t.tv_sec() as u64, t.tv_nsec() as u32)
    }

    fn words(command: &str) -> Vec<Vec<String>> {
        segments(command)
    }

    #[test]
    fn splits_quotes_operators_and_redirections() {
        assert_eq!(
            words(r#"git commit -m "a; b" && echo 'x y' | cat > out.txt 2>&1"#),
            vec![
                vec!["git", "commit", "-m", "a; b"],
                vec!["echo", "x y"],
                vec!["cat"],
            ]
        );
    }

    #[test]
    fn finds_commands_inside_wrappers_and_subshells() {
        for command in [
            "FOO=1 git push origin main",
            "env GIT_TRACE=1 git push origin main",
            "command git push origin main",
            "time git push origin main",
            "/usr/bin/git push origin main",
            "echo $(git push origin main)",
            "echo `git push origin main`",
            "(git push origin main)",
            "bash -c 'git push origin main'",
            "sh -lc 'git push origin main'",
            "eval git push origin main",
            "true && git push origin main",
            "git -C . push origin main",
            "git -c push.default=current push origin main",
        ] {
            assert!(!actions(command).is_empty(), "{command}");
        }
    }

    fn push_of(command: &str) -> Push {
        match actions(command).pop().unwrap().kind {
            Kind::Push(push) => push,
            Kind::PrMerge => panic!("{command}"),
        }
    }

    #[test]
    fn reads_the_pushed_branches() {
        let branch = |name: &str| Destination::Branch(name.to_owned());
        assert_eq!(
            push_of("git push origin main").destinations,
            [branch("main")]
        );
        assert_eq!(
            push_of("git push -u origin HEAD:refs/heads/main").destinations,
            [branch("main")]
        );
        assert_eq!(
            push_of("git push origin +feat:main").destinations,
            [branch("main")]
        );
        assert_eq!(
            push_of("git push origin :main").destinations,
            [branch("main")]
        );
        assert_eq!(
            push_of("git push --delete origin main").destinations,
            [branch("main")]
        );
        assert_eq!(
            push_of("git push origin HEAD").destinations,
            [Destination::CurrentBranch]
        );
        assert_eq!(
            push_of("git push -o ci.skip origin a b").destinations,
            [branch("a"), branch("b")]
        );
        assert!(push_of("git push").destinations.is_empty());
        assert!(push_of("git push origin").destinations.is_empty());
        assert!(push_of("git push --all").all_branches);
        assert!(push_of("git push --mirror origin").all_branches);
        assert!(push_of("git push --tags").tags_only);
        assert!(!push_of("git push --tags origin main").tags_only);
    }

    #[test]
    fn ignores_other_commands() {
        for command in [
            "git status",
            "git log --oneline",
            "git pull origin main",
            "git fetch origin main",
            "gh pr view 3",
            "gh pr create --fill",
            "gh pr checks 3",
            "chute submit 3",
            "chute up",
            "echo git push origin main",
            "cargo test",
        ] {
            assert!(actions(command).is_empty(), "{command}");
        }
    }

    struct Fixture {
        sb: Sandbox,
        repo: TestRepo,
        known: Vec<RepoInfo>,
    }

    impl Fixture {
        fn new(with_chute_toml: bool) -> Fixture {
            let sb = Sandbox::new();
            let repo = TestRepo::new(&sb, "acme/widget");
            repo.git(&["remote", "set-head", "origin", "main"]);
            if with_chute_toml {
                repo.commit_file("chute.toml", "approval = \"agent\"\n", "chore: chute 설정");
            }
            Fixture {
                sb,
                repo,
                known: Vec::new(),
            }
        }

        fn ext(&self) -> Ext {
            Ext::from_vars(|k| self.sb.var(k))
        }

        fn verdict(&self, command: &str) -> Option<String> {
            self.verdict_in(command, self.repo.path())
        }

        fn verdict_in(&self, command: &str, cwd: &Path) -> Option<String> {
            let input = serde_json::json!({
                "tool_name": "Bash",
                "tool_input": {"command": command},
                "cwd": cwd,
            });
            decide(
                &self.ext(),
                &input.to_string(),
                cwd,
                Some(&self.sb.home()),
                &|| self.known.clone(),
            )
            .unwrap()
        }
    }

    const PUSHES_TO_MAIN: [&str; 14] = [
        "git push origin main",
        "git push origin HEAD:main",
        "git push origin HEAD:refs/heads/main",
        "git push -u origin main",
        "git push --force origin main",
        "git push -f origin feat/a:main",
        "git push origin +main",
        "git push origin :main",
        "git push origin --delete main",
        "git push origin HEAD",
        "git push",
        "git push --all origin",
        "git push --mirror",
        "git push origin 'refs/heads/*:refs/heads/*'",
    ];

    #[test]
    fn blocks_pushes_to_the_default_branch_in_a_chute_repository() {
        let fx = Fixture::new(true);
        assert_eq!(fx.repo.git(&["branch", "--show-current"]).trim(), "main");
        for command in PUSHES_TO_MAIN {
            let reason = fx.verdict(command);
            let reason = reason.unwrap_or_else(|| panic!("막히지 않았다: {command}"));
            assert!(reason.contains("chute preflight"), "{reason}");
            assert!(reason.contains("chute submit"), "{reason}");
        }
    }

    #[test]
    fn a_require_head_repository_is_told_to_report_instead_of_to_submit() {
        for (config, reports) in [
            ("require_head = true\n", true),
            ("require_head = false\n", false),
            ("", false),
            ("require_head = true\nnot toml [", false),
        ] {
            let config = format!("{config}[[verify]]\nname = \"t\"\nrun = \"true\"\n");
            let fx = Fixture::new(false);
            fx.repo.commit_file("chute.toml", &config, "chore: 설정");
            fx.repo.git(&["push", "--quiet", "origin", "main"]);
            for command in ["git push origin main", "gh pr merge 12"] {
                let reason = fx.verdict(command).unwrap();
                assert!(reason.contains("chute preflight"), "{reason}");
                assert_eq!(
                    reason.contains("PR 번호와 HEAD를 보고한다"),
                    reports,
                    "{reason}"
                );
                assert_eq!(reason.contains("`chute submit`"), !reports, "{reason}");
            }
        }
    }

    #[test]
    fn require_head_is_read_from_the_default_branch_even_when_origin_head_is_unset() {
        let fx = Fixture::new(false);
        fx.repo.git(&["branch", "-m", "master"]);
        fx.repo.commit_file(
            "chute.toml",
            "require_head = true\n[[verify]]\nname = \"t\"\nrun = \"true\"\n",
            "chore: 설정",
        );
        fx.repo.git(&["push", "--quiet", "origin", "master"]);
        fx.repo.git(&["remote", "set-head", "origin", "-d"]);
        let reason = fx.verdict("git push origin master").unwrap();
        assert!(reason.contains("PR 번호와 HEAD를 보고한다"), "{reason}");
    }

    const CONFIG: &str = "[[verify]]\nname = \"t\"\nrun = \"true\"\n";

    fn forked(top: &str) -> Fixture {
        let fx = Fixture::new(false);
        let at = fx.sb.root().join("clones/me/widget.git");
        let (at_s, from) = (
            at.to_string_lossy().into_owned(),
            fx.repo.origin().to_string_lossy().into_owned(),
        );
        fx.repo.git(&["clone", "--quiet", "--bare", &from, &at_s]);
        fx.repo.git(&["remote", "rename", "origin", "upstream"]);
        fx.repo.git(&["remote", "add", "origin", &at_s]);
        fx.repo.git(&["fetch", "--quiet", "origin"]);
        fx.repo.git(&["remote", "set-head", "origin", "main"]);
        fx.repo
            .commit_file("chute.toml", &format!("{top}{CONFIG}"), "chore: chute 설정");
        fx.repo.git(&["push", "--quiet", "upstream", "main"]);
        fx
    }

    #[test]
    fn only_the_managed_remote_is_guarded_when_origin_is_a_fork() {
        let fx = forked("remote = \"upstream\"\n");
        for command in [
            "git push upstream main",
            "git push upstream HEAD:main",
            "git push --force upstream main",
        ] {
            let reason = fx.verdict(command).unwrap_or_else(|| panic!("{command}"));
            assert!(reason.contains("chute preflight"), "{reason}");
        }
        for command in [
            "git push origin main",
            "git push -f origin HEAD:main",
            "git push origin :main",
            "git push --all origin",
            "git push upstream feat/a",
            "git push origin",
        ] {
            assert_eq!(fx.verdict(command), None, "{command}");
        }
        assert!(fx.verdict("gh pr merge 12").is_some());
    }

    #[test]
    fn the_registered_repository_picks_the_guarded_remote_without_a_key() {
        let mut fx = forked("");
        assert!(
            fx.verdict("git push origin main").is_some(),
            "정할 수 없으면 대상 원격을 지킨다"
        );
        fx.known = vec![RepoInfo {
            slug: repo::parse_slug("acme/widget").unwrap(),
            path: PathBuf::from("/unused"),
            remote: "unused".into(),
            remote_name: "upstream".into(),
            default_branch: "main".into(),
        }];
        assert_eq!(fx.verdict("git push origin main"), None);
        assert!(fx.verdict("git push upstream main").is_some());
    }

    #[test]
    fn a_bare_push_goes_where_git_would_send_it() {
        let fx = forked("remote = \"upstream\"\n");
        fx.repo.git(&["config", "push.default", "current"]);
        assert!(
            fx.verdict("git push").is_some(),
            "main의 upstream은 upstream"
        );
        fx.repo.git(&["branch", "--quiet", "--unset-upstream"]);
        assert_eq!(fx.verdict("git push"), None, "origin이 기본이다");

        fx.repo.git(&["config", "branch.main.remote", "upstream"]);
        assert!(fx.verdict("git push").is_some(), "upstream 원격");
        fx.repo.git(&["config", "remote.pushDefault", "origin"]);
        assert_eq!(fx.verdict("git push"), None, "pushDefault가 먼저다");
        fx.repo
            .git(&["config", "branch.main.pushRemote", "upstream"]);
        assert!(fx.verdict("git push").is_some(), "pushRemote가 먼저다");
        assert_eq!(
            fx.verdict("git push origin"),
            None,
            "명령에 적힌 원격이 먼저다"
        );
        assert!(fx.verdict("git push upstream").is_some());
        assert!(fx.verdict("git push --repo=upstream").is_some());
        assert!(
            fx.verdict("git push --repo=origin main").is_some(),
            "첫 위치 인자가 --repo를 덮는다"
        );
        assert_eq!(fx.verdict("git push --repo=origin"), None);
    }

    #[test]
    fn the_first_positional_is_the_repository_even_with_repo_and_urls_follow_the_managed_head() {
        let fx = forked("remote = \"upstream\"\n");
        for command in [
            "git push --repo origin upstream main",
            "git push --repo=origin upstream main",
        ] {
            assert!(fx.verdict(command).is_some(), "{command}");
        }
        fx.repo.git(&["branch", "-m", "develop"]);
        fx.repo.git(&["push", "--quiet", "upstream", "develop"]);
        fx.repo.git(&["fetch", "--quiet", "upstream"]);
        fx.repo.git(&["remote", "set-head", "upstream", "develop"]);
        for command in [
            "git push upstream develop",
            "git push git@github.com:acme/widget.git develop",
            "git push --repo=https://github.com/acme/widget develop",
        ] {
            assert!(fx.verdict(command).is_some(), "{command}");
        }
    }

    #[test]
    fn owner_and_name_match_without_regard_to_case_and_every_push_url_counts() {
        let fx = forked("remote = \"upstream\"\n");
        for command in [
            "git push https://github.com/Acme/Widget main",
            "git push git@github.com:ACME/widget.git HEAD:main",
        ] {
            assert!(fx.verdict(command).is_some(), "{command}");
        }
        fx.repo.git(&[
            "remote",
            "set-url",
            "--push",
            "upstream",
            "git@github.com:me/widget.git",
        ]);
        assert_eq!(fx.verdict("git push upstream main"), None);
        fx.repo.git(&[
            "remote",
            "set-url",
            "--add",
            "--push",
            "upstream",
            "git@github.com:acme/widget.git",
        ]);
        assert!(fx.verdict("git push upstream main").is_some());
    }

    #[test]
    fn a_remote_name_with_a_slash_still_gives_the_upstream_branch() {
        let fx = forked("remote = \"team/up\"\n");
        fx.repo.git(&["remote", "rename", "upstream", "team/up"]);
        fx.repo.git(&["fetch", "--quiet", "team/up"]);
        fx.repo.git(&["checkout", "--quiet", "-b", "feat"]);
        fx.repo.git(&["branch", "--quiet", "-u", "team/up/main"]);
        fx.repo.git(&["config", "push.default", "upstream"]);
        assert!(fx.verdict("git push").is_some());
    }

    #[test]
    fn a_url_is_matched_by_its_repository() {
        let fx = forked("remote = \"upstream\"\n");
        assert!(
            fx.verdict("git push git@github.com:acme/widget.git main")
                .is_some()
        );
        assert!(
            fx.verdict("git push https://github.com/acme/widget main")
                .is_some()
        );
        assert_eq!(
            fx.verdict("git push https://github.com/me/widget.git main"),
            None
        );
    }

    #[test]
    fn a_push_whose_remote_is_unknown_is_blocked_and_says_why() {
        let fx = Fixture::new(true);
        for command in ["git push nowhere main", "git push nowhere feat/a"] {
            let reason = fx.verdict(command).unwrap_or_else(|| panic!("{command}"));
            assert!(
                reason.contains("어느 원격으로 가는지 알 수 없어"),
                "{reason}"
            );
        }
        fx.repo.git(&["remote", "remove", "origin"]);
        let reason = fx.verdict("git push").unwrap();
        assert!(
            reason.contains("어느 원격으로 가는지 알 수 없어"),
            "{reason}"
        );
    }

    #[test]
    fn the_hand_in_advice_follows_the_remote_the_push_goes_to() {
        let fx = Fixture::new(false);
        let at = fx.sb.root().join("clones/acme/widget.git");
        let (at_s, from) = (
            at.to_string_lossy().into_owned(),
            fx.repo.origin().to_string_lossy().into_owned(),
        );
        fx.repo.git(&["clone", "--quiet", "--bare", &from, &at_s]);
        fx.repo.git(&["remote", "add", "mirror", &at_s]);
        let config = |require_head: bool| format!("require_head = {require_head}\n{CONFIG}");
        fx.repo
            .commit_file("chute.toml", &config(true), "chore: 설정");
        fx.repo.git(&["push", "--quiet", "origin", "main"]);
        fx.repo.git(&["checkout", "--quiet", "-b", "alt", "HEAD~1"]);
        fx.repo
            .commit_file("chute.toml", &config(false), "chore: 설정");
        fx.repo
            .git(&["push", "--quiet", "--force", "mirror", "alt:main"]);
        fx.repo.git(&["fetch", "--quiet", "mirror"]);
        fx.repo.git(&["remote", "set-head", "mirror", "main"]);
        fx.repo.git(&["checkout", "--quiet", "main"]);

        let origin = fx.verdict("git push origin main").unwrap();
        assert!(origin.contains("PR 번호와 HEAD를 보고한다"), "{origin}");
        let mirror = fx.verdict("git push mirror main").unwrap();
        assert!(mirror.contains("`chute submit`"), "{mirror}");
        assert!(!mirror.contains("PR 번호와 HEAD를 보고한다"), "{mirror}");
    }

    #[test]
    fn blocks_gh_pr_merge_and_the_api_merge_endpoint() {
        let fx = Fixture::new(true);
        for command in [
            "gh pr merge 12",
            "gh pr merge 12 --squash --auto",
            "gh -R acme/widget pr merge 12",
            "gh pr merge --admin",
            "gh api -X PUT repos/acme/widget/pulls/12/merge",
        ] {
            let reason = fx.verdict(command);
            assert!(
                reason.is_some_and(|r| r.contains("chute submit")),
                "막히지 않았다: {command}"
            );
        }
    }

    #[test]
    fn lets_pr_branch_pushes_and_other_work_through() {
        let fx = Fixture::new(true);
        for command in [
            "git push origin feat/a",
            "git push -u origin feat/a",
            "git push origin HEAD:feat/a",
            "git push --force-with-lease origin feat/a",
            "git push origin feat/main",
            "git push origin mainline",
            "git push --tags",
            "git push --delete origin feat/a",
            "git pull origin main",
            "gh pr create --fill",
            "gh pr view 12",
            "chute preflight",
            "chute submit 12",
            "git status && cargo test",
        ] {
            assert_eq!(fx.verdict(command), None, "{command}");
        }
    }

    #[test]
    fn a_bare_push_follows_the_branch_that_is_checked_out() {
        let fx = Fixture::new(true);
        fx.repo.git(&["checkout", "--quiet", "-b", "feat/a"]);
        assert_eq!(fx.verdict("git push"), None);
        assert_eq!(fx.verdict("git push origin"), None);
        assert_eq!(fx.verdict("git push origin HEAD"), None);
        assert!(fx.verdict("git push origin HEAD:main").is_some());
        assert!(fx.verdict("git push origin main").is_some());
    }

    #[test]
    fn follows_the_default_branch_name_of_the_clone() {
        let fx = Fixture::new(true);
        fx.repo.git(&["push", "--quiet", "origin", "main:trunk"]);
        fx.repo.git(&["fetch", "--quiet", "origin"]);
        fx.repo.git(&["remote", "set-head", "origin", "trunk"]);
        assert!(fx.verdict("git push origin trunk").is_some());
        assert_eq!(fx.verdict("git push origin main"), None);
    }

    #[test]
    fn a_bare_push_follows_the_upstream_and_survives_a_tag_named_like_the_branch() {
        let fx = Fixture::new(true);
        fx.repo.git(&["tag", "main"]);
        assert!(fx.verdict("git push").is_some(), "main이라는 태그가 있어도");
        assert!(fx.verdict("git push origin HEAD").is_some());

        fx.repo.git(&["checkout", "--quiet", "-b", "feature"]);
        fx.repo.git(&["branch", "--quiet", "-u", "origin/main"]);
        fx.repo.git(&["config", "push.default", "upstream"]);
        assert!(
            fx.verdict("git push").is_some(),
            "upstream이 main인 feature"
        );
        fx.repo.git(&["config", "push.default", "simple"]);
        fx.repo.git(&["branch", "--quiet", "--unset-upstream"]);
        assert_eq!(fx.verdict("git push"), None);
    }

    #[test]
    fn finds_the_default_branch_through_any_remote_name() {
        let fx = Fixture::new(true);
        fx.repo.git(&["push", "--quiet", "origin", "main:trunk"]);
        fx.repo.git(&["fetch", "--quiet", "origin"]);
        fx.repo.git(&["remote", "set-head", "origin", "trunk"]);
        fx.repo.git(&["remote", "rename", "origin", "up"]);
        assert!(fx.verdict("git push up trunk").is_some());
        fx.repo.git(&["branch", "--quiet", "origin/trunk"]);
        assert!(fx.verdict("git push up trunk").is_some());
    }

    #[test]
    fn sees_through_continuations_keywords_and_more_wrappers() {
        let fx = Fixture::new(true);
        for command in [
            "git \\\n  push origin main",
            "gh pr \\\n  merge 5",
            "git push \\\n  origin main",
            "if true; then git push origin main; fi",
            "for i in 1; do git push origin main; done",
            "{ git push origin main; }",
            "! git push origin main",
            "timeout 60 git push origin main",
            "timeout -s KILL 60 git push origin main",
            "nice -n 5 git push origin main",
            "xargs git push origin main",
            "env -u FOO git push origin main",
            "sudo -u root git push origin main",
            "(cd /tmp); git push origin main",
            "(cd /tmp && true); gh pr merge 5",
        ] {
            assert!(fx.verdict(command).is_some(), "막히지 않았다: {command}");
        }
    }

    #[test]
    fn expands_the_home_directory_and_follows_git_dir_and_work_tree() {
        let fx = Fixture::new(true);
        let home = fx.sb.home();
        std::os::unix::fs::symlink(fx.repo.path(), home.join("w")).unwrap();
        let plain = fx.sb.root().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let repo = fx.repo.path().display();
        for command in [
            "cd ~/w && git push origin main".to_owned(),
            "git -C ~/w push origin main".to_owned(),
            format!("git --work-tree={repo} push origin main"),
            format!("git --work-tree {repo} push origin main"),
            format!("git --git-dir={repo}/.git push origin main"),
        ] {
            assert!(
                fx.verdict_in(&command, &plain).is_some(),
                "막히지 않았다: {command}"
            );
        }
    }

    #[test]
    fn heredocs_that_a_shell_runs_or_that_only_look_like_heredocs_are_scanned() {
        let fx = Fixture::new(true);
        for command in [
            "echo '<<EOF'\ngit push origin main\nEOF",
            "echo \"<<EOF\"\ngit push origin main\nEOF",
            "# <<EOF\ngit push origin main\nEOF",
            "x=$((1<<EOF))\ngit push origin main\nEOF",
            "bash <<EOF\ngit push origin main\nEOF",
            "cat <<'EOF' | sh\ngit push origin main\nEOF",
            "cat <<EOF\n$(git push origin main)\nEOF",
            "echo \"$(git push origin main)\"",
            "cd -P . && git push origin main",
            "cd -- . && git push origin main",
            "git push origin feat:heads/main",
            "git push origin feat:refs/heads/main",
        ] {
            assert!(fx.verdict(command).is_some(), "막히지 않았다: {command}");
        }
    }

    #[test]
    fn markdown_and_scripts_written_through_a_quoted_heredoc_are_text() {
        let fx = Fixture::new(true);
        for command in [
            "git commit -m \"$(cat <<'EOF'\n- `git push origin main`\nEOF\n)\"",
            "cat > README.md <<'EOF'\n```\ngit push origin main\n```\nEOF",
            "cat > deploy.sh <<'EOF'\n#!/bin/sh\ngit push origin main\nEOF",
            "cat > bash.md <<'EOF'\ngit push origin main\nEOF",
        ] {
            assert_eq!(fx.verdict(command), None, "{command}");
        }
    }

    #[test]
    fn a_command_with_thousands_of_heredoc_openers_finishes() {
        let fx = Fixture::new(true);
        let command = "cat <<E\n".repeat(5_000);
        let cpu_started = thread_cpu_time();
        let wall_started = std::time::Instant::now();
        assert_eq!(fx.verdict(&command), None);
        let cpu = thread_cpu_time() - cpu_started;
        assert!(cpu < std::time::Duration::from_secs(10), "{cpu:?}");
        let wall = wall_started.elapsed();
        assert!(wall < std::time::Duration::from_secs(120), "{wall:?}");
    }

    #[test]
    fn heredoc_bodies_are_text_not_commands() {
        let fx = Fixture::new(true);
        for command in [
            "cat > notes.md <<EOF\ngit push origin main\nEOF",
            "git commit --file - <<'EOF'\ngit push origin main is forbidden\nEOF",
            "cat <<-EOF\n\tgh pr merge 5 is not allowed\n\tEOF",
            "cat <<<'git push origin main'",
        ] {
            assert_eq!(fx.verdict(command), None, "{command}");
        }
        assert!(
            fx.verdict("cat <<EOF\ntext\nEOF\ngit push origin main")
                .is_some(),
            "heredoc 뒤의 명령은 본다"
        );
        assert!(
            fx.verdict("git push origin main <<EOF\nx\nEOF").is_some(),
            "heredoc를 받는 명령도 본다"
        );
    }

    #[test]
    fn follows_directory_changes_inside_the_command() {
        let fx = Fixture::new(true);
        let elsewhere = fx.sb.root().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let repo = fx.repo.path().to_string_lossy().into_owned();
        assert!(
            fx.verdict_in(&format!("git -C {repo} push origin main"), &elsewhere)
                .is_some()
        );
        assert!(
            fx.verdict_in(&format!("cd {repo} && git push origin main"), &elsewhere)
                .is_some()
        );
        assert!(
            fx.verdict_in(&format!("cd {repo} && gh pr merge 3"), &elsewhere)
                .is_some()
        );
        assert_eq!(
            fx.verdict_in("git push origin main", &elsewhere),
            None,
            "chute 저장소가 아닌 폴더"
        );
        assert_eq!(
            fx.verdict_in(
                &format!(
                    "cd {repo} && cd {} && git push origin main",
                    elsewhere.display()
                ),
                &elsewhere
            ),
            None
        );
    }

    #[test]
    fn blocks_nothing_without_chute_toml_at_the_work_tree_root() {
        let fx = Fixture::new(false);
        for command in PUSHES_TO_MAIN.iter().chain(&["gh pr merge 12"]) {
            assert_eq!(fx.verdict(command), None, "{command}");
        }
        let fx = Fixture::new(true);
        let sub = fx.repo.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(
            fx.verdict_in("git push origin main", &sub).is_some(),
            "하위 폴더에서도 저장소 루트의 chute.toml을 본다"
        );
    }

    #[test]
    fn a_folder_that_is_not_a_repository_blocks_nothing() {
        let fx = Fixture::new(true);
        let plain = fx.sb.root().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(fx.verdict_in("git push origin main", &plain), None);
        assert_eq!(fx.verdict_in("gh pr merge 3", &plain.join("missing")), None);
    }

    #[test]
    fn other_tools_and_empty_input_pass() {
        let fx = Fixture::new(true);
        let ext = fx.ext();
        let cwd = fx.repo.path();
        for raw in [
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a"}}"#,
            r#"{"tool_name":"Bash","tool_input":{}}"#,
            r#"{"tool_name":"Bash"}"#,
            r#"{}"#,
        ] {
            assert_eq!(
                decide(&ext, raw, cwd, None, &Vec::new).unwrap(),
                None,
                "{raw}"
            );
        }
        assert!(decide(&ext, "not json", cwd, None, &Vec::new).is_err());
    }
}
