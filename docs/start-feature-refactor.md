# start-feature refactor — kit-core touch-ups

**Status:** companion brief to `/workspaces/jkit-cli/docs/start-feature-refactor.md`. The plugin's spec phase collapsed from `/write-change` + `/spec-delta` into a single `/start-feature` skill on a per-feature `feature/<slug>` git branch; `docs/changes/{pending,done}/` and `chore(complete):` commits are gone. Most binary changes land in `jkit-cli`, but `kit-core` has a few small surface points that need adjusting too.

## What needs to change

### 1. `kit plan-status` — run dir resolution

Today the resolver picks the lexicographically latest dir under `.jkit/` **excluding `done` and `adhoc-*`**. With no archive dir anymore, drop the `done` exclusion:

- Old: skip `.jkit/done/` and `.jkit/adhoc-*/`.
- New: skip `.jkit/adhoc-*/` only.

In practice, on a `feature/<slug>` branch there's typically exactly one matching run dir, so the resolution is unambiguous.

### 2. `kit plan-status` — drop `chore(complete):` handling

Today the git-walk in plan-status collects two regex categories:

```
^(feat|fix|chore)\(impl\):
^chore\(complete\):
```

The second goes away — `chore(complete):` commits are no longer emitted (there's no `jkit changes complete` step; merging the feature branch to main IS the completion).

The map-impl-commits-to-plan-tasks logic also has a special override:

> When a `chore(complete):` commit exists in the window, override: mark every task `completed: true` regardless of impl-commit count, attribute each to the matching impl commit (or the last one if N tasks but only 1 impl commit — the chain-endpoint case).

That override goes away. The chain-endpoint case (single consolidated impl commit covering all tasks) should now be handled directly by the index-match logic: **when N tasks have only 1 matching impl commit, mark all N as `completed: true` and attribute each to that commit**. This was previously gated on `chore(complete):` existing; now it's the default for the chain-endpoint shape.

This preserves the existing `already_synced` recommendation behavior — once all tasks have impl commits (or the single chain-endpoint impl commit covers them), plan-status returns `already_synced` and `/java-verify` finalizes by announcing the branch is ready to merge.

### 3. Other commands — sanity check

- `kit scenarios skip --run <run> <domain> <id>` — still writes `<run>/skipped-scenarios.json`. Run dir lives on the feature branch now; the command itself doesn't care which branch. **No change needed.**
- `kit scenarios sync` — operates on `docs/test-scenarios.yaml`. **No change needed.**
- `kit plugin-status`, `kit contract publish`, `kit contracts install / refresh-catalog / bootstrap-marketplace` — unrelated to the spec-phase refactor. **No change needed.**

But search the codebase for stale references just to be safe:
- `change-summary` / `change-summary.md` — should be zero hits in this repo, but check.
- `docs/changes/` / `pending/` / `done/` paths — should be zero hits.
- `chore(complete)` regex / string — should be zero hits *after* the plan-status edits.

## Validation criteria

```bash
cd /workspaces/kit-core
cargo build --release
cargo test --lib
```

Both pass. Then vendor:

```bash
cp target/release/kit /workspaces/jkit/bin/kit-linux-x86_64
```

Smoke test against a scratch project's feature branch:
- Branch has `<type>(spec)` → `<type>(plan)` → `<type>(impl)` commits, each touching the run dir's plan.md once.
- `kit plan-status` returns `already_synced` (or `implement_from_plan` with the right `next_pending_task_index`) — same shape as before, just without the `chore(complete):` dependency.

## Commit hygiene

Per `/workspaces/jkit/CLAUDE.md`'s vendoring rule, three coordinated commits land together for the full refactor:

1. **`/workspaces/jkit-cli`:** the bigger change — `jkit feature start`/`init`, sql-migration entity scan, removal of `jkit changes *`. See `/workspaces/jkit-cli/docs/start-feature-refactor.md`.
2. **`/workspaces/kit-core`:** this brief — plan-status run-dir + commit-regex tweaks. Suggested subject: `fix: plan-status drops chore(complete) handling for branch-per-feature workflow`.
3. **`/workspaces/jkit`:** vendor both binaries (`bin/jkit-linux-x86_64` + `bin/kit-linux-x86_64`). Suggested subject: `bin: vendor jkit + kit with start-feature refactor`.

Order doesn't matter strictly — kit-core and jkit-cli can be built independently — but vendoring should be the last commit so the plugin binary updates land alongside an internally-consistent CLI surface.

## Reference

- `/workspaces/jkit/docs/LOGIC.md` §3.2 — `kit plan-status` semantics (the section in this repo's `docs/kit-plan-status-prd.md` may also need refreshing if it still describes the old `chore(complete):` override).
