use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::git;

#[derive(Subcommand)]
pub enum ContractCmd {
    /// Push a pre-staged contract bundle and update the marketplace.
    Publish {
        /// Service name; matches the staged dir at .jkit/contract-stage/<service>/
        #[arg(long)]
        service: String,
        /// Required for any network or git mutation. Without it: dry-run only.
        #[arg(long, default_value_t = false)]
        confirmed: bool,
        /// Skip the chore(contract) commits at the end.
        #[arg(long, default_value_t = false)]
        no_commit: bool,
    },
}

pub fn run(cmd: ContractCmd) -> Result<ExitCode> {
    match cmd {
        ContractCmd::Publish {
            service,
            confirmed,
            no_commit,
        } => publish(&service, confirmed, no_commit),
    }
}

#[derive(Deserialize)]
struct ContractCfg {
    #[serde(rename = "contractRepo")]
    contract_repo: Option<String>,
    #[serde(rename = "marketplaceRepo")]
    marketplace_repo: Option<String>,
    #[serde(rename = "marketplaceName")]
    marketplace_name: Option<String>,
}

#[derive(Serialize)]
struct DryRunOut<'a> {
    service: &'a str,
    confirmed: bool,
    contract_repo: &'a str,
    marketplace_repo: &'a str,
    marketplace_name: &'a str,
    would_push_files: Vec<String>,
    would_run: Vec<String>,
    would_commit: Vec<String>,
}

#[derive(Serialize)]
struct CommitInfo {
    sha: String,
    subject: String,
}

#[derive(Serialize)]
struct ConfirmedOut<'a> {
    service: &'a str,
    confirmed: bool,
    contract_pushed: bool,
    contract_sha: Option<String>,
    marketplace_pushed: bool,
    marketplace_sha: Option<String>,
    catalog_written: Option<String>,
    commits: Vec<CommitInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blocking_errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    already_committed: Option<bool>,
}

pub fn publish(service: &str, confirmed: bool, no_commit: bool) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;

    // Step 1: read .jkit/contract.json
    let cfg_path = cwd.join(".jkit").join("contract.json");
    if !cfg_path.is_file() {
        return Err(anyhow!(".jkit/contract.json missing"));
    }
    let cfg_text = std::fs::read_to_string(&cfg_path)
        .with_context(|| format!("failed to read {}", cfg_path.display()))?;
    let cfg: ContractCfg = serde_json::from_str(&cfg_text)
        .with_context(|| format!("invalid JSON in {}", cfg_path.display()))?;

    let mut missing: Vec<&str> = Vec::new();
    if cfg.contract_repo.as_deref().unwrap_or("").is_empty() {
        missing.push("contractRepo");
    }
    if cfg.marketplace_repo.as_deref().unwrap_or("").is_empty() {
        missing.push("marketplaceRepo");
    }
    if cfg.marketplace_name.as_deref().unwrap_or("").is_empty() {
        missing.push("marketplaceName");
    }
    if !missing.is_empty() {
        return Err(anyhow!(
            "missing fields in .jkit/contract.json: {}",
            missing.join(", ")
        ));
    }

    let contract_repo = cfg.contract_repo.unwrap();
    let marketplace_repo = cfg.marketplace_repo.unwrap();
    let marketplace_name = cfg.marketplace_name.unwrap();

    // Step 2: stage dir exists and non-empty
    let stage_dir = cwd
        .join(".jkit")
        .join("contract-stage")
        .join(service);
    if !stage_dir.is_dir() {
        return Err(anyhow!(
            "stage dir not found: {}",
            stage_dir.display()
        ));
    }
    let stage_files = collect_stage_files(&stage_dir)?;
    if stage_files.is_empty() {
        return Err(anyhow!("stage dir is empty: {}", stage_dir.display()));
    }

    if !confirmed {
        let modified = read_modified_files(&stage_dir).unwrap_or_default();
        let mut would_commit: Vec<String> = Vec::new();
        if !no_commit {
            if !modified.is_empty() {
                let tool = guess_tool(&modified);
                would_commit.push(format!("chore(contract): add {} configuration", tool));
            }
            would_commit.push(format!(
                "chore(contract): publish service contract for {}",
                service
            ));
        }
        let out = DryRunOut {
            service,
            confirmed: false,
            contract_repo: &contract_repo,
            marketplace_repo: &marketplace_repo,
            marketplace_name: &marketplace_name,
            would_push_files: stage_files
                .iter()
                .map(|p| {
                    p.strip_prefix(&stage_dir)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .to_string()
                })
                .collect(),
            would_run: vec![
                format!("git push {}", contract_repo),
                "marketplace.json update + push".to_string(),
                format!("claude plugin marketplace update {}", marketplace_name),
            ],
            would_commit,
        };
        crate::envelope::print_ok(serde_json::to_value(&out)?);
    }

    // Step 4: confirmed
    let mut blocking: Vec<String> = Vec::new();
    let mut contract_pushed = false;
    let mut contract_sha: Option<String> = None;
    let mut marketplace_pushed = false;
    let mut marketplace_sha: Option<String> = None;

    // 4a. Push contract repo.
    match push_contract_repo(&stage_dir, &contract_repo, service) {
        Ok(sha) => {
            contract_pushed = true;
            contract_sha = Some(sha);
        }
        Err(e) => {
            blocking.push(format!("contract push failed: {}", e));
        }
    }

    let mut catalog_path: Option<PathBuf> = None;

    if contract_pushed {
        // 4b. Update marketplace.
        match update_marketplace(&marketplace_repo, &marketplace_name, service, &contract_repo) {
            Ok((sha, catalog)) => {
                marketplace_pushed = true;
                marketplace_sha = Some(sha);
                // 4c. claude plugin marketplace update
                if let Err(e) = run_claude_marketplace_update(&marketplace_name) {
                    blocking.push(format!("`claude plugin marketplace update` failed: {}", e));
                }
                // 4d. Write catalog. Lock `.jkit/` so two concurrent
                // `kit contract publish` invocations can't lose each other's
                // catalog updates; atomic_write guarantees readers never see
                // a half-written file.
                let jkit_dir = cwd.join(".jkit");
                let cp = jkit_dir.join("marketplace-catalog.json");
                let lock_result = crate::lockfile::lock_file_in(&jkit_dir, "marketplace-catalog");
                let write_result = lock_result.and_then(|_lock| {
                    crate::lockfile::atomic_write(
                        &cp,
                        (serde_json::to_string_pretty(&catalog)? + "\n").as_bytes(),
                    )
                });
                if let Err(e) = write_result {
                    blocking.push(format!("failed to write catalog: {}", e));
                } else {
                    catalog_path = Some(cp);
                }
            }
            Err(e) => {
                blocking.push(format!("marketplace push failed: {}", e));
            }
        }
    }

    // Commits.
    let mut commits: Vec<CommitInfo> = Vec::new();
    let mut already_committed: Option<bool> = None;

    if blocking.is_empty() && !no_commit {
        // Detect if last two commits are already chore(contract) re-run.
        let head_subjects = recent_subjects(&cwd, 2).unwrap_or_default();
        if head_subjects.len() == 2
            && head_subjects[0].starts_with("chore(contract): publish service contract for ")
            && head_subjects[1].starts_with("chore(contract): ")
        {
            already_committed = Some(true);
        } else {
            let modified = read_modified_files(&stage_dir).unwrap_or_default();
            if !modified.is_empty() {
                let tool = guess_tool(&modified);
                let subject = format!("chore(contract): add {} configuration", tool);
                let to_stage: Vec<PathBuf> =
                    modified.iter().map(|p| cwd.join(p)).collect();
                if let Err(e) = stage_and_commit(&cwd, &to_stage, &subject) {
                    blocking.push(format!("commit failed: {}", e));
                } else if let Ok(sha) = git::rev_parse_head(&cwd) {
                    commits.push(CommitInfo { sha, subject });
                }
            }

            if blocking.is_empty() {
                let subject = format!(
                    "chore(contract): publish service contract for {}",
                    service
                );
                let mut to_stage: Vec<PathBuf> = Vec::new();
                to_stage.push(cwd.join(".jkit/contract.json"));
                let gi = cwd.join(".gitignore");
                if gi.exists() {
                    to_stage.push(gi);
                }
                if let Some(cp) = &catalog_path {
                    to_stage.push(cp.clone());
                }
                if let Err(e) = stage_and_commit(&cwd, &to_stage, &subject) {
                    blocking.push(format!("commit failed: {}", e));
                } else if let Ok(sha) = git::rev_parse_head(&cwd) {
                    commits.push(CommitInfo { sha, subject });
                }
            }
        }
    }

    if !blocking.is_empty() {
        let prefix = if contract_pushed {
            "contract publish failed after partial push"
        } else {
            "contract publish failed"
        };
        let msg = format!("{}: {}", prefix, blocking.join("; "));
        crate::envelope::print_err(&msg, None);
    }

    let out = ConfirmedOut {
        service,
        confirmed: true,
        contract_pushed,
        contract_sha,
        marketplace_pushed,
        marketplace_sha,
        catalog_written: catalog_path.map(|p| {
            p.strip_prefix(&cwd)
                .unwrap_or(&p)
                .to_string_lossy()
                .to_string()
        }),
        commits,
        blocking_errors: blocking.clone(),
        already_committed,
    };
    crate::envelope::print_ok(serde_json::to_value(&out)?)
}

fn collect_stage_files(stage: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let p = e.path();
            let name = e.file_name();
            // Skip .git in case it was init'd already, and the metadata file.
            if name == ".git" {
                continue;
            }
            if p.is_dir() {
                walk(&p, out)?;
            } else if name != ".modified-files.json" {
                out.push(p);
            }
        }
        Ok(())
    }
    walk(stage, &mut out)
        .with_context(|| format!("failed to walk {}", stage.display()))?;
    out.sort();
    Ok(out)
}

fn read_modified_files(stage: &Path) -> Option<Vec<String>> {
    let p = stage.join(".modified-files.json");
    let raw = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&raw).ok()
}

fn guess_tool(modified: &[String]) -> &'static str {
    for f in modified {
        if f.ends_with("smart-doc.json") {
            return "smart-doc";
        }
        if f.ends_with("pom.xml") {
            return "maven";
        }
        if f.ends_with("package.json") {
            return "npm";
        }
        if f.ends_with("go.mod") {
            return "go";
        }
        if f.ends_with("Cargo.toml") {
            return "cargo";
        }
    }
    "build-tool"
}

fn push_contract_repo(stage: &Path, repo: &str, service: &str) -> Result<String> {
    // Init git in the stage dir if needed.
    let git_dir = stage.join(".git");
    if !git_dir.exists() {
        run_or_err(stage, &["init", "-q", "-b", "main"])?;
    }
    // Ensure all stage files are tracked.
    run_or_err(stage, &["add", "-A"])?;

    // If contract repo is non-empty (has commits at remote), refuse first push.
    // Probe with ls-remote.
    let probe = Command::new("git")
        .current_dir(stage)
        .args(["ls-remote", "--exit-code", "--heads", repo])
        .stdin(Stdio::null())
        .output()
        .context("git ls-remote failed")?;
    if probe.status.success() {
        // The remote already has at least one branch; treat as not-empty.
        return Err(anyhow!(
            "contract repo must be empty for first push — auto-generated README will collide"
        ));
    }

    // Commit if there's anything to commit.
    let status = run_capture(stage, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        run_or_err(
            stage,
            &[
                "-c",
                "user.email=kit@local",
                "-c",
                "user.name=kit",
                "commit",
                "-q",
                "-m",
                &format!("contract({}): initial publish", service),
            ],
        )?;
    }

    // Configure origin.
    let _ = run_capture(stage, &["remote", "remove", "origin"]);
    run_or_err(stage, &["remote", "add", "origin", repo])?;
    run_or_err(stage, &["push", "-u", "origin", "HEAD:main"])?;

    let sha = run_capture(stage, &["rev-parse", "HEAD"])?.trim().to_string();
    Ok(sha)
}

fn update_marketplace(
    repo: &str,
    name: &str,
    service: &str,
    contract_repo: &str,
) -> Result<(String, serde_json::Value)> {
    let tmp = tempdir_in_temp("kit-marketplace")?;
    let clone = run_capture_no_dir(&["clone", "--depth", "1", repo, tmp.to_string_lossy().as_ref()]);
    if clone.is_err() {
        return Err(anyhow!("marketplace repo missing or unreachable: {}", repo));
    }

    let mp_path = tmp.join("marketplace.json");
    let mut value: serde_json::Value = if mp_path.is_file() {
        let raw = std::fs::read_to_string(&mp_path)?;
        if raw.trim().is_empty() {
            serde_json::json!({"name": name, "plugins": []})
        } else {
            serde_json::from_str(&raw)?
        }
    } else {
        serde_json::json!({"name": name, "plugins": []})
    };

    let plugins = value
        .get_mut("plugins")
        .and_then(|x| x.as_array_mut())
        .ok_or_else(|| anyhow!("marketplace.json: 'plugins' must be an array"))?;

    let plugin_name = format!("{}-contract", service);
    let already = plugins.iter().any(|p| {
        p.get("name").and_then(|x| x.as_str()) == Some(&plugin_name)
    });
    if !already {
        plugins.push(serde_json::json!({
            "name": plugin_name,
            "source": contract_repo,
        }));
    }

    std::fs::write(&mp_path, serde_json::to_string_pretty(&value)? + "\n")?;
    run_or_err(&tmp, &["add", "marketplace.json"])?;
    run_or_err(
        &tmp,
        &[
            "-c",
            "user.email=kit@local",
            "-c",
            "user.name=kit",
            "commit",
            "-q",
            "-m",
            &format!("chore(marketplace): publish {}", plugin_name),
        ],
    )?;
    run_or_err(&tmp, &["push", "origin", "HEAD"])?;
    let sha = run_capture(&tmp, &["rev-parse", "HEAD"])?.trim().to_string();

    let _ = std::fs::remove_dir_all(&tmp);
    Ok((sha, value))
}

fn run_claude_marketplace_update(name: &str) -> Result<()> {
    // Verify `claude` is on PATH.
    let probe = Command::new("sh")
        .args(["-lc", "command -v claude >/dev/null 2>&1"])
        .status()
        .context("failed to probe for claude")?;
    if !probe.success() {
        return Err(anyhow!(
            "`claude` CLI not on PATH — install with `npm install -g @anthropic-ai/claude-code`"
        ));
    }
    let out = Command::new("claude")
        .args(["plugin", "marketplace", "update", name])
        .output()
        .context("failed to invoke `claude plugin marketplace update`")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!(
            "`claude plugin marketplace update {}` exited {}: {}",
            name,
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "killed".into()),
            if stderr.is_empty() { "(no stderr)" } else { &stderr }
        ));
    }
    Ok(())
}

fn recent_subjects(cwd: &Path, n: usize) -> Result<Vec<String>> {
    let out = git::git_stdout(
        cwd,
        ["log", &format!("-{n}"), "--format=%s"],
    )?;
    Ok(out.lines().map(|l| l.to_string()).collect())
}

fn stage_and_commit(cwd: &Path, paths: &[PathBuf], subject: &str) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<String> = vec!["add".into(), "--".into()];
    for p in paths {
        args.push(p.to_string_lossy().to_string());
    }
    let out = Command::new("git")
        .current_dir(cwd)
        .args(&args)
        .output()
        .context("git add failed")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    // If nothing was staged (paths are unchanged), skip the commit.
    let staged = git::git_stdout(cwd, ["diff", "--cached", "--name-only"])?;
    if staged.trim().is_empty() {
        return Ok(());
    }

    let out = Command::new("git")
        .current_dir(cwd)
        .args(["commit", "-q", "-m", subject])
        .output()
        .context("git commit failed")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git commit failed for '{}': {}",
            subject,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn run_or_err(cwd: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("git invocation failed")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {:?} failed in {}: {}",
            args,
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn run_capture(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("git invocation failed")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn run_capture_no_dir(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("git invocation failed")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn tempdir_in_temp(prefix: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = base.join(format!("{}-{}-{}", prefix, pid, nanos));
    std::fs::create_dir_all(&p).with_context(|| format!("failed to create {}", p.display()))?;
    Ok(p)
}
