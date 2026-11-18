use std::path::{Component, Path};

use anyhow::{Context, Result};

use super::AGENTS_LINE;
use crate::cmd::Refusal;
use crate::config;
use crate::ext::Ext;
use crate::model::Sha;

const DEFAULT_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
const NEW_FILE: &str = "AGENTS.md";

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Chosen {
    pub file: String,
    pub content: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Missing,
    File,
    Symlink,
    Dir,
    Submodule,
}

pub(super) fn choose(
    ext: &Ext,
    repo: &Path,
    base: &Sha,
    requested: Option<&str>,
) -> Result<Chosen> {
    let lookup = Lookup { ext, repo, base };
    let file = match requested {
        Some(requested) => relative(requested)?,
        None => default_file(&lookup)?,
    };
    for prefix in parent_prefixes(&file) {
        if !matches!(lookup.entry(prefix)?.0, Entry::Dir | Entry::Missing) {
            return Err(refuse(format!(
                "{prefix}가 폴더가 아니라 {file}을 만들 수 없다"
            )));
        }
    }
    let (entry, blob) = lookup.entry(&file)?;
    let existing = match entry {
        Entry::Missing => None,
        Entry::File => Some(lookup.blob(&blob)?),
        Entry::Symlink => {
            return Err(refuse(format!(
                "{file}은 심볼릭 링크다. 내용을 쓰면 링크가 일반 파일로 바뀐다. 링크가 가리키는 파일을 --instructions로 고른다"
            )));
        }
        Entry::Dir => return Err(refuse(format!("{file}은 폴더다. 파일을 고른다"))),
        Entry::Submodule => {
            return Err(refuse(format!("{file}은 서브모듈이다. 파일을 고른다")));
        }
    };
    Ok(Chosen {
        content: with_chute_line(existing.as_deref()),
        file,
    })
}

pub(super) fn with_chute_line(existing: Option<&[u8]>) -> Option<Vec<u8>> {
    let old = existing.unwrap_or_default();
    let present = old
        .split(|b| *b == b'\n')
        .any(|line| line.strip_suffix(b"\r").unwrap_or(line) == AGENTS_LINE.as_bytes());
    if present {
        return None;
    }
    let mut content = old.to_vec();
    if !content.is_empty() && !content.ends_with(b"\n") {
        content.push(b'\n');
    }
    content.extend_from_slice(AGENTS_LINE.as_bytes());
    content.push(b'\n');
    Some(content)
}

fn refuse(message: String) -> anyhow::Error {
    Refusal(message).into()
}

fn relative(requested: &str) -> Result<String> {
    let mut parts = Vec::new();
    for component in Path::new(requested).components() {
        match component {
            Component::Normal(part) => match part.to_str() {
                Some(part) => parts.push(part),
                None => return Err(refuse("--instructions의 경로가 UTF-8이 아니다".into())),
            },
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(refuse(format!(
                    "--instructions에 ..가 든 경로를 줄 수 없다: {requested}"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(refuse(format!(
                    "--instructions에 절대 경로를 줄 수 없다. 저장소 안의 상대 경로를 준다: {requested}"
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(refuse("--instructions에 파일 경로를 줘야 한다".into()));
    }
    if parts.iter().any(|part| part.eq_ignore_ascii_case(".git")) {
        return Err(refuse(format!(
            "--instructions에 .git 안의 경로를 줄 수 없다: {requested}"
        )));
    }
    let file = parts.join("/");
    if file.eq_ignore_ascii_case(config::FILE_NAME) {
        return Err(refuse(format!(
            "--instructions에 {}을 줄 수 없다. 지침 파일을 고른다",
            config::FILE_NAME
        )));
    }
    Ok(file)
}

fn default_file(lookup: &Lookup) -> Result<String> {
    for file in DEFAULT_FILES {
        if lookup.entry(file)?.0 != Entry::Missing {
            return Ok(file.to_owned());
        }
    }
    Ok(NEW_FILE.to_owned())
}

fn parent_prefixes(file: &str) -> Vec<&str> {
    file.match_indices('/').map(|(i, _)| &file[..i]).collect()
}

struct Lookup<'a> {
    ext: &'a Ext,
    repo: &'a Path,
    base: &'a Sha,
}

impl Lookup<'_> {
    fn entry(&self, path: &str) -> Result<(Entry, String)> {
        let out = self
            .ext
            .git(self.repo, ["ls-tree", "-z", self.base.as_str(), "--", path])?
            .ok()
            .with_context(|| format!("{}의 {path}를 읽지 못했다", self.base))?;
        let Some(line) = out.split('\0').find(|line| !line.is_empty()) else {
            return Ok((Entry::Missing, String::new()));
        };
        let (meta, _) = line.split_once('\t').unwrap_or((line, ""));
        let mut fields = meta.split(' ');
        let mode = fields.next().unwrap_or_default();
        let _kind = fields.next();
        let object = fields.next().unwrap_or_default().to_owned();
        let entry = match mode {
            "120000" => Entry::Symlink,
            "040000" => Entry::Dir,
            "160000" => Entry::Submodule,
            _ => Entry::File,
        };
        Ok((entry, object))
    }

    fn blob(&self, object: &str) -> Result<Vec<u8>> {
        let out = self.ext.git(self.repo, ["cat-file", "blob", object])?;
        if !out.success() {
            anyhow::bail!("파일 내용을 읽지 못했다: {}", out.stderr_text().trim());
        }
        Ok(out.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line() -> Vec<u8> {
        format!("{AGENTS_LINE}\n").into_bytes()
    }

    #[test]
    fn the_line_is_added_after_the_content_and_a_missing_final_newline() {
        let with_newline = with_chute_line(Some(b"# rules\n")).unwrap();
        assert_eq!(with_newline, [b"# rules\n".to_vec(), line()].concat());
        let without = with_chute_line(Some(b"# rules")).unwrap();
        assert_eq!(without, [b"# rules\n".to_vec(), line()].concat());
        assert_eq!(with_chute_line(Some(b"")).unwrap(), line());
        assert_eq!(with_chute_line(None).unwrap(), line());
    }

    #[test]
    fn a_file_that_already_has_the_line_is_left_alone() {
        let text = format!("# rules\n{AGENTS_LINE}\nmore");
        assert_eq!(with_chute_line(Some(text.as_bytes())), None);
        let crlf = format!("# rules\r\n{AGENTS_LINE}\r\n");
        assert_eq!(with_chute_line(Some(crlf.as_bytes())), None);
        let inside_a_longer_line = format!("note: {AGENTS_LINE}\n");
        assert!(with_chute_line(Some(inside_a_longer_line.as_bytes())).is_some());
    }

    #[test]
    fn relative_paths_are_cleaned_and_everything_else_is_refused() {
        assert_eq!(relative("docs/./ai.md").unwrap(), "docs/ai.md");
        assert_eq!(relative("./AGENTS.md").unwrap(), "AGENTS.md");
        for bad in [
            "",
            ".",
            "/abs.md",
            "../x.md",
            "a/../b.md",
            "chute.toml",
            "./CHUTE.TOML",
            ".git/info/exclude",
            "sub/.GIT/x",
        ] {
            let err = relative(bad).unwrap_err();
            assert!(err.downcast_ref::<Refusal>().is_some(), "{bad}");
        }
    }

    #[test]
    fn parent_prefixes_lists_every_folder_above_the_file() {
        assert_eq!(parent_prefixes("a/b/c.md"), ["a", "a/b"]);
        assert!(parent_prefixes("c.md").is_empty());
    }
}
