use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Output, Stdio};

pub fn run_git<I, S>(cwd: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("failed to invoke git")
}

pub fn git_stdout<I, S>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = run_git(cwd, args)?;
    if !out.status.success() {
        return Err(anyhow!(
            "git failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn rev_parse_head(cwd: &Path) -> Result<String> {
    Ok(git_stdout(cwd, ["rev-parse", "HEAD"])?.trim().to_string())
}

/// Commit that introduced `path` (first commit where the file was added).
pub fn first_commit_for_path(cwd: &Path, path: &str) -> Result<Option<String>> {
    let out = git_stdout(
        cwd,
        ["log", "--diff-filter=A", "--format=%H", "--", path],
    )?;
    let last = out.lines().last().map(str::trim).filter(|s| !s.is_empty());
    Ok(last.map(str::to_string))
}

/// Subjects of commits in `from..to`, oldest first (chronological/topological order).
/// `from` may be `None`, meaning all commits up to `to`.
pub fn commit_subjects(cwd: &Path, from: Option<&str>, to: &str) -> Result<Vec<(String, String)>> {
    let range = match from {
        Some(f) => format!("{}..{}", f, to),
        None => to.to_string(),
    };
    // %H<TAB>%s, then reverse so oldest first.
    let out = git_stdout(
        cwd,
        ["log", "--reverse", "--format=%H%x09%s", &range],
    )?;
    let mut v = Vec::new();
    for line in out.lines() {
        if let Some((sha, subject)) = line.split_once('\t') {
            v.push((sha.to_string(), subject.to_string()));
        }
    }
    Ok(v)
}
