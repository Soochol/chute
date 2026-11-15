use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::ext::Ext;
use crate::model::{FailedCheck, NonEmpty, PrNumber};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Pending,
    NoChecks,
    Failed(NonEmpty<FailedCheck>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bucket {
    Pass,
    Skipping,
    Pending,
    Fail,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Check {
    pub name: String,
    #[serde(default)]
    pub link: String,
    pub bucket: Bucket,
}

pub fn judge(checks: &[Check]) -> Verdict {
    let failed: Vec<FailedCheck> = checks
        .iter()
        .filter(|c| matches!(c.bucket, Bucket::Fail | Bucket::Cancel))
        .map(|c| FailedCheck {
            name: c.name.clone(),
            link: c.link.clone(),
        })
        .collect();
    if let Some(failed) = NonEmpty::new(failed) {
        Verdict::Failed(failed)
    } else if checks.iter().any(|c| c.bucket == Bucket::Pending) {
        Verdict::Pending
    } else if checks.is_empty() {
        Verdict::NoChecks
    } else {
        Verdict::Passed
    }
}

pub fn parse(stdout: &str) -> Result<Vec<Check>> {
    serde_json::from_str(stdout).context("gh pr checks의 출력을 읽지 못했다")
}

/// Reads the checks GitHub reports for the PR's current head.
///
/// `gh pr checks` exits 8 while checks run and 1 when one failed, both with the JSON on
/// stdout. With no checks at all it prints an error and no JSON.
pub fn read(ext: &Ext, repo: &Path, pr: PrNumber) -> Result<Verdict> {
    let out = ext.gh(
        repo,
        [
            "pr",
            "checks",
            &pr.0.to_string(),
            "--json",
            "name,link,bucket",
        ],
    )?;
    let text = out.stdout_text();
    if matches!(out.code, Some(0 | 1 | 8)) && !text.trim().is_empty() {
        return Ok(judge(&parse(&text)?));
    }
    if out.code == Some(1) && out.stderr_text().contains("no checks reported") {
        return Ok(Verdict::NoChecks);
    }
    bail!(
        "{pr}의 check를 gh로 읽지 못했다: {}",
        out.stderr_text().trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, bucket: Bucket) -> Check {
        Check {
            name: name.into(),
            link: format!("https://ci.example/{name}"),
            bucket,
        }
    }

    fn judged(buckets: &[Bucket]) -> Verdict {
        let checks: Vec<Check> = buckets
            .iter()
            .enumerate()
            .map(|(i, b)| check(&format!("c{i}"), *b))
            .collect();
        judge(&checks)
    }

    #[test]
    fn all_passed_or_skipped_passes() {
        assert_eq!(judged(&[Bucket::Pass, Bucket::Pass]), Verdict::Passed);
        assert_eq!(judged(&[Bucket::Pass, Bucket::Skipping]), Verdict::Passed);
        assert_eq!(judged(&[Bucket::Skipping]), Verdict::Passed);
    }

    #[test]
    fn a_running_check_makes_it_pending() {
        assert_eq!(judged(&[Bucket::Pass, Bucket::Pending]), Verdict::Pending);
    }

    #[test]
    fn no_checks_is_its_own_verdict() {
        assert_eq!(judged(&[]), Verdict::NoChecks);
    }

    #[test]
    fn a_failed_or_cancelled_check_fails_even_with_running_ones() {
        for bad in [Bucket::Fail, Bucket::Cancel] {
            for others in [vec![Bucket::Pass], vec![Bucket::Pending, Bucket::Pass]] {
                let mut buckets = others;
                buckets.push(bad);
                match judged(&buckets) {
                    Verdict::Failed(failed) => {
                        assert_eq!(failed.len(), 1);
                        assert_eq!(
                            failed.first().link,
                            format!("https://ci.example/c{}", buckets.len() - 1)
                        );
                    }
                    other => panic!("{buckets:?}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn the_gh_json_shape_parses_and_a_missing_link_is_empty() {
        let checks = parse(
            r#"[{"name":"build","bucket":"fail","link":"https://x/1","state":"FAILURE"},
                {"name":"lint","bucket":"pass"}]"#,
        )
        .unwrap();
        assert_eq!(checks[0], check_with("build", "https://x/1", Bucket::Fail));
        assert_eq!(checks[1].link, "");
        assert!(parse("not json").is_err());
    }

    #[test]
    fn the_seven_planned_cases_judge_as_planned() {
        use Bucket::*;
        let cases: [(&str, &[Bucket], &str); 7] = [
            ("all passed", &[Pass, Pass], "passed"),
            ("skipped and passed", &[Skipping, Pass], "passed"),
            ("running", &[Pass, Pending], "pending"),
            ("none", &[], "no_checks"),
            ("failed", &[Pass, Fail], "failed"),
            ("cancelled", &[Pass, Cancel], "failed"),
            ("failed and running", &[Pending, Fail], "failed"),
        ];
        for (what, buckets, expected) in cases {
            let got = match judged(buckets) {
                Verdict::Passed => "passed",
                Verdict::Pending => "pending",
                Verdict::NoChecks => "no_checks",
                Verdict::Failed(_) => "failed",
            };
            assert_eq!(got, expected, "{what}");
        }
    }

    fn check_with(name: &str, link: &str, bucket: Bucket) -> Check {
        Check {
            name: name.into(),
            link: link.into(),
            bucket,
        }
    }
}
