mod common;

use common::*;
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

fn parse_stdout(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        panic!(
            "expected JSON envelope, got: {:?} ({})",
            String::from_utf8_lossy(bytes),
            e
        )
    })
}

/// Build a self-contained "marketplace" git repo at `path` with the given
/// list of (name, description) plugin entries. Returns the path as a
/// file:// URL safe to pass to `git clone`.
fn make_marketplace_repo(path: &Path, plugins: &[(&str, &str)]) -> String {
    let with_versions: Vec<(&str, &str, Option<&str>)> =
        plugins.iter().map(|(n, d)| (*n, *d, None)).collect();
    make_marketplace_repo_versioned(path, &with_versions)
}

/// Like make_marketplace_repo but lets each plugin entry carry a version
/// (the field added by F.1). Used to exercise F.2's latest_version
/// propagation into the local catalog.
fn make_marketplace_repo_versioned(
    path: &Path,
    plugins: &[(&str, &str, Option<&str>)],
) -> String {
    sh(path, &["init", "-q", "-b", "main"]);
    sh(path, &["config", "user.email", "test@local"]);
    sh(path, &["config", "user.name", "test"]);
    sh(path, &["config", "commit.gpgsign", "false"]);
    let plugin_arr: Vec<_> = plugins
        .iter()
        .map(|(n, d, v)| {
            let mut o = serde_json::json!({"name": n, "description": d});
            if let Some(ver) = v {
                o["version"] = serde_json::Value::String(ver.to_string());
            }
            o
        })
        .collect();
    let mp = serde_json::json!({
        "name": "test-marketplace",
        "plugins": plugin_arr,
    });
    write(
        path,
        ".claude-plugin/marketplace.json",
        &serde_json::to_string_pretty(&mp).unwrap(),
    );
    commit_all(path, "init marketplace");
    format!("file://{}", path.display())
}

#[test]
fn refresh_catalog_writes_marketplace_catalog_json() {
    let mp_dir = TempDir::new().unwrap();
    let url = make_marketplace_repo(
        mp_dir.path(),
        &[("billing-contract", "Billing service contract")],
    );

    let work = TempDir::new().unwrap();
    git_init(work.path());

    let out = kit()
        .current_dir(work.path())
        .args([
            "contracts",
            "refresh-catalog",
            "--marketplace-repo",
            &url,
            "--marketplace-name",
            "test-mp",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["marketplace_name"], "test-mp");
    assert_eq!(v["contracts"], serde_json::json!(["billing-contract"]));
    assert!(work.path().join(".jkit/marketplace-catalog.json").is_file());
    // contract.json gets persisted with the values.
    let cj = work.path().join(".jkit/contract.json");
    assert!(cj.is_file());
    let cj_v: Value = serde_json::from_str(&std::fs::read_to_string(&cj).unwrap()).unwrap();
    assert_eq!(cj_v["marketplaceRepo"], url);
    assert_eq!(cj_v["marketplaceName"], "test-mp");
}

#[test]
fn refresh_catalog_without_args_falls_back_to_contract_json() {
    let mp_dir = TempDir::new().unwrap();
    let url = make_marketplace_repo(mp_dir.path(), &[("foo", "Foo svc")]);
    let work = TempDir::new().unwrap();
    git_init(work.path());
    write(
        work.path(),
        ".jkit/contract.json",
        &format!(
            r#"{{"marketplaceRepo":"{}","marketplaceName":"prefilled"}}"#,
            url
        ),
    );

    let out = kit()
        .current_dir(work.path())
        .args(["contracts", "refresh-catalog"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["marketplace_name"], "prefilled");
}

#[test]
fn refresh_catalog_missing_repo_returns_error_envelope() {
    let work = TempDir::new().unwrap();
    git_init(work.path());

    let out = kit()
        .current_dir(work.path())
        .args(["contracts", "refresh-catalog"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], false);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.contains("marketplaceRepo"), "msg: {msg}");
}

#[test]
fn refresh_catalog_unreachable_repo_returns_error_envelope() {
    let work = TempDir::new().unwrap();
    git_init(work.path());

    let out = kit()
        .current_dir(work.path())
        .args([
            "contracts",
            "refresh-catalog",
            "--marketplace-repo",
            "/nonexistent/path/to/nowhere",
            "--marketplace-name",
            "nope",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], false);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.contains("git clone") || msg.contains("clone"), "msg: {msg}");
}

#[test]
fn install_skips_services_not_in_catalog() {
    let mp_dir = TempDir::new().unwrap();
    let url = make_marketplace_repo(
        mp_dir.path(),
        &[("present-contract", "OK")],
    );
    let work = TempDir::new().unwrap();
    git_init(work.path());

    let out = kit()
        .current_dir(work.path())
        .env("KIT_SKIP_CLAUDE", "1")
        .args([
            "contracts",
            "install",
            "absent-contract",
            "present-contract",
            "--marketplace-repo",
            &url,
            "--marketplace-name",
            "test-mp",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["installed"], serde_json::json!(["present-contract"]));
    assert_eq!(v["skipped_not_in_catalog"], serde_json::json!(["absent-contract"]));
    assert_eq!(v["claude_install_failed"], serde_json::json!([]));
    assert_eq!(v["committed"], true);
    assert!(v["commit_subject"]
        .as_str()
        .unwrap()
        .contains("install contracts"));
}

#[test]
fn install_with_no_services_commits_refresh_subject() {
    let mp_dir = TempDir::new().unwrap();
    let url = make_marketplace_repo(mp_dir.path(), &[("foo", "")]);
    let work = TempDir::new().unwrap();
    git_init(work.path());

    let out = kit()
        .current_dir(work.path())
        .env("KIT_SKIP_CLAUDE", "1")
        .args([
            "contracts",
            "install",
            "--marketplace-repo",
            &url,
            "--marketplace-name",
            "test-mp",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["installed"], serde_json::json!([]));
    assert_eq!(v["committed"], true);
    assert_eq!(
        v["commit_subject"].as_str().unwrap(),
        "chore: refresh marketplace catalog"
    );
}

#[test]
fn refresh_catalog_propagates_latest_version_from_marketplace() {
    let mp_dir = TempDir::new().unwrap();
    let url = make_marketplace_repo_versioned(
        mp_dir.path(),
        &[
            ("billing-contract", "Billing", Some("1.5.0")),
            ("legacy-contract", "Pre-versioning", None),
        ],
    );
    let work = TempDir::new().unwrap();
    git_init(work.path());

    let out = kit()
        .current_dir(work.path())
        .args([
            "contracts",
            "refresh-catalog",
            "--marketplace-repo",
            &url,
            "--marketplace-name",
            "test-mp",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    // Read the on-disk catalog and verify latest_version was propagated
    // from the marketplace plugin entry, with serde's
    // skip_serializing_if dropping it for the legacy entry.
    let catalog_text =
        std::fs::read_to_string(work.path().join(".jkit/marketplace-catalog.json")).unwrap();
    let cv: Value = serde_json::from_str(&catalog_text).unwrap();
    let contracts = cv["contracts"].as_array().unwrap();
    let billing = contracts
        .iter()
        .find(|c| c["name"] == "billing-contract")
        .unwrap();
    assert_eq!(billing["latest_version"], "1.5.0");
    let legacy = contracts
        .iter()
        .find(|c| c["name"] == "legacy-contract")
        .unwrap();
    assert!(legacy.get("latest_version").is_none(), "legacy entry should omit latest_version");
}

#[test]
fn install_idempotent_no_change_no_commit() {
    let mp_dir = TempDir::new().unwrap();
    let url = make_marketplace_repo(mp_dir.path(), &[("foo", "")]);
    let work = TempDir::new().unwrap();
    git_init(work.path());

    // First run: writes catalog + contract.json + commits.
    let out1 = kit()
        .current_dir(work.path())
        .env("KIT_SKIP_CLAUDE", "1")
        .args([
            "contracts",
            "refresh-catalog",
            "--marketplace-repo",
            &url,
            "--marketplace-name",
            "test-mp",
        ])
        .output()
        .unwrap();
    assert!(out1.status.success());
    sh(work.path(), &["add", "."]);
    sh(work.path(), &["commit", "-q", "-m", "init"]);

    // Second run with no-op: catalog unchanged (modulo updated_at), still
    // succeeds, just returns ok envelope. Doesn't crash on repeat.
    let out2 = kit()
        .current_dir(work.path())
        .env("KIT_SKIP_CLAUDE", "1")
        .args([
            "contracts",
            "refresh-catalog",
            "--marketplace-repo",
            &url,
            "--marketplace-name",
            "test-mp",
        ])
        .output()
        .unwrap();
    assert!(out2.status.success(), "stderr: {}", String::from_utf8_lossy(&out2.stderr));
    let v = parse_stdout(&out2.stdout);
    assert_eq!(v["ok"], true);
}
