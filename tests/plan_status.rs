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
fn snapshot_taken_at_first_impl_commit_no_drift() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/2026-04-25-foo/plan.md",
        "## Tasks\n\n1. **A** — x\n2. **B** — y\n",
    );
    commit_all(tmp.path(), "chore: scaffold plan");
    write(tmp.path(), "a.txt", "a");
    commit_all(tmp.path(), "feat(impl): A");

    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["plan_edited_mid_flight"], false);
    assert!(v["plan_snapshot_sha256"].as_str().is_some());
    assert_eq!(v["recommendation"], "implement_from_plan");

    // The snapshot file should exist on disk.
    assert!(tmp.path().join(".jkit/2026-04-25-foo/.plan-snapshot.json").is_file());
}

#[test]
fn snapshot_detects_plan_edited_mid_flight() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/2026-04-25-foo/plan.md",
        "## Tasks\n\n1. **A** — x\n2. **B** — y\n3. **C** — z\n",
    );
    commit_all(tmp.path(), "chore: scaffold plan");
    write(tmp.path(), "a.txt", "a");
    commit_all(tmp.path(), "feat(impl): A");

    // First call snapshots plan.md.
    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success());

    // Edit plan.md (delete task B, renumber).
    write(
        tmp.path(),
        ".jkit/2026-04-25-foo/plan.md",
        "## Tasks\n\n1. **A** — x\n2. **C** — z\n",
    );

    // Second call should detect drift.
    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["plan_edited_mid_flight"], true);
    assert_eq!(v["recommendation"], "plan_edited_mid_flight");
    // next_pending_task_index is suppressed when we can't trust matching.
    assert!(v["next_pending_task_index"].is_null());
}

#[test]
fn snapshot_not_taken_before_first_impl_commit() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/2026-04-25-foo/plan.md",
        "## Tasks\n\n1. **A** — x\n",
    );
    commit_all(tmp.path(), "chore: scaffold plan");

    let out = kit().current_dir(tmp.path()).args(["plan-status"]).output().unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["plan_edited_mid_flight"], false);
    assert!(v["plan_snapshot_sha256"].is_null());
    // Plan can be edited freely pre-impl — no drift recorded yet.
    assert!(!tmp.path().join(".jkit/2026-04-25-foo/.plan-snapshot.json").exists());
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
