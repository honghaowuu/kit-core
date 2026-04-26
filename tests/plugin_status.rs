mod common;

use common::*;
use serde_json::Value;
use tempfile::TempDir;

fn parse_stdout(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("valid JSON")
}

#[test]
fn missing_plugin_returns_installed_false() {
    let tmp = TempDir::new().unwrap();
    let out = kit()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .args(["plugin-status", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["installed"], false);
    assert_eq!(v["plugin_name"], "billing");
    assert_eq!(v["drift_status"], "unknown");
}

#[test]
fn drift_status_behind_when_catalog_has_newer_version() {
    let tmp = TempDir::new().unwrap();
    let plugin_root = ".claude/plugins/billing-contract";
    write(
        tmp.path(),
        &format!("{}/.claude-plugin/plugin.json", plugin_root),
        r#"{"name": "billing-contract", "version": "1.0.0", "skills": ["billing"]}"#,
    );
    write(
        tmp.path(),
        ".jkit/marketplace-catalog.json",
        r#"{"marketplaceName":"x","updatedAt":"x","contracts":[{"name":"billing-contract","description":"","latest_version":"1.5.0"}]}"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .args(["plugin-status", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["plugin_version"], "1.0.0");
    assert_eq!(v["latest_version"], "1.5.0");
    assert_eq!(v["drift_status"], "behind");
}

#[test]
fn drift_status_unknown_when_catalog_missing() {
    let tmp = TempDir::new().unwrap();
    let plugin_root = ".claude/plugins/billing-contract";
    write(
        tmp.path(),
        &format!("{}/.claude-plugin/plugin.json", plugin_root),
        r#"{"name": "billing-contract", "version": "1.0.0", "skills": ["billing"]}"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .args(["plugin-status", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["drift_status"], "unknown");
    assert!(v.get("latest_version").is_none(), "latest_version should be omitted when unknown");
}

#[test]
fn project_level_plugin_with_full_metadata() {
    let tmp = TempDir::new().unwrap();
    let plugin_root = ".claude/plugins/billing-contract";
    write(
        tmp.path(),
        &format!("{}/.claude-plugin/plugin.json", plugin_root),
        r#"{"name": "billing-contract", "version": "1.0.0", "skills": ["billing"]}"#,
    );
    write(
        tmp.path(),
        &format!("{}/skills/billing/SKILL.md", plugin_root),
        "# billing\n\n## SDK\n\n```xml\n<dependency>\n  <groupId>com.example</groupId>\n  <artifactId>billing-api</artifactId>\n  <version>1.2.0</version>\n</dependency>\n```\n",
    );
    write(
        tmp.path(),
        &format!("{}/reference/contract.yaml", plugin_root),
        "openapi: 3.0.3\n",
    );

    // Either input form should resolve to the same plugin.
    for arg in ["billing", "billing-contract"] {
        let out = kit()
            .current_dir(tmp.path())
            .env("HOME", tmp.path())
            .args(["plugin-status", arg])
            .output()
            .unwrap();
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        let v = parse_stdout(&out.stdout);
        assert_eq!(v["ok"], true);
        assert_eq!(v["installed"], true);
        assert_eq!(v["plugin_version"], "1.0.0");
        assert_eq!(v["skill_name"], "billing");
        assert_eq!(v["sdk"]["present"], true);
        assert_eq!(v["sdk"]["group_id"], "com.example");
        assert_eq!(v["sdk"]["artifact_id"], "billing-api");
        assert_eq!(v["sdk"]["version"], "1.2.0");
        assert!(v["contract_yaml_path"].as_str().unwrap().contains("contract.yaml"));
    }
}

#[test]
fn project_level_wins_over_user_level() {
    let tmp = TempDir::new().unwrap();
    // Project install
    write(
        tmp.path(),
        ".claude/plugins/billing-contract/.claude-plugin/plugin.json",
        r#"{"name":"billing-contract","version":"2.0.0","skills":["billing"]}"#,
    );
    // Fake home with same plugin at older version
    let home = TempDir::new().unwrap();
    write(
        home.path(),
        ".claude/plugins/billing-contract/.claude-plugin/plugin.json",
        r#"{"name":"billing-contract","version":"1.0.0","skills":["billing"]}"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .env("HOME", home.path())
        .args(["plugin-status", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["plugin_version"], "2.0.0");
    let warnings = v["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("duplicate")));
}

#[test]
fn repository_and_bugs_fields_surface_urls() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        ".claude/plugins/billing-contract/.claude-plugin/plugin.json",
        r#"{
          "name":"billing-contract","version":"1.0.0","skills":["billing"],
          "repository":{"type":"git","url":"git+https://github.com/acme/billing-contract.git"},
          "bugs":{"url":"https://acme.example.com/tickets/billing"}
        }"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .args(["plugin-status", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["repo_url"], "https://github.com/acme/billing-contract");
    assert_eq!(v["issues_url"], "https://acme.example.com/tickets/billing");
}

#[test]
fn issues_url_derived_from_github_repo_when_bugs_absent() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        ".claude/plugins/billing-contract/.claude-plugin/plugin.json",
        r#"{
          "name":"billing-contract","version":"1.0.0","skills":["billing"],
          "repository":"https://github.com/acme/billing-contract"
        }"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .args(["plugin-status", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["repo_url"], "https://github.com/acme/billing-contract");
    assert_eq!(v["issues_url"], "https://github.com/acme/billing-contract/issues");
}

#[test]
fn no_issues_url_for_non_github_repo_when_bugs_absent() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        ".claude/plugins/billing-contract/.claude-plugin/plugin.json",
        r#"{
          "name":"billing-contract","version":"1.0.0","skills":["billing"],
          "repository":"https://gitlab.example.com/acme/billing-contract"
        }"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .args(["plugin-status", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["repo_url"], "https://gitlab.example.com/acme/billing-contract");
    assert!(v["issues_url"].is_null());
}

#[test]
fn missing_contract_yaml_surfaces_warning() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        ".claude/plugins/billing-contract/.claude-plugin/plugin.json",
        r#"{"name":"billing-contract","version":"1.0.0","skills":["billing"]}"#,
    );

    let out = kit()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .args(["plugin-status", "billing"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert!(v["contract_yaml_path"].is_null());
    let warnings = v["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("contract.yaml")));
}
