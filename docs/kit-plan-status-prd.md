# kit plan-status — Product Requirements

**Version:** 1.0
**Subcommand of:** `kit` (universal binary, used by every `*kit` language plugin)
**Status:** proposed extension

---

## Purpose

Report the current jkit plan/run state as a single deterministic JSON object, so the `java-tdd` skill stops improvising plan detection and resume logic from raw filesystem inspection + `git log` parsing.

Currently `java-tdd` Step 1 (plan detection) and the Resume rule both ask the model to:

1. List `.jkit/*/`, find the latest dir
2. Read `.jkit/spec-sync` and compare to `git HEAD`
3. Read `<run>/plan.md` and parse the task list
4. Walk `git log --oneline` for `feat(impl)|fix(impl)|chore(impl)` commits since the run baseline
5. Match commit subjects to plan task titles, identify the next pending task

Step 5 in particular is fuzzy and where models drift on resume. A binary call collapses all five steps to one deterministic answer.

**Design principle:** Reads structured inputs (filesystem + git), emits a single structured JSON describing run/plan state. Java-tdd consumes the JSON; never inspects `.jkit/` or `git log` directly.

---

## Inputs

| Source | Used for |
|---|---|
| `.jkit/<run>/` directories | Latest-run lookup |
| `<run>/plan.md` | Task list parsing |
| `.jkit/spec-sync` (single line, sha or empty) | Detect spec drift vs HEAD |
| `git log` from run baseline to HEAD | Completed-task detection |

The "run baseline" is the commit that introduced `<run>/plan.md` (computed via `git log --diff-filter=A --format=%H -- <run>/plan.md | tail -1`). No additional marker file is required.

---

## CLI

```
kit plan-status [--run <dir>]
```

| Argument | Default | Description |
|---|---|---|
| `--run <dir>` | latest under `.jkit/` | Specific run dir; omit to use the lexicographically latest |

---

## Algorithm

1. Resolve target run:
   - With `--run`: use it. Missing dir → exit 1.
   - Without: list `.jkit/*/`, sort lexicographically, pick last. None → emit `{"recommendation": "no_plan"}` and exit 0.
2. Read `<run>/plan.md`. Missing → `recommendation: "no_plan"`. Parse the `## Tasks` section as an ordered list (rules below).
3. Compute `baseline_sha` = commit that introduced `<run>/plan.md`.
4. Read `.jkit/spec-sync` if present. Compare to `git rev-parse HEAD` → `spec_sync_behind_head: bool`.
5. Walk commits from `baseline_sha`..`HEAD`, filtering subjects matching `^(feat|fix|chore)\(impl\):`. The Nth such commit (in topological order) is treated as completing the Nth plan task — ordinal matching, not subject similarity.
6. Emit JSON.

**Why ordinal matching?** Java-tdd commits one impl commit per task, in plan order. Subject-similarity matching is fragile (renames, typos, multi-task commits). Ordinal matching breaks only if the user commits an out-of-band impl commit during the run — that's a workflow violation worth surfacing as a stderr warning, not silently rectifying.

---

## Plan task parsing

`plan.md` format is owned by `superpowers:writing-plans`. `plan-status` parses the `## Tasks` section as an ordered list. Expected shape:

```markdown
## Tasks

1. **Add ValidationFilter** — wire request-body validation into the controller chain
2. **Persist invoice rows** — repository + JPA mapping
3. **Expose metrics** — Micrometer counter on validation rejection
```

Rules:

- `title` = the first bold span in the list item, if any. If no bold span, the full list-item text up to the first ` — `, ` -- `, or `:`.
- Empty `## Tasks` section → `tasks: []`.
- No `## Tasks` heading at all → `recommendation: "no_plan"`, `tasks: []`.

The parser must align with whatever `superpowers:writing-plans` emits. If the writing-plans format changes, the parser must change in lock-step — surface this as a coordination constraint when either is modified.

---

## Output

Single JSON object to stdout:

```json
{
  "run_dir": ".jkit/2026-04-25-foo",
  "plan_path": ".jkit/2026-04-25-foo/plan.md",
  "baseline_sha": "abc1234567890abcdef",
  "head_sha": "def4567890abcdef1234",
  "spec_sync_behind_head": true,
  "tasks": [
    {"index": 0, "title": "Add ValidationFilter", "completed": true,  "commit_sha": "111aaa..."},
    {"index": 1, "title": "Persist invoice rows", "completed": true,  "commit_sha": "222bbb..."},
    {"index": 2, "title": "Expose metrics",       "completed": false, "commit_sha": null}
  ],
  "next_pending_task_index": 2,
  "recommendation": "implement_from_plan"
}
```

### `recommendation` field

| Value | Meaning |
|---|---|
| `"no_plan"` | No run dir or no `plan.md`. java-tdd should fall through to ad-hoc TDD. |
| `"already_synced"` | Plan exists but `.jkit/spec-sync` matches HEAD. Nothing to implement; java-tdd should stop and report. |
| `"implement_from_plan"` | Plan exists, spec-sync behind HEAD. java-tdd routes by `next_pending_task_index` (covers both fresh-start and resume). |

---

## Edge cases

| Case | Behavior |
|---|---|
| `.jkit/` doesn't exist | `{"recommendation": "no_plan"}`, exit 0 |
| Latest run dir has no `plan.md` | `{"recommendation": "no_plan"}`, exit 0 |
| `## Tasks` section absent | `{"tasks": [], "recommendation": "no_plan"}`, exit 0 |
| Plan has N tasks but >N impl commits since baseline | Tasks 0..N-1 marked completed; warn to stderr: `"plan-status: <M> impl commits exceed <N> plan tasks; tail commits ignored"` |
| Plan has N tasks but 0 impl commits | All tasks `completed: false`, `next_pending_task_index: 0` |
| `.jkit/spec-sync` missing | `spec_sync_behind_head: true` if any commits exist after baseline; otherwise `false` |
| Run dir name doesn't match `YYYY-MM-DD-*` | Use lexicographic order anyway — no special parsing |
| `--run` points to a missing dir | Exit 1 with error |
| Parser cannot find a bold span and no ` — `/`:` separator | Use the full list-item text as `title` |

---

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (including `recommendation: "no_plan"`) |
| 1 | `--run` argument invalid; required git command failed; I/O failure |

---

## Suggested dependencies

```toml
# Additions to the existing jkit Cargo.toml
pulldown-cmark = "0.12"   # plan.md parsing
git2          = "0.19"    # alternative to shelling out to git; pick one
```

(If `jkit` already shells out to `git` for other subcommands, stay consistent — don't add `git2` just for this.)

---

## Impact on java-tdd

- **Step 1 (Plan detection)** → one `kit plan-status` call. Skill reads `recommendation` and (if `implement_from_plan`) `plan_path` + `next_pending_task_index`. Removes ~6 lines of in-prompt filesystem + git logic.
- **Resume rule** → `next_pending_task_index` from the same JSON. Removes the "grep `git log --oneline` for `feat(impl)` commits since run baseline, cross-reference against plan tasks" instruction. The accuracy gain here is the main motivation: ordinal matching in a typed binary is reliable; commit-subject pattern matching in-prompt is not.

Net: ~10 skill lines reclaimed, accuracy win on resume.
