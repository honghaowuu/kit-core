# kit contract publish — Product Requirements

**Version:** 1.0
**Subcommand of:** `kit` (universal binary, used by every `*kit` language plugin)
**Language:** Rust
**Status:** proposed (split from the previously-proposed `jkit contract` PRD; the `publish` subcommand is language-agnostic — it pushes whatever's in the staged directory, regardless of how it was generated, so it lives in `kit`)

---

## Purpose

Pushes a pre-staged contract bundle to its GitHub repo, updates the Claude Code marketplace, runs `claude plugin marketplace update`, writes the local catalog, and creates two scoped `chore(contract):` commits.

The staged directory at `.jkit/contract-stage/<service>/` is produced by the active language plugin's `<lang>kit contract stage` (see `docs/jkit-contract-prd.md` for the Java side; future `gkit contract stage` will use a different OpenAPI generator under the hood). Every plugin emits the same staged-directory shape (`.claude-plugin/plugin.json`, `skills/<service>/SKILL.md`, `domains/<slug>.md`, `reference/contract.yaml`), so the publish step is identical regardless of source language — hence its placement in `kit`.

`publish` defaults to **dry-run** — the binary describes what it would do. `--confirmed` is required to actually push.

---

## Inputs

| Source | Purpose |
|---|---|
| `.jkit/contract.json` | `{contractRepo, marketplaceRepo, marketplaceName}` |
| `.jkit/contract-stage/<service>/` | Staged contract bundle (produced by `<lang>kit contract stage`) |

---

## CLI

```
kit contract publish --service <name> [--confirmed] [--no-commit]
```

| Argument | Default | Description |
|---|---|---|
| `--service <name>` | required | Service name (matches the staged dir at `.jkit/contract-stage/<service>/`) |
| `--confirmed` | false | **Mandatory for any network or git mutation.** Without it: dry-run reporting only. |
| `--no-commit` | false | Skip the `chore(contract):` commits at the end (caller wants to manage commits) |

---

## Algorithm

1. Read `.jkit/contract.json`. Missing or any field absent → exit 1 with the missing field list (skill gathers them and re-runs).
2. Check `.jkit/contract-stage/<service>/` exists and is non-empty. Missing → exit 1.
3. Without `--confirmed`: emit a JSON description of the planned actions; do not push, do not commit. Exit 0.
4. With `--confirmed`:
   - Push contract repo to `contractRepo` (init the staging dir as a git repo if not already; force-push protected by branch rules at GitHub).
   - Clone `marketplaceRepo` to a tempdir; update `marketplace.json` to include this contract; push; delete the tempdir.
   - Run `claude plugin marketplace update <marketplaceName>`.
   - Write `.jkit/marketplace-catalog.json` with the catalog snapshot.
5. Unless `--no-commit`:
   - If `smart-doc.json` or any build-tool config (e.g. `pom.xml`, `package.json`, `go.mod`) were modified during the staging run, commit them as `chore(contract): add <tool> configuration`. The set of files to look for is provided by the active language plugin's stage step via `.jkit/contract-stage/<service>/.modified-files.json`.
   - Commit `.jkit/contract.json`, `.gitignore`, `.jkit/marketplace-catalog.json` as `chore(contract): publish service contract for <service>`.
6. Emit JSON describing what was pushed and committed.

---

## Output (dry-run)

```json
{
  "service": "billing",
  "confirmed": false,
  "contract_repo": "git@github.com:example/billing-contract.git",
  "marketplace_repo": "git@github.com:example/marketplace.git",
  "marketplace_name": "example-marketplace",
  "would_push_files": [
    ".claude-plugin/plugin.json",
    "skills/billing/SKILL.md",
    "domains/invoice.md",
    "reference/contract.yaml"
  ],
  "would_run": [
    "git push <contract-repo>",
    "marketplace.json update + push",
    "claude plugin marketplace update example-marketplace"
  ],
  "would_commit": [
    "chore(contract): add smart-doc configuration",
    "chore(contract): publish service contract for billing"
  ]
}
```

## Output (confirmed)

```json
{
  "service": "billing",
  "confirmed": true,
  "contract_pushed": true,
  "contract_sha": "abc1234",
  "marketplace_pushed": true,
  "marketplace_sha": "def5678",
  "catalog_written": ".jkit/marketplace-catalog.json",
  "commits": [
    {"sha": "111aaa", "subject": "chore(contract): add smart-doc configuration"},
    {"sha": "222bbb", "subject": "chore(contract): publish service contract for billing"}
  ]
}
```

---

## Edge cases

| Case | Behavior |
|---|---|
| Contract repo not empty on first push | Refuse with `"contract repo must be empty for first push — auto-generated README will collide"` |
| Marketplace repo missing | Exit 1; instruct the human to create it |
| `claude plugin marketplace update` not on PATH | Exit 1; surface install instructions |
| Contract push succeeds but marketplace push fails | Surface partial state in `blocking_errors`; do not run subsequent commits. Re-running with `--confirmed` should be idempotent. |
| Two `chore(contract):` commits already on HEAD (re-run) | Skip both commits; surface `already_committed: true` |
| `--no-commit` | Skip the commit step but still push |

---

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (dry-run or confirmed) |
| 1 | Missing `.jkit/contract.json`; missing stage dir; push failure; commit failure |

---

## Suggested dependencies

```toml
[dependencies]
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
clap       = { version = "4", features = ["derive"] }
git2       = "0.19"     # alternative: shell out to git — pick one consistently across kit subcommands
```

---

## Impact on bash scripts — deprecated

The following scripts under `bin/` become deprecated once `kit contract publish` ships:

| Script | Replaced by |
|---|---|
| `bin/contract-push.sh` | `kit contract publish --confirmed` (push phase) |
| `bin/marketplace-publish.sh` | `kit contract publish --confirmed` (marketplace phase) |
| `bin/marketplace-sync.sh` | `kit contract publish --confirmed` (sync phase) |

Remove them once the binary is implemented and `publish-contract` is migrated, mirroring the `bin/pom-add.sh` removal in commit `2495380`.

---

## Impact on publish-contract skill

- **Step 11 (push, marketplace, commits)** → `kit contract publish --service <name> --confirmed`. Replaces the three bash scripts and the conditional-commit logic. Default dry-run mode supports the skill's hard-gate at Step 10.
