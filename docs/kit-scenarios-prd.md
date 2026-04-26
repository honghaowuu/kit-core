# kit scenarios — Product Requirements

**Version:** 1.0
**Subcommand of:** `kit` (universal binary, used by every `*kit` language plugin)
**Language:** Rust
**Status:** proposed (split from the previously-proposed standalone `scenarios` binary; the language-agnostic halves — `sync` and `skip` — live in `kit`, while `prereqs` and `gap` live with each language plugin: `jkit scenarios`, future `gkit scenarios`, etc.)

---

## Purpose

Two language-agnostic subcommands that own the OpenAPI/yaml/skip-list halves of the test-scenarios pipeline. The implementation has zero references to Java/Maven/Spring or any other language tooling — it operates on `api-spec.yaml` (OpenAPI v3), `test-scenarios.yaml` (flat manifest), and `<run>/skipped-scenarios.json` (per-run audit). Output is consumed by every language plugin's `<lang>kit scenarios gap` subcommand without modification.

| Subcommand | Owns |
|---|---|
| `kit scenarios sync` | Derive required test scenarios from `api-spec.yaml` and append missing entries to `test-scenarios.yaml` |
| `kit scenarios skip` | Record a per-run scenario skip so the lightweight gate doesn't re-prompt on resume |

(Implementation-loop subcommands `prereqs` and `gap` are language-specific — see `docs/jkit-scenarios-prd.md` for the Java variants.)

---

## Why not Schemathesis or similar?

Schemathesis is a **runtime fuzz tester** — it generates HTTP traffic from an OpenAPI spec and asserts live responses conform. It solves a different problem: dynamic conformance checking against a running server. It does not enumerate planning-level scenarios, does not maintain a test-case manifest, and cannot run until tests already exist.

Sync is a pre-implementation planning tool — it writes a scenario manifest that humans and AI consume when authoring integration tests in any language. Schemathesis is complementary; once tests exist, it can be added as a supplementary fuzzer. It is not a substitute.

Other alternatives surveyed (`dredd`, `prism`, `pact`, NIST `tcases`) are also runtime-oriented or produce output models that don't align with the flat-yaml convention. Chosen path: small Rust binary with a plain OpenAPI parser (`openapiv3` crate) plus local rules.

---

## Input files

| File | Used by | Purpose |
|---|---|---|
| `docs/domains/<domain>/api-spec.yaml` | sync | OpenAPI v3 source of truth for endpoints, required fields, response codes |
| `docs/domains/<domain>/test-scenarios.yaml` | sync (read + append), skip (read for endpoint lookup) | Flat YAML scenario manifest |
| `<run>/skipped-scenarios.json` | skip (read + write) | Per-run audit of scenarios the human chose to skip |

**test-scenarios.yaml schema** — flat list, one entry per scenario:

```yaml
- endpoint: "POST /invoices/bulk"
  id: happy-path
  description: valid list of 3 → 201 + invoice IDs

- endpoint: "POST /invoices/bulk"
  id: auth-missing-token
  description: missing token → 401
```

---

## CLI

```
kit scenarios sync <domain>
kit scenarios skip --run <dir> <domain> <id>
```

| Argument | Default | Description |
|---|---|---|
| `<domain>` | required | Domain name. Resolves to `docs/domains/<domain>/` |
| `--run <dir>` | — | Path to a `.jkit/<run>/` directory (skip only) |

---

## `sync` — scenario generation

### Algorithm

1. Parse `docs/domains/<domain>/api-spec.yaml` as OpenAPI v3.
2. For every endpoint (`method + path`), derive the required scenario set (table below).
3. Load existing `test-scenarios.yaml` (missing → treat as empty).
4. Build the set of existing keys: `(endpoint, id)` tuples.
5. For each derived scenario not present, **append** it to the yaml.
6. Never modify, reorder, or remove existing entries.
7. Warn (stderr) about orphaned entries — yaml `endpoint` values no longer in the spec — but do not prune them.

### Derivation table

| Source in api-spec.yaml | Scenario ID |
|---|---|
| Always | `happy-path` |
| Each `required` field in request body | `validation-<field>-missing` |
| Each `required` field in query params | `validation-query-<field>-missing` |
| Each `required` field in path params | `validation-path-<field>-missing` |
| Response `400` or `422` | `validation-<description-slug>` (fallback: `validation-bad-request`) |
| Response `401` | `auth-missing-token` |
| Response `403` | `auth-<description-slug>` (fallback: `auth-forbidden`) |
| Response `404` | `not-found` |
| Response `409` | `business-<description-slug>` (fallback: `business-conflict`) |

`<description-slug>` = kebab-cased response description text. The category prefix comes from the row, not the description (e.g. description `"Duplicate idempotency key"` + row prefix `business-` → `business-duplicate-idempotency-key`).

### Edge cases

| Case | Behavior |
|---|---|
| Non-2xx response has no `description` | Use fallback ID (`validation-bad-request`, `auth-forbidden`, `business-conflict`) |
| Two responses with identical slug under one endpoint | Keep first, warn to stderr for the rest |
| Human-added scenario ID not produced by the table | Preserve untouched (append-only guarantees this) |
| Endpoint present in yaml but removed from spec | Preserve; warn to stderr — humans may still want the history |
| `required` list absent on a body schema | Treat as no required fields; skip `validation-<field>-missing` rows |
| `api-spec.yaml` missing | Exit 1 — sync requires a spec |
| `test-scenarios.yaml` missing | Create it with derived entries |
| `oneOf` / `anyOf` body schemas | Apply to each branch independently; dedupe by `(endpoint, id)` |
| Body or schema is a `$ref` to `#/components/...` | Resolved against `components.schemas` / `components.requestBodies`; cycles are guarded by a visited set |

### Output formatting

- Use `serde_yaml` for write; preserve a blank line between entries to match the hand-maintained style.
- If no new entries were appended, **do not rewrite the file** — avoid spurious diffs.
- On success, stderr: `sync: <N> added, <M> already present, <K> orphaned`.

---

## `skip` — record a per-run scenario skip

When the lightweight gate in `<lang>kit scenarios gap` accepts "skip this scenario," that decision needs to be recorded so resume (re-running `gap`) doesn't re-prompt.

### Algorithm

1. Resolve `<dir>/skipped-scenarios.json`. Create with `[]` if absent.
2. Look up the scenario's `endpoint` from `docs/domains/<domain>/test-scenarios.yaml` (so the caller only needs `<domain> <id>`).
3. Append `{"domain": "...", "endpoint": "...", "id": "..."}` if not already present.
4. Idempotent — re-skipping is a no-op.

### CLI example

```
kit scenarios skip --run .jkit/2026-04-25-foo billing happy-path
```

### Exit codes

| Code | Meaning |
|---|---|
| `0` | Skip recorded (or already present) |
| `1` | `<dir>` missing, scenario id not found in domain's `test-scenarios.yaml`, or I/O error |

Per-run only. Permanent skips (scenarios that should never get tests) are out of scope; revisit as a `skip: true` flag on `test-scenarios.yaml` entries if the need arises.

---

## Default (no subcommand)

Future enhancement: `kit scenarios <domain>` could chain `sync` then dispatch to the active language plugin's `gap`. Out of scope for v1.0 — explicit invocation per subcommand for now.

---

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success (including zero new entries, zero gaps, idempotent skip) |
| `1` | YAML parse failure, OpenAPI parse failure, missing required file, I/O failure |

---

## Suggested dependencies

```toml
[dependencies]
serde      = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
serde_json = "1"
clap       = { version = "4", features = ["derive"] }
openapiv3  = "2"
heck       = "0.5"
```

(Note: `quick-xml` and `which` are not needed here — they live in `jkit`'s side of the split, since pom-parsing and runtime-probing are Java-specific.)

---

## Impact on spec-delta

- **Step 7b** ("Sync test-scenarios.yaml") → `kit scenarios sync <domain>` per affected domain. The in-prompt derivation table is removed from the skill — the binary owns that logic.
- **Step 9** ("Scenario gap detection") → routes to the active language plugin's `gap` (e.g. `jkit scenarios gap <domain>`).

---

## Impact on scenario-tdd

The skip step uses this binary; everything else routes through the language-specific PRD. See `docs/jkit-scenarios-prd.md` for the Java path.
