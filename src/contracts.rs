//! `kit contracts {refresh-catalog,install}` — port of `bin/install-contracts.sh`.
//!
//! - `refresh-catalog` clones the marketplace repo, builds
//!   `.jkit/marketplace-catalog.json` from `marketplace.json`, no installs.
//! - `install` does the same plus `claude plugin install <name> --scope project`
//!   for each requested service, then commits the touched config files.
//!
//! Both subcommands take `--marketplace-repo` / `--marketplace-name` overrides;
//! when omitted, they fall back to `.jkit/contract.json`. Interactive prompting
//! is intentionally NOT done here — the binary emits a single JSON envelope on
//! stdout. A thin shim (`bin/install-contracts.sh`) collects the values from
//! the human and forwards them as flags.

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use crate::envelope;
use crate::lockfile;

const CONTRACT_JSON: &str = ".jkit/contract.json";
const SETTINGS_JSON: &str = ".claude/settings.json";
const CATALOG_JSON: &str = ".jkit/marketplace-catalog.json";

#[derive(Subcommand)]
pub enum ContractsCmd {
    /// Clone the marketplace repo and write `.jkit/marketplace-catalog.json`.
    /// Does NOT install any plugins. Idempotent.
    RefreshCatalog {
        #[arg(long)]
        marketplace_repo: Option<String>,
        #[arg(long)]
        marketplace_name: Option<String>,
    },
    /// Refresh the catalog, then `claude plugin install <name>` for each
    /// service, then commit settings.json + catalog + contract.json.
    Install {
        /// Service names to install (space-separated). Empty = catalog refresh
        /// only, but the commit subject becomes "refresh marketplace catalog".
        services: Vec<String>,
        #[arg(long)]
        marketplace_repo: Option<String>,
        #[arg(long)]
        marketplace_name: Option<String>,
    },
}

pub fn run(cmd: ContractsCmd) -> Result<ExitCode> {
    match cmd {
        ContractsCmd::RefreshCatalog {
            marketplace_repo,
            marketplace_name,
        } => refresh_catalog(marketplace_repo, marketplace_name),
        ContractsCmd::Install {
            services,
            marketplace_repo,
            marketplace_name,
        } => install(services, marketplace_repo, marketplace_name),
    }
}

#[derive(Serialize, Debug)]
struct CatalogContract {
    name: String,
    description: String,
    /// Version of the contract published to the marketplace, when known.
    /// Propagated from `marketplace.json`'s plugin entry (written by
    /// `kit contract publish` — see F.1). `kit plugin-status` compares
    /// this to the locally installed plugin.json to compute drift.
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_version: Option<String>,
}

#[derive(Serialize, Debug)]
struct Catalog {
    #[serde(rename = "marketplaceName")]
    marketplace_name: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    contracts: Vec<CatalogContract>,
}

#[derive(Serialize, Debug)]
struct RefreshOut {
    marketplace_name: String,
    catalog_path: String,
    contracts: Vec<String>,
    updated_at: String,
}

#[derive(Serialize, Debug)]
struct InstallOut {
    marketplace_name: String,
    catalog_path: String,
    installed: Vec<String>,
    skipped_not_in_catalog: Vec<String>,
    claude_install_failed: Vec<String>,
    committed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_subject: Option<String>,
}

pub fn refresh_catalog(repo_arg: Option<String>, name_arg: Option<String>) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("reading cwd")?;
    let (repo, name) = resolve_marketplace_config(&cwd, repo_arg, name_arg)?;
    let (catalog, _) = build_catalog_from_remote(&repo, &name)?;
    write_catalog(&cwd, &catalog)?;
    let out = RefreshOut {
        marketplace_name: catalog.marketplace_name.clone(),
        catalog_path: CATALOG_JSON.to_string(),
        contracts: catalog.contracts.iter().map(|c| c.name.clone()).collect(),
        updated_at: catalog.updated_at.clone(),
    };
    envelope::print_ok(serde_json::to_value(&out)?)
}

pub fn install(
    services: Vec<String>,
    repo_arg: Option<String>,
    name_arg: Option<String>,
) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("reading cwd")?;
    let (repo, name) = resolve_marketplace_config(&cwd, repo_arg, name_arg)?;

    // Phase 1: build + persist catalog (same as refresh-catalog).
    let (catalog, _) = build_catalog_from_remote(&repo, &name)?;
    write_catalog(&cwd, &catalog)?;

    // Phase 2: claude plugin marketplace add/update — best-effort. Errors
    // surface in claude_install_failed via the install loop's exit codes,
    // not here, because these are necessary even when no services are
    // requested (the user may want a clean catalog refresh).
    if let Err(e) = claude_marketplace_add(&repo) {
        return Err(anyhow!("`claude plugin marketplace add {}` failed: {}", repo, e));
    }
    if let Err(e) = claude_marketplace_update(&name) {
        return Err(anyhow!(
            "`claude plugin marketplace update {}` failed: {}",
            name,
            e
        ));
    }

    // Phase 3: validate + install each requested service.
    let catalog_names: std::collections::HashSet<String> =
        catalog.contracts.iter().map(|c| c.name.clone()).collect();
    let mut installed: Vec<String> = Vec::new();
    let mut skipped_not_in_catalog: Vec<String> = Vec::new();
    let mut claude_install_failed: Vec<String> = Vec::new();
    for s in &services {
        if !catalog_names.contains(s) {
            skipped_not_in_catalog.push(s.clone());
            continue;
        }
        match claude_plugin_install(s) {
            Ok(()) => installed.push(s.clone()),
            Err(_) => claude_install_failed.push(s.clone()),
        }
    }

    // Phase 4: stage + commit. Commit is best-effort; failure isn't fatal
    // (user can re-stage manually) but is reflected in `committed`.
    let commit_subject = if installed.is_empty() {
        "chore: refresh marketplace catalog".to_string()
    } else {
        format!("chore: install contracts [{}]", installed.join(", "))
    };
    let committed = stage_and_commit(&cwd, &commit_subject).unwrap_or(false);

    let out = InstallOut {
        marketplace_name: catalog.marketplace_name,
        catalog_path: CATALOG_JSON.to_string(),
        installed,
        skipped_not_in_catalog,
        claude_install_failed,
        committed,
        commit_subject: if committed { Some(commit_subject) } else { None },
    };
    envelope::print_ok(serde_json::to_value(&out)?)
}

/// Returns `(marketplace_repo, marketplace_name)`, persisting any non-None
/// override back into `.jkit/contract.json`.
fn resolve_marketplace_config(
    cwd: &Path,
    repo_arg: Option<String>,
    name_arg: Option<String>,
) -> Result<(String, String)> {
    let cfg_path = cwd.join(CONTRACT_JSON);
    let mut cfg: Value = if cfg_path.is_file() {
        let text = std::fs::read_to_string(&cfg_path)
            .with_context(|| format!("reading {}", cfg_path.display()))?;
        serde_json::from_str(&text).unwrap_or_else(|_| Value::Object(Default::default()))
    } else {
        Value::Object(Default::default())
    };

    let repo = repo_arg
        .or_else(|| {
            cfg.get("marketplaceRepo")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "marketplaceRepo missing — pass --marketplace-repo or set it in {}",
                CONTRACT_JSON
            )
        })?;

    let name = name_arg
        .or_else(|| {
            cfg.get("marketplaceName")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "marketplaceName missing — pass --marketplace-name or set it in {}",
                CONTRACT_JSON
            )
        })?;

    // Persist (idempotent: only writes if values differ).
    let cfg_obj = cfg.as_object_mut().expect("ensured object above");
    let need_write = cfg_obj
        .get("marketplaceRepo")
        .and_then(|v| v.as_str())
        != Some(&repo)
        || cfg_obj
            .get("marketplaceName")
            .and_then(|v| v.as_str())
            != Some(&name);
    if need_write {
        cfg_obj.insert("marketplaceRepo".into(), Value::String(repo.clone()));
        cfg_obj.insert("marketplaceName".into(), Value::String(name.clone()));
        std::fs::create_dir_all(cwd.join(".jkit"))?;
        let text = serde_json::to_string_pretty(&cfg)? + "\n";
        lockfile::atomic_write(&cfg_path, text.as_bytes())?;
    }

    Ok((repo, name))
}

/// Clone marketplace repo (shallow, into a temp dir), parse `marketplace.json`,
/// build a `Catalog`. Does not write to disk. Tempdir cleaned on drop.
fn build_catalog_from_remote(repo: &str, name: &str) -> Result<(Catalog, tempfile::TempDir)> {
    let tmp = tempfile::Builder::new()
        .prefix("kit-marketplace-")
        .tempdir()
        .context("creating tempdir for marketplace clone")?;

    let clone_status = Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--quiet",
            repo,
            tmp.path().to_string_lossy().as_ref(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("invoking git clone")?;
    if !clone_status.status.success() {
        let stderr = String::from_utf8_lossy(&clone_status.stderr).trim().to_string();
        return Err(anyhow!(
            "git clone {} failed: {}",
            repo,
            if stderr.is_empty() { "(no stderr)" } else { &stderr }
        ));
    }

    // Marketplace lives at .claude-plugin/marketplace.json per the
    // claude-code marketplace convention.
    let mp_path = tmp.path().join(".claude-plugin").join("marketplace.json");
    let mp_text = std::fs::read_to_string(&mp_path)
        .with_context(|| format!("reading {}", mp_path.display()))?;
    let mp: Value = serde_json::from_str(&mp_text)
        .with_context(|| format!("parsing {}", mp_path.display()))?;
    let plugins = mp
        .get("plugins")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("marketplace.json missing `plugins` array"))?;

    let contracts: Vec<CatalogContract> = plugins
        .iter()
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?.to_string();
            let description = p
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let latest_version = p
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Some(CatalogContract {
                name,
                description,
                latest_version,
            })
        })
        .collect();

    let catalog = Catalog {
        marketplace_name: name.to_string(),
        updated_at: now_iso8601(),
        contracts,
    };
    Ok((catalog, tmp))
}

fn write_catalog(cwd: &Path, catalog: &Catalog) -> Result<()> {
    let jkit_dir = cwd.join(".jkit");
    std::fs::create_dir_all(&jkit_dir)?;
    let _lock = lockfile::lock_file_in(&jkit_dir, "marketplace-catalog")?;
    let path = cwd.join(CATALOG_JSON);
    let text = serde_json::to_string_pretty(catalog)? + "\n";
    lockfile::atomic_write(&path, text.as_bytes())?;
    Ok(())
}

fn stage_and_commit(cwd: &Path, subject: &str) -> Result<bool> {
    let candidates = [SETTINGS_JSON, CATALOG_JSON, CONTRACT_JSON];
    let to_stage: Vec<&Path> = candidates
        .iter()
        .filter(|p| cwd.join(p).is_file())
        .map(|p| Path::new(*p))
        .collect();
    if to_stage.is_empty() {
        return Ok(false);
    }

    let mut add = Command::new("git");
    add.current_dir(cwd).arg("add");
    for p in &to_stage {
        add.arg(p);
    }
    let s = add
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("git add")?;
    if !s.status.success() {
        return Err(anyhow!(
            "git add failed: {}",
            String::from_utf8_lossy(&s.stderr).trim()
        ));
    }

    // Check whether anything is actually staged. `git diff --cached --quiet`
    // exits 0 if nothing to commit. All streams suppressed so nothing leaks
    // to our envelope channel.
    let staged = Command::new("git")
        .current_dir(cwd)
        .args(["diff", "--cached", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("git diff --cached")?;
    if staged.success() {
        return Ok(false); // nothing to commit
    }

    let s = Command::new("git")
        .current_dir(cwd)
        .args(["commit", "-q", "-m", subject])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("git commit")?;
    if !s.status.success() {
        return Err(anyhow!(
            "git commit failed: {}",
            String::from_utf8_lossy(&s.stderr).trim()
        ));
    }
    Ok(true)
}

fn claude_marketplace_add(repo: &str) -> Result<()> {
    run_claude(&["plugin", "marketplace", "add", repo])
}

fn claude_marketplace_update(name: &str) -> Result<()> {
    run_claude(&["plugin", "marketplace", "update", name])
}

fn claude_plugin_install(plugin_name: &str) -> Result<()> {
    run_claude(&["plugin", "install", plugin_name, "--scope", "project"])
}

fn run_claude(args: &[&str]) -> Result<()> {
    // Test escape hatch: when KIT_SKIP_CLAUDE=1, treat all claude calls as
    // successful no-ops. Lets the install path be exercised in CI without
    // a real `claude` binary.
    if std::env::var("KIT_SKIP_CLAUDE").as_deref() == Ok("1") {
        return Ok(());
    }
    let out = Command::new("claude")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("invoking `claude` (is it on PATH?)")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`claude {}` exited {}: {}",
            args.join(" "),
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "killed".into()),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn now_iso8601() -> String {
    // Minimal RFC3339-ish UTC timestamp without a chrono dep — kit-core
    // doesn't use chrono and pulling it in just for this is overkill.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    // Y-M-D H:M:S derivation. Anchored at 1970-01-01 UTC.
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        y, mo, d, h, mi, s
    )
}

/// Convert epoch seconds to (year, month, day, hour, minute, second) UTC.
/// Days-from-civil algorithm by Howard Hinnant (public domain).
fn epoch_to_ymdhms(z: i64) -> (i32, u32, u32, u32, u32, u32) {
    let secs_per_day = 86_400i64;
    let days = z.div_euclid(secs_per_day);
    let secs_of_day = z.rem_euclid(secs_per_day) as u32;
    let h = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    // Hinnant's days_from_civil inverse:
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y as i32, mo as u32, d as u32, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_2026_04_26_round_trip() {
        // 2026-04-26T00:00:00Z = `date -u -d '2026-04-26 00:00:00' +%s`
        let (y, mo, d, h, mi, s) = epoch_to_ymdhms(1_777_161_600);
        assert_eq!((y, mo, d, h, mi, s), (2026, 4, 26, 0, 0, 0));
    }

    #[test]
    fn epoch_2024_02_29_leap_day() {
        // 2024-02-29T12:34:56Z = 1709210096
        let (y, mo, d, h, mi, s) = epoch_to_ymdhms(1_709_210_096);
        assert_eq!((y, mo, d, h, mi, s), (2024, 2, 29, 12, 34, 56));
    }

    #[test]
    fn epoch_zero_is_unix_epoch() {
        let (y, mo, d, h, mi, s) = epoch_to_ymdhms(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }
}
