mod common;

use common::*;
use serde_json::Value;
use tempfile::TempDir;

const SPEC: &str = r#"openapi: 3.0.3
info: {title: billing, version: '1'}
paths:
  /invoices/bulk:
    post:
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [customerId, items]
              properties:
                customerId: {type: string}
                items: {type: array, items: {type: string}}
      responses:
        '201': {description: created}
        '401': {description: missing token}
        '409': {description: Duplicate idempotency key}
"#;

#[test]
fn sync_creates_test_scenarios_yaml_when_missing() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "docs/domains/billing/api-spec.yaml", SPEC);

    let out = kit()
        .current_dir(tmp.path())
        .args(["scenarios", "sync", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    // Output now lives at the top-level docs/test-scenarios.yaml under
    // domains.billing (flat list, single-API-type domain).
    let yaml = std::fs::read_to_string(tmp.path().join("docs/test-scenarios.yaml")).unwrap();
    assert!(yaml.contains("happy-path"));
    assert!(yaml.contains("validation-customer-id-missing"));
    assert!(yaml.contains("validation-items-missing"));
    assert!(yaml.contains("auth-missing-token"));
    assert!(yaml.contains("business-duplicate-idempotency-key"));
    assert!(yaml.contains("billing"));

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("added"));
}

#[test]
fn sync_is_append_only_and_idempotent() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "docs/domains/billing/api-spec.yaml", SPEC);

    // First run: creates the file.
    let out = kit().current_dir(tmp.path()).args(["scenarios", "sync", "billing"]).output().unwrap();
    assert!(out.status.success());
    let first = std::fs::read_to_string(tmp.path().join("docs/test-scenarios.yaml")).unwrap();

    // Second run: should not modify the file (no new entries).
    let out = kit().current_dir(tmp.path()).args(["scenarios", "sync", "billing"]).output().unwrap();
    assert!(out.status.success());
    let second = std::fs::read_to_string(tmp.path().join("docs/test-scenarios.yaml")).unwrap();
    assert_eq!(first, second, "file should be untouched on second run");
}

#[test]
fn sync_preserves_human_added_entries() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "docs/domains/billing/api-spec.yaml", SPEC);
    // Pre-seed the new top-level file with a hand-curated entry.
    let pre = "domains:\n  billing:\n    - endpoint: \"POST /invoices/bulk\"\n      id: weird-edge-case\n      description: a thing the spec doesn't capture\n";
    write(tmp.path(), "docs/test-scenarios.yaml", pre);

    let out = kit().current_dir(tmp.path()).args(["scenarios", "sync", "billing"]).output().unwrap();
    assert!(out.status.success());
    let yaml = std::fs::read_to_string(tmp.path().join("docs/test-scenarios.yaml")).unwrap();
    assert!(yaml.contains("weird-edge-case"));
    assert!(yaml.contains("happy-path"));
}

#[test]
fn sync_missing_spec_exits_one() {
    let tmp = TempDir::new().unwrap();
    let out = kit().current_dir(tmp.path()).args(["scenarios", "sync", "billing"]).output().unwrap();
    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert!(v["error"]["message"].as_str().unwrap().contains("api-spec.yaml"));
}

#[test]
fn skip_records_idempotently() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "docs/domains/billing/api-spec.yaml", SPEC);
    kit().current_dir(tmp.path()).args(["scenarios", "sync", "billing"]).output().unwrap();
    std::fs::create_dir_all(tmp.path().join(".jkit/2026-04-25-foo")).unwrap();

    let out = kit()
        .current_dir(tmp.path())
        .args(["scenarios", "skip", "--run", ".jkit/2026-04-25-foo", "billing", "happy-path"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["already_present"], false);
    assert_eq!(v["endpoint"], "POST /invoices/bulk");

    // Re-run: idempotent.
    let out = kit()
        .current_dir(tmp.path())
        .args(["scenarios", "skip", "--run", ".jkit/2026-04-25-foo", "billing", "happy-path"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["already_present"], true);

    // File contains exactly one entry.
    let raw = std::fs::read_to_string(tmp.path().join(".jkit/2026-04-25-foo/skipped-scenarios.json")).unwrap();
    let arr: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(arr.as_array().unwrap().len(), 1);
}

#[test]
fn skip_unknown_id_exits_one() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "docs/domains/billing/api-spec.yaml", SPEC);
    kit().current_dir(tmp.path()).args(["scenarios", "sync", "billing"]).output().unwrap();
    std::fs::create_dir_all(tmp.path().join(".jkit/2026-04-25-foo")).unwrap();

    let out = kit()
        .current_dir(tmp.path())
        .args(["scenarios", "skip", "--run", ".jkit/2026-04-25-foo", "billing", "no-such-id"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
}
