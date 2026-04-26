mod common;

use common::*;
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn missing_contract_json_exits_one() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    let out = kit()
        .current_dir(tmp.path())
        .args(["contract", "publish", "--service", "billing"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn contract_json_missing_fields_lists_them() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(tmp.path(), ".jkit/contract.json", r#"{"contractRepo": "git@x"}"#);
    write(tmp.path(), ".jkit/contract-stage/billing/skills/billing/SKILL.md", "x");

    let out = kit()
        .current_dir(tmp.path())
        .args(["contract", "publish", "--service", "billing"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.contains("marketplaceRepo"), "msg: {msg}");
    assert!(msg.contains("marketplaceName"), "msg: {msg}");
}

#[test]
fn missing_stage_dir_exits_one() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/contract.json",
        r#"{"contractRepo":"git@a","marketplaceRepo":"git@b","marketplaceName":"m"}"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .args(["contract", "publish", "--service", "billing"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn dry_run_emits_planned_actions_without_pushing() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/contract.json",
        r#"{"contractRepo":"git@example.com:billing-contract.git","marketplaceRepo":"git@example.com:marketplace.git","marketplaceName":"example-marketplace"}"#,
    );
    write(
        tmp.path(),
        ".jkit/contract-stage/billing/.claude-plugin/plugin.json",
        r#"{"name":"billing-contract","version":"1.0.0","skills":["billing"]}"#,
    );
    write(
        tmp.path(),
        ".jkit/contract-stage/billing/skills/billing/SKILL.md",
        "# billing\n",
    );
    write(
        tmp.path(),
        ".jkit/contract-stage/billing/reference/contract.yaml",
        "openapi: 3.0.3\n",
    );
    write(
        tmp.path(),
        ".jkit/contract-stage/billing/.modified-files.json",
        r#"["smart-doc.json"]"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .args(["contract", "publish", "--service", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["confirmed"], false);
    assert_eq!(v["service"], "billing");
    let pushed = v["would_push_files"].as_array().unwrap();
    assert!(pushed
        .iter()
        .any(|p| p.as_str().unwrap().contains("plugin.json")));
    let commits = v["would_commit"].as_array().unwrap();
    assert!(commits.len() == 2);
    assert!(commits[0].as_str().unwrap().contains("smart-doc"));
    assert!(commits[1].as_str().unwrap().contains("publish service contract for billing"));
}

#[test]
fn dry_run_no_commit_omits_commit_plan() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());
    write(
        tmp.path(),
        ".jkit/contract.json",
        r#"{"contractRepo":"a","marketplaceRepo":"b","marketplaceName":"m"}"#,
    );
    write(
        tmp.path(),
        ".jkit/contract-stage/billing/skills/billing/SKILL.md",
        "x",
    );
    let out = kit()
        .current_dir(tmp.path())
        .args(["contract", "publish", "--service", "billing", "--no-commit"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert!(v["would_commit"].as_array().unwrap().is_empty());
}
