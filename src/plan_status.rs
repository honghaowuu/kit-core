use anyhow::{anyhow, Context, Result};
use clap::Parser;
use pulldown_cmark::{Event, HeadingLevel, Parser as MdParser, Tag, TagEnd};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::git;

#[derive(Parser)]
pub struct Args {
    /// Specific run dir; omit to use the lexicographically latest under .jkit/.
    #[arg(long)]
    run: Option<PathBuf>,
}

#[derive(Serialize)]
struct Task {
    index: usize,
    title: String,
    completed: bool,
    commit_sha: Option<String>,
}

#[derive(Serialize)]
struct Output {
    run_dir: Option<String>,
    plan_path: Option<String>,
    baseline_sha: Option<String>,
    head_sha: Option<String>,
    spec_sync_behind_head: bool,
    tasks: Vec<Task>,
    next_pending_task_index: Option<usize>,
    recommendation: &'static str,
}

const REC_NO_PLAN: &str = "no_plan";
const REC_ALREADY_SYNCED: &str = "already_synced";
const REC_IMPLEMENT: &str = "implement_from_plan";

pub fn run(args: Args) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;
    let output = compute(&cwd, args.run.as_deref())?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(ExitCode::SUCCESS)
}

fn compute(cwd: &Path, run_arg: Option<&Path>) -> Result<Output> {
    // 1. Resolve run dir.
    let run_dir = match run_arg {
        Some(p) => {
            let abs = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
            if !abs.is_dir() {
                return Err(anyhow!("run dir not found: {}", p.display()));
            }
            Some(abs)
        }
        None => latest_run_dir(&cwd.join(".jkit"))?,
    };

    let Some(run_dir) = run_dir else {
        return Ok(no_plan(None));
    };

    let plan_path = run_dir.join("plan.md");
    if !plan_path.is_file() {
        return Ok(no_plan(Some(rel(cwd, &run_dir))));
    }

    let plan_text = std::fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;

    let parsed = parse_plan_tasks(&plan_text);

    // No `## Tasks` heading at all → no_plan.
    if !parsed.has_tasks_heading {
        return Ok(Output {
            run_dir: Some(rel(cwd, &run_dir)),
            plan_path: Some(rel(cwd, &plan_path)),
            baseline_sha: None,
            head_sha: None,
            spec_sync_behind_head: false,
            tasks: Vec::new(),
            next_pending_task_index: None,
            recommendation: REC_NO_PLAN,
        });
    }

    // Git data.
    let head_sha = git::rev_parse_head(cwd).ok();
    let baseline_sha =
        git::first_commit_for_path(cwd, &rel(cwd, &plan_path)).unwrap_or(None);

    // Spec sync behind head?
    let spec_sync_path = cwd.join(".jkit/spec-sync");
    let spec_sync_behind_head = if spec_sync_path.is_file() {
        let content = std::fs::read_to_string(&spec_sync_path)
            .unwrap_or_default()
            .trim()
            .to_string();
        match (content.is_empty(), head_sha.as_deref()) {
            (true, _) => true,
            (false, Some(h)) => content != h,
            (false, None) => false,
        }
    } else {
        // Missing: behind iff any commits exist after baseline.
        match (&baseline_sha, &head_sha) {
            (Some(b), Some(h)) => b != h,
            (None, Some(_)) => true,
            _ => false,
        }
    };

    // Walk impl commits.
    let impl_commits = if let Some(head) = head_sha.as_deref() {
        let from = baseline_sha.as_deref();
        let subjects = git::commit_subjects(cwd, from, head).unwrap_or_default();
        // The baseline commit itself shouldn't be counted (it introduces plan.md).
        // `from..to` already excludes `from`, so we're fine.
        subjects
            .into_iter()
            .filter(|(_, subj)| is_impl_subject(subj))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let n_tasks = parsed.tasks.len();
    if impl_commits.len() > n_tasks && n_tasks > 0 {
        eprintln!(
            "plan-status: {} impl commits exceed {} plan tasks; tail commits ignored",
            impl_commits.len(),
            n_tasks
        );
    }

    let mut tasks: Vec<Task> = parsed
        .tasks
        .iter()
        .enumerate()
        .map(|(i, title)| {
            let (completed, sha) = match impl_commits.get(i) {
                Some((sha, _)) => (true, Some(sha.clone())),
                None => (false, None),
            };
            Task {
                index: i,
                title: title.clone(),
                completed,
                commit_sha: sha,
            }
        })
        .collect();

    let next_pending = tasks.iter().find(|t| !t.completed).map(|t| t.index);

    let recommendation = if tasks.is_empty() {
        REC_NO_PLAN
    } else if !spec_sync_behind_head {
        REC_ALREADY_SYNCED
    } else {
        REC_IMPLEMENT
    };

    // If no_plan, drop tasks per PRD.
    if recommendation == REC_NO_PLAN {
        tasks.clear();
    }

    Ok(Output {
        run_dir: Some(rel(cwd, &run_dir)),
        plan_path: Some(rel(cwd, &plan_path)),
        baseline_sha,
        head_sha,
        spec_sync_behind_head,
        tasks,
        next_pending_task_index: if recommendation == REC_IMPLEMENT {
            next_pending
        } else {
            None
        },
        recommendation,
    })
}

fn no_plan(run: Option<String>) -> Output {
    Output {
        run_dir: run,
        plan_path: None,
        baseline_sha: None,
        head_sha: None,
        spec_sync_behind_head: false,
        tasks: Vec::new(),
        next_pending_task_index: None,
        recommendation: REC_NO_PLAN,
    }
}

fn rel(base: &Path, p: &Path) -> String {
    p.strip_prefix(base)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

fn latest_run_dir(jkit_dir: &Path) -> Result<Option<PathBuf>> {
    if !jkit_dir.is_dir() {
        return Ok(None);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(jkit_dir)
        .with_context(|| format!("failed to read {}", jkit_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    entries.sort();
    Ok(entries.pop())
}

fn is_impl_subject(s: &str) -> bool {
    // ^(feat|fix|chore)\(impl\):
    let s = s.trim_start();
    for prefix in ["feat(impl):", "fix(impl):", "chore(impl):"] {
        if s.starts_with(prefix) {
            return true;
        }
    }
    false
}

struct ParsedPlan {
    has_tasks_heading: bool,
    tasks: Vec<String>,
}

/// Parse plan.md, returning the ordered task titles under `## Tasks`.
fn parse_plan_tasks(md: &str) -> ParsedPlan {
    let parser = MdParser::new(md);
    let mut events = parser.into_iter().collect::<Vec<_>>().into_iter().peekable();

    let mut has_tasks_heading = false;

    // Walk to "## Tasks".
    while let Some(ev) = events.next() {
        if let Event::Start(Tag::Heading { level, .. }) = ev {
            if level == HeadingLevel::H2 {
                // Collect heading text until end.
                let mut heading_text = String::new();
                while let Some(inner) = events.next() {
                    match inner {
                        Event::End(TagEnd::Heading(_)) => break,
                        Event::Text(t) | Event::Code(t) => heading_text.push_str(&t),
                        _ => {}
                    }
                }
                if heading_text.trim().eq_ignore_ascii_case("Tasks") {
                    has_tasks_heading = true;
                    break;
                }
            }
        }
    }

    if !has_tasks_heading {
        return ParsedPlan { has_tasks_heading: false, tasks: Vec::new() };
    }

    // Find the next list and collect items.
    let mut tasks = Vec::new();
    while let Some(ev) = events.next() {
        match ev {
            Event::Start(Tag::Heading { .. }) => break, // next section, no list
            Event::Start(Tag::List(_)) => {
                // Walk list items.
                let mut depth: i32 = 1;
                while let Some(inner) = events.next() {
                    match inner {
                        Event::Start(Tag::List(_)) => depth += 1,
                        Event::End(TagEnd::List(_)) => {
                            depth -= 1;
                            if depth == 0 {
                                return ParsedPlan { has_tasks_heading: true, tasks };
                            }
                        }
                        Event::Start(Tag::Item) if depth == 1 => {
                            // Collect the item content until matching End(Item),
                            // tracking the *first* bold span if any.
                            let mut full_text = String::new();
                            let mut bold_text: Option<String> = None;
                            let mut in_bold = false;
                            let mut item_depth: i32 = 1;
                            while let Some(item_ev) = events.next() {
                                match item_ev {
                                    Event::Start(Tag::Item) => item_depth += 1,
                                    Event::End(TagEnd::Item) => {
                                        item_depth -= 1;
                                        if item_depth == 0 {
                                            break;
                                        }
                                    }
                                    Event::Start(Tag::Strong) => {
                                        if bold_text.is_none() {
                                            in_bold = true;
                                            bold_text = Some(String::new());
                                        }
                                    }
                                    Event::End(TagEnd::Strong) => {
                                        in_bold = false;
                                    }
                                    Event::Text(t) | Event::Code(t) => {
                                        full_text.push_str(&t);
                                        if in_bold {
                                            if let Some(b) = bold_text.as_mut() {
                                                b.push_str(&t);
                                            }
                                        }
                                    }
                                    Event::SoftBreak | Event::HardBreak => {
                                        full_text.push(' ');
                                        if in_bold {
                                            if let Some(b) = bold_text.as_mut() {
                                                b.push(' ');
                                            }
                                        }
                                    }
                                    Event::Start(Tag::List(_)) => {
                                        // Skip nested list entirely.
                                        let mut nested_depth = 1;
                                        while let Some(nev) = events.next() {
                                            match nev {
                                                Event::Start(Tag::List(_)) => nested_depth += 1,
                                                Event::End(TagEnd::List(_)) => {
                                                    nested_depth -= 1;
                                                    if nested_depth == 0 {
                                                        break;
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            tasks.push(extract_title(&full_text, bold_text.as_deref()));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    ParsedPlan { has_tasks_heading: true, tasks }
}

fn extract_title(full: &str, bold: Option<&str>) -> String {
    if let Some(b) = bold {
        let t = b.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let trimmed = full.trim();
    for sep in [" — ", " -- ", ":"] {
        if let Some((head, _)) = trimmed.split_once(sep) {
            let h = head.trim();
            if !h.is_empty() {
                return h.to_string();
            }
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty() {
        let p = parse_plan_tasks("# title\n");
        assert!(!p.has_tasks_heading);
    }

    #[test]
    fn parse_bold_titles() {
        let md = "## Tasks\n\n1. **Add filter** — wire it\n2. **Persist rows** — repo + JPA\n";
        let p = parse_plan_tasks(md);
        assert!(p.has_tasks_heading);
        assert_eq!(p.tasks, vec!["Add filter", "Persist rows"]);
    }

    #[test]
    fn parse_no_bold_uses_separator() {
        let md = "## Tasks\n\n1. add filter — wire it\n2. persist rows: repo\n";
        let p = parse_plan_tasks(md);
        assert_eq!(p.tasks, vec!["add filter", "persist rows"]);
    }

    #[test]
    fn parse_full_text_when_no_separator() {
        let md = "## Tasks\n\n1. just a sentence\n";
        let p = parse_plan_tasks(md);
        assert_eq!(p.tasks, vec!["just a sentence"]);
    }

    #[test]
    fn parse_empty_tasks_section() {
        let md = "## Tasks\n\n## Other\n";
        let p = parse_plan_tasks(md);
        assert!(p.has_tasks_heading);
        assert!(p.tasks.is_empty());
    }

    #[test]
    fn impl_subject_match() {
        assert!(is_impl_subject("feat(impl): add foo"));
        assert!(is_impl_subject("fix(impl): bar"));
        assert!(is_impl_subject("chore(impl): ci tweak"));
        assert!(!is_impl_subject("feat: add foo"));
        assert!(!is_impl_subject("docs(impl): readme"));
    }
}
