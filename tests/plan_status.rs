mod common;

use common::*;
use serde_json::Value;
use tempfile::TempDir;

fn parse_stdout(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("valid JSON")
}

#[test]
fn no_jkit_dir_returns_no_plan() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(tmp.path(), "README.md", "x");
    commit_all(tmp.path(), "init");

    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["recommendation"], "no_plan");
    assert!(v["tasks"].as_array().unwrap().is_empty());
}

#[test]
fn plan_with_no_tasks_heading_returns_no_plan() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(tmp.path(), ".jkit/2026-04-25-foo/plan.md", "# header\n\nno tasks heading\n");
    commit_all(tmp.path(), "feat: add plan");

    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["recommendation"], "no_plan");
}

#[test]
fn plan_with_tasks_no_impl_commits_marks_all_pending() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/2026-04-25-foo/plan.md",
        "## Tasks\n\n1. **First task** — do something\n2. **Second task** — and another\n",
    );
    commit_all(tmp.path(), "feat: add plan");

    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["recommendation"], "implement_from_plan");
    assert_eq!(v["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(v["tasks"][0]["title"], "First task");
    assert_eq!(v["tasks"][0]["completed"], false);
    assert_eq!(v["tasks"][1]["completed"], false);
    assert_eq!(v["next_pending_task_index"], 0);
}

#[test]
fn impl_commits_advance_completed_state() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/2026-04-25-foo/plan.md",
        "## Tasks\n\n1. **Add filter** — wire it\n2. **Persist rows** — repo + JPA\n3. **Expose metrics** — micrometer\n",
    );
    commit_all(tmp.path(), "chore: scaffold plan");
    write(tmp.path(), "a.txt", "a");
    commit_all(tmp.path(), "feat(impl): wire ValidationFilter");
    write(tmp.path(), "b.txt", "b");
    commit_all(tmp.path(), "feat(impl): repo + JPA mapping");

    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["recommendation"], "implement_from_plan");
    assert_eq!(v["tasks"][0]["completed"], true);
    assert_eq!(v["tasks"][1]["completed"], true);
    assert_eq!(v["tasks"][2]["completed"], false);
    assert_eq!(v["next_pending_task_index"], 2);
}

#[test]
fn already_synced_when_all_tasks_have_impl_commits() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/2026-04-25-foo/plan.md",
        "## Tasks\n\n1. **Add filter** — wire it\n2. **Persist rows** — repo + JPA\n",
    );
    commit_all(tmp.path(), "chore: scaffold plan");
    write(tmp.path(), "a.txt", "a");
    commit_all(tmp.path(), "feat(impl): wire ValidationFilter");
    write(tmp.path(), "b.txt", "b");
    commit_all(tmp.path(), "feat(impl): repo + JPA mapping");

    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["recommendation"], "already_synced");
    assert_eq!(v["tasks"][0]["completed"], true);
    assert_eq!(v["tasks"][1]["completed"], true);
    assert!(v["next_pending_task_index"].is_null());
    assert!(v.get("spec_sync_behind_head").is_none());
}

#[test]
fn run_arg_invalid_exits_non_zero() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());

    let out = kit()
        .current_dir(tmp.path())
        .args(["plan-status", "--run", ".jkit/nope"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], false);
    assert!(v["error"]["message"].as_str().unwrap().contains("run dir not found"));
}
