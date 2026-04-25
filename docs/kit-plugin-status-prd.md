# kit plugin-status — Product Requirements

**Version:** 1.0
**Subcommand of:** `kit` (universal binary, used by every `*kit` language plugin)
**Status:** proposed extension

---

## Purpose

A small subcommand that returns the install state of a Claude Code plugin (typically a contract plugin published by `jkit contract publish`). Skills consuming a contract plugin (e.g. `generate-feign`) currently improvise the "is the plugin installed?" check — there's no reliable in-prompt way to query Claude Code's plugin registry. This subcommand makes the check deterministic.

**Design principle:** read-only, side-effect-free. Returns enough metadata to drive subsequent skill decisions (SDK availability, contract path for OpenAPI input, plugin version) in one call.

---

## CLI

```
kit plugin-status <plugin-name>
```

| Argument | Description |
|---|---|
| `<plugin-name>` | Plugin name as published in marketplace.json — e.g. `billing-contract`. The argument may also be the bare service name (`billing`); the binary tries both. |

---

## Algorithm

1. Search the standard plugin install paths in order:
   - `<repo>/.claude/plugins/<name>/`
   - `<repo>/.claude/plugins/<name>-contract/`
   - `~/.claude/plugins/<name>/`
   - `~/.claude/plugins/<name>-contract/`
2. First hit wins. None → emit `installed: false` and exit 0 (not finding a plugin is not an error).
3. From the resolved plugin dir:
   - Read `.claude-plugin/plugin.json` for `name`, `version`, `skills[]`.
   - Read the contract `SKILL.md` (under `skills/<service-name>/`) for SDK detection — look for a `## SDK` heading and parse the `<dependency>` block underneath.
   - Locate `reference/contract.yaml` if present.
4. Emit JSON.

---

## Output

```json
{
  "plugin_name": "billing-contract",
  "installed": true,
  "plugin_path": ".claude/plugins/billing-contract/",
  "plugin_version": "1.0.0",
  "skill_name": "billing",
  "contract_yaml_path": ".claude/plugins/billing-contract/reference/contract.yaml",
  "sdk": {
    "present": true,
    "group_id": "com.example",
    "artifact_id": "billing-api",
    "version": "1.2.0"
  },
  "warnings": []
}
```

| Field | Type | Notes |
|---|---|---|
| `plugin_name` | string | Echo of resolved plugin (may differ from arg if `-contract` was appended) |
| `installed` | bool | False ⇒ rest of object may be `null` |
| `plugin_path` | string \| null | Absolute or repo-relative path to the plugin dir |
| `plugin_version` | string \| null | From `plugin.json` |
| `skill_name` | string \| null | The contract skill (typically the bare service name) |
| `contract_yaml_path` | string \| null | OpenAPI input for `openapi-generator-cli`; `null` if `reference/contract.yaml` missing |
| `sdk` | object \| null | Parsed from `## SDK` block; `null` (or `{"present": false}`) if absent |
| `warnings` | string[] | E.g. `"plugin.json missing 'version'"`, `"SKILL.md has SDK heading but no <dependency> block"` |

---

## Edge cases

| Case | Behavior |
|---|---|
| Plugin dir present but `plugin.json` missing | `installed: true`, set `warnings`, leave `plugin_version` null |
| Multiple plugin dirs match (project + user level) | Project (`<repo>/.claude/plugins/`) wins. Surface duplicate in `warnings`. |
| `## SDK` heading present but no `<dependency>` block | `sdk: {"present": false}`, surface in `warnings` |
| `reference/contract.yaml` missing but plugin otherwise present | `contract_yaml_path: null`; surface in `warnings`. Caller (e.g. generate-feign) treats the plugin as unusable for code-gen. |
| Symlinked plugin dir | Follow the symlink |

---

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (including `installed: false`) |
| 1 | I/O error reading a found plugin's files |

---

## Suggested dependencies

```toml
# Additions to existing jkit Cargo.toml — likely already present
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
serde_yaml    = "0.9"
pulldown-cmark = "0.12"   # parse SKILL.md for ## SDK block
```

---

## Impact on skills

- **generate-feign Step 2** → `kit plugin-status <service>`. Replaces the hand-wavy "check whether `/{service-name}` skill is available" with a deterministic call. Returns `contract_yaml_path` for OpenAPI input and `sdk` for the early SDK opt-in.

Future contract-consuming skills (any skill that reads a published contract) can use the same subcommand for installation gating.
