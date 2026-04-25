# kit

Universal CLI used by every `*kit` language plugin (`jkit`, future `gkit`, …). All four subcommands are language-agnostic — they operate on shared filesystem and git inputs and emit JSON for the calling skill to consume.

Specs live in [`docs/`](docs/) — one PRD per subcommand. The implementation tracks those PRDs.

## Subcommands

| Command | What it does |
|---|---|
| `kit plan-status [--run <dir>]` | Report current `.jkit/<run>/` plan state as JSON: tasks, completion (ordinal match against `feat\|fix\|chore(impl):` commits), `next_pending_task_index`, and a `recommendation` of `no_plan` / `already_synced` / `implement_from_plan`. |
| `kit plugin-status <name>` | Report the install state of a Claude Code plugin. Searches `<repo>/.claude/plugins/` then `~/.claude/plugins/`, tries both `<name>` and `<name>-contract`. Returns version, skill name, contract.yaml path, and parsed Maven SDK coordinates from `## SDK`. |
| `kit scenarios sync <domain>` | Derive required test scenarios from `docs/domains/<domain>/api-spec.yaml` (OpenAPI v3) and **append** missing entries to `docs/domains/<domain>/test-scenarios.yaml`. Append-only — never modifies or reorders existing entries. |
| `kit scenarios skip --run <dir> <domain> <id>` | Idempotently record a per-run scenario skip into `<run>/skipped-scenarios.json` so resume doesn't re-prompt. |
| `kit contract publish --service <name> [--confirmed] [--no-commit]` | Push a pre-staged contract bundle (from `.jkit/contract-stage/<service>/`) to its GitHub repo, update the Claude Code marketplace, run `claude plugin marketplace update`, and create scoped `chore(contract):` commits. **Dry-run by default**; `--confirmed` is required for any network or git mutation. |

Every subcommand emits JSON on stdout. Stderr carries advisory warnings (orphaned scenarios, duplicate plugin installs, impl-commit count > plan tasks, etc.).

## Build

```sh
cargo build --release
# binary at target/release/kit
```

## Test

```sh
cargo test
```

Unit tests cover plan parsing, SDK-block parsing, and OpenAPI-driven scenario derivation. Integration tests in `tests/` exercise the CLI against tempdir + real `git` fixtures (no network).

## Layout

```
src/
  main.rs           clap routing
  plan_status.rs
  plugin_status.rs
  scenarios.rs
  contract.rs
  git.rs            shared git shell-out helpers
docs/
  kit-plan-status-prd.md
  kit-plugin-status-prd.md
  kit-scenarios-prd.md
  kit-contract-publish-prd.md
tests/              integration tests
```

## Design notes

- **Shell out to `git`**, not `git2` — keeps the binary lean and matches the operational model (the host already has `git` configured for the user).
- **Side-effect surface is gated**: `plan-status`, `plugin-status`, `scenarios sync` (when no new entries), and `contract publish` (without `--confirmed`) are all read-only.
- **Stable JSON shapes**: callers (skills) parse stdout. Field names and recommendation values are part of the contract — see the PRDs for the authoritative shape.
