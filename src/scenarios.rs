use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use heck::ToKebabCase;
use openapiv3::{
    Components, OpenAPI, Operation, Parameter, ReferenceOr, RequestBody, Response, Schema,
    SchemaKind, Type,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Subcommand)]
pub enum ScenarioCmd {
    /// Derive required scenarios for a domain and append into top-level
    /// `docs/test-scenarios.yaml`. Three input modes:
    ///
    ///   `--from-code` — runs `jkit api-spec show <slug>` to pull the
    ///       current code's surface (smartdoc on controllers). Used by
    ///       /migrate-project for backfill and ad-hoc re-derivation.
    ///
    ///   `--proposed <path>` — reads the run's `proposed-api.yaml` (the
    ///       writing-plans output). Used during the spec-delta /
    ///       writing-plans phase before code exists.
    ///
    ///   default (no flag) — reads `docs/domains/<slug>/api-spec.yaml`.
    ///       Legacy path; only useful for projects that still have the
    ///       per-domain layout. Errors when the file is absent.
    Sync {
        /// Domain slug; controls which `domains.<slug>` entry gets the
        /// derived scenarios.
        domain: String,
        /// Pull the API surface from live controllers via
        /// `jkit api-spec show <domain>`.
        #[arg(long, conflicts_with = "proposed")]
        from_code: bool,
        /// Read the API surface from a `proposed-api.yaml` file.
        #[arg(long, conflicts_with = "from_code")]
        proposed: Option<PathBuf>,
    },
    /// Record a per-run scenario skip. For multi-API-type domains pass
    /// `--api-type` when the scenario id is ambiguous across types (the
    /// command will refuse to guess).
    Skip {
        /// Path to a .jkit/<run>/ directory.
        #[arg(long)]
        run: PathBuf,
        /// Domain name; resolves to docs/domains/<domain>/.
        domain: String,
        /// Scenario id from test-scenarios.yaml.
        id: String,
        /// API type to disambiguate when multi-type and the id matches
        /// scenarios in more than one per-type file. One of `web-api`,
        /// `microservice-api`, `open-api`.
        #[arg(long)]
        api_type: Option<String>,
    },
}

pub fn run(cmd: ScenarioCmd) -> Result<ExitCode> {
    match cmd {
        ScenarioCmd::Sync {
            domain,
            from_code,
            proposed,
        } => match (from_code, proposed) {
            (true, _) => sync_from_code(&domain),
            (false, Some(path)) => sync_from_proposed(&domain, &path),
            (false, None) => sync(&domain),
        },
        ScenarioCmd::Skip {
            run,
            domain,
            id,
            api_type,
        } => skip(&run, &domain, &id, api_type.as_deref()),
    }
}

/// `--from-code` — shell out to `jkit api-spec show <domain>`, parse the
/// returned `spec` field as OpenAPI, derive scenarios, and append them
/// under `domains.<slug>` in the top-level `docs/test-scenarios.yaml`
/// (flat list — controllers within a single slug are treated as one
/// surface here; multi-type splits happen via separate per-type sync
/// calls).
fn sync_from_code(domain: &str) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;
    let output = std::process::Command::new("jkit")
        .args(["api-spec", "show", domain])
        .output()
        .context("invoking `jkit api-spec show`")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stdout);
        return Err(anyhow!(
            "jkit api-spec show {domain} failed: {}",
            err.trim()
        ));
    }
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("parsing jkit api-spec show output as JSON")?;
    if envelope.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err(anyhow!(
            "jkit api-spec show {domain} returned ok:false: {}",
            envelope
        ));
    }
    let spec_value = envelope
        .get("spec")
        .ok_or_else(|| anyhow!("jkit api-spec show output missing `spec` field"))?;
    let spec: OpenAPI = serde_json::from_value(spec_value.clone())
        .context("parsing api-spec output as OpenAPI 3.x")?;
    sync_from_spec(&cwd, domain, None, &spec)
}

/// `--proposed <path>` — load the proposal YAML, derive scenarios,
/// append under `domains.<slug>`. The proposal is a strict OpenAPI 3.x
/// subset (writing-plans authors it).
fn sync_from_proposed(domain: &str, proposed_path: &Path) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;
    let abs = if proposed_path.is_absolute() {
        proposed_path.to_path_buf()
    } else {
        cwd.join(proposed_path)
    };
    if !abs.is_file() {
        return Err(anyhow!(
            "proposed-api.yaml not found: {}",
            proposed_path.display()
        ));
    }
    let text = std::fs::read_to_string(&abs)
        .with_context(|| format!("reading {}", abs.display()))?;
    let spec: OpenAPI = serde_yaml::from_str(&text)
        .with_context(|| format!("parsing {} as OpenAPI 3.x", abs.display()))?;
    sync_from_spec(&cwd, domain, None, &spec)
}

/// Shared write path: derive scenarios from a parsed OpenAPI doc and
/// merge them into `docs/test-scenarios.yaml` under `domains.<domain>`
/// (or `domains.<domain>.<api_type>` when `api_type` is `Some`).
fn sync_from_spec(
    cwd: &Path,
    domain: &str,
    api_type: Option<&str>,
    spec: &OpenAPI,
) -> Result<ExitCode> {
    let derived = derive_scenarios(spec);

    let mut scenarios_file = crate::scenarios_yaml::ScenariosFile::load(cwd)?;
    let existing: Vec<crate::scenarios_yaml::ScenarioEntry> =
        match scenarios_file.section(domain)? {
            Some(crate::scenarios_yaml::SlugSection::Flat(v)) if api_type.is_none() => v,
            Some(crate::scenarios_yaml::SlugSection::PerApiType(buckets)) => {
                if let Some(ty) = api_type {
                    buckets
                        .into_iter()
                        .find(|(t, _)| t == ty)
                        .map(|(_, v)| v)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };

    let existing_keys: BTreeSet<(String, String)> = existing
        .iter()
        .map(|e| (e.endpoint.clone(), e.id.clone()))
        .collect();

    let mut combined: Vec<crate::scenarios_yaml::ScenarioEntry> = existing.clone();
    let mut seen: BTreeSet<(String, String)> = existing_keys.clone();
    let mut n_added = 0usize;
    for e in derived.iter() {
        let key = (e.endpoint.clone(), e.id.clone());
        if !seen.contains(&key) {
            combined.push(crate::scenarios_yaml::ScenarioEntry {
                endpoint: e.endpoint.clone(),
                id: e.id.clone(),
                description: e.description.clone(),
            });
            seen.insert(key);
            n_added += 1;
        }
    }

    let n_present = derived.len().saturating_sub(n_added);

    if n_added > 0 {
        scenarios_file.put_entries(domain, api_type, &combined)?;
        scenarios_file.save()?;
    }

    eprintln!(
        "sync{}: {} added, {} already present",
        api_type
            .map(|t| format!(" [{t}]"))
            .unwrap_or_default(),
        n_added,
        n_present,
    );

    crate::envelope::print_ok(serde_json::json!({
        "domain": domain,
        "api_type": api_type,
        "added": n_added,
        "already_present": n_present,
        "scenarios_path": "docs/test-scenarios.yaml",
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenarioEntry {
    pub endpoint: String,
    pub id: String,
    pub description: String,
}

/// Per-domain layout detection — local copy of jkit-cli's `domain_layout`
/// minimal subset. kit-core and jkit-cli live in separate repos, so keeping
/// this in sync is convention rather than enforced.
const API_TYPES: &[&str] = &["web-api", "microservice-api", "open-api"];

#[derive(Debug, Clone)]
struct DomainBucket {
    api_type: Option<String>, // None for flat layout
    spec_path: PathBuf,
}

fn buckets_for(domain_dir: &Path) -> Vec<DomainBucket> {
    let mut multi: Vec<DomainBucket> = Vec::new();
    for ty in API_TYPES {
        let sub = domain_dir.join(ty);
        let spec = sub.join("api-spec.yaml");
        if sub.is_dir() && spec.exists() {
            multi.push(DomainBucket {
                api_type: Some((*ty).to_string()),
                spec_path: spec,
            });
        }
    }
    if !multi.is_empty() {
        return multi;
    }
    vec![DomainBucket {
        api_type: None,
        spec_path: domain_dir.join("api-spec.yaml"),
    }]
}

pub fn sync(domain: &str) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;
    let domain_dir = cwd.join("docs").join("domains").join(domain);
    let buckets = buckets_for(&domain_dir);

    // Refuse if the only bucket is flat and its spec doesn't exist — caller
    // is misusing the command. (api-spec.yaml input still comes from the
    // legacy per-domain location until the proposed-api.yaml input mode lands.)
    if buckets.len() == 1 && buckets[0].api_type.is_none() && !buckets[0].spec_path.is_file() {
        return Err(anyhow!(
            "missing api-spec.yaml at {} — neither flat nor any per-type subdir found",
            buckets[0].spec_path.display()
        ));
    }

    let multi_type = buckets.len() > 1 || buckets[0].api_type.is_some();
    let mut scenarios_file = crate::scenarios_yaml::ScenariosFile::load(&cwd)?;
    let mut per_type_results: Vec<serde_json::Value> = Vec::new();
    let mut total_added = 0usize;
    let mut total_present = 0usize;
    let mut total_orphan = 0usize;

    for bucket in &buckets {
        if !bucket.spec_path.is_file() {
            continue;
        }

        let spec_text = std::fs::read_to_string(&bucket.spec_path)
            .with_context(|| format!("failed to read {}", bucket.spec_path.display()))?;
        let spec: OpenAPI = serde_yaml::from_str(&spec_text).with_context(|| {
            format!(
                "failed to parse OpenAPI v3 from {}",
                bucket.spec_path.display()
            )
        })?;

        let derived = derive_scenarios(&spec);

        // Existing entries for this (slug, api_type) come from the top-level
        // file. For multi-type domains we read the per-bucket section; for
        // single-type we read the flat list.
        let existing: Vec<crate::scenarios_yaml::ScenarioEntry> =
            match scenarios_file.section(domain)? {
                Some(crate::scenarios_yaml::SlugSection::Flat(v)) if bucket.api_type.is_none() => v,
                Some(crate::scenarios_yaml::SlugSection::PerApiType(buckets_existing)) => {
                    if let Some(ty) = &bucket.api_type {
                        buckets_existing
                            .into_iter()
                            .find(|(t, _)| t == ty)
                            .map(|(_, v)| v)
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            };

        let existing_keys: BTreeSet<(String, String)> = existing
            .iter()
            .map(|e| (e.endpoint.clone(), e.id.clone()))
            .collect();

        let mut combined: Vec<crate::scenarios_yaml::ScenarioEntry> = existing.clone();
        let mut seen: BTreeSet<(String, String)> = existing_keys.clone();
        let mut n_added = 0usize;
        for e in derived.iter() {
            let key = (e.endpoint.clone(), e.id.clone());
            if !seen.contains(&key) {
                combined.push(crate::scenarios_yaml::ScenarioEntry {
                    endpoint: e.endpoint.clone(),
                    id: e.id.clone(),
                    description: e.description.clone(),
                });
                seen.insert(key);
                n_added += 1;
            }
        }

        let spec_endpoints: BTreeSet<String> =
            derived.iter().map(|e| e.endpoint.clone()).collect();
        let mut orphan_count = 0usize;
        for e in &existing {
            if !spec_endpoints.contains(&e.endpoint) {
                eprintln!(
                    "sync{}: orphaned entry — endpoint '{}' (id '{}') no longer in spec",
                    bucket
                        .api_type
                        .as_deref()
                        .map(|t| format!(" [{t}]"))
                        .unwrap_or_default(),
                    e.endpoint,
                    e.id
                );
                orphan_count += 1;
            }
        }

        let n_present = derived.len() - n_added;

        if n_added > 0 {
            scenarios_file.put_entries(domain, bucket.api_type.as_deref(), &combined)?;
        }

        eprintln!(
            "sync{}: {} added, {} already present, {} orphaned",
            bucket
                .api_type
                .as_deref()
                .map(|t| format!(" [{t}]"))
                .unwrap_or_default(),
            n_added,
            n_present,
            orphan_count
        );

        let mut entry = serde_json::Map::new();
        if let Some(ty) = &bucket.api_type {
            entry.insert("api_type".into(), serde_json::Value::String(ty.clone()));
        }
        entry.insert("added".into(), serde_json::json!(n_added));
        entry.insert("already_present".into(), serde_json::json!(n_present));
        entry.insert("orphaned".into(), serde_json::json!(orphan_count));
        per_type_results.push(serde_json::Value::Object(entry));

        total_added += n_added;
        total_present += n_present;
        total_orphan += orphan_count;
    }

    if total_added > 0 {
        scenarios_file.save()?;
    }

    crate::envelope::print_ok(serde_json::json!({
        "domain": domain,
        "multi_type": multi_type,
        "buckets": per_type_results,
        "added": total_added,
        "already_present": total_present,
        "orphaned": total_orphan,
        "scenarios_path": format!("docs/test-scenarios.yaml"),
    }))
}

pub fn skip(
    run_dir: &Path,
    domain: &str,
    id: &str,
    api_type: Option<&str>,
) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;
    let run = if run_dir.is_absolute() {
        run_dir.to_path_buf()
    } else {
        cwd.join(run_dir)
    };
    if !run.is_dir() {
        return Err(anyhow!("run dir missing: {}", run_dir.display()));
    }

    let scenarios_file = crate::scenarios_yaml::ScenariosFile::load(&cwd)?;
    let section = scenarios_file
        .section(domain)?
        .ok_or_else(|| anyhow!("domain '{}' has no entry in docs/test-scenarios.yaml", domain))?;

    // Find the scenario id, optionally constrained to one api-type. When
    // multiple matches exist across api-types, require --api-type to
    // disambiguate.
    let mut matches: Vec<(Option<String>, crate::scenarios_yaml::ScenarioEntry)> = Vec::new();
    for (ty, entry) in section.iter_with_type() {
        if let Some(filter_ty) = api_type {
            if ty != Some(filter_ty) {
                continue;
            }
        }
        if entry.id == id {
            matches.push((ty.map(str::to_string), entry.clone()));
        }
    }

    if matches.is_empty() {
        return Err(anyhow!(
            "scenario id '{}' not found in domain '{}'{}",
            id,
            domain,
            api_type
                .map(|t| format!(" under api-type '{t}'"))
                .unwrap_or_default()
        ));
    }
    if matches.len() > 1 {
        let types: Vec<&str> = matches
            .iter()
            .filter_map(|(t, _)| t.as_deref())
            .collect();
        return Err(anyhow!(
            "scenario id '{}' is ambiguous in domain '{}' across api-types: {} — pass --api-type to disambiguate",
            id,
            domain,
            types.join(", ")
        ));
    }
    let (matched_ty, entry) = matches.into_iter().next().unwrap();

    let skipped_path = run.join("skipped-scenarios.json");
    let mut current: Vec<serde_json::Value> = if skipped_path.is_file() {
        let r = std::fs::read_to_string(&skipped_path)?;
        if r.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&r)?
        }
    } else {
        Vec::new()
    };

    let already = current.iter().any(|v| {
        v.get("domain").and_then(|x| x.as_str()) == Some(domain)
            && v.get("id").and_then(|x| x.as_str()) == Some(id)
            && v.get("api_type").and_then(|x| x.as_str()) == matched_ty.as_deref()
    });

    if !already {
        let mut record = serde_json::Map::new();
        record.insert("domain".into(), serde_json::Value::String(domain.to_string()));
        record.insert(
            "endpoint".into(),
            serde_json::Value::String(entry.endpoint.clone()),
        );
        record.insert("id".into(), serde_json::Value::String(id.to_string()));
        if let Some(ty) = &matched_ty {
            record.insert("api_type".into(), serde_json::Value::String(ty.clone()));
        }
        current.push(serde_json::Value::Object(record));
        let pretty = serde_json::to_string_pretty(&current)?;
        std::fs::write(&skipped_path, format!("{}\n", pretty))?;
    }

    let mut response = serde_json::Map::new();
    response.insert("domain".into(), serde_json::Value::String(domain.to_string()));
    response.insert(
        "endpoint".into(),
        serde_json::Value::String(entry.endpoint.clone()),
    );
    response.insert("id".into(), serde_json::Value::String(id.to_string()));
    if let Some(ty) = &matched_ty {
        response.insert("api_type".into(), serde_json::Value::String(ty.clone()));
    }
    response.insert("already_present".into(), serde_json::json!(already));
    response.insert(
        "path".into(),
        serde_json::Value::String(skipped_path.display().to_string()),
    );
    crate::envelope::print_ok(serde_json::Value::Object(response))
}

pub fn derive_scenarios(spec: &OpenAPI) -> Vec<ScenarioEntry> {
    let mut out: Vec<ScenarioEntry> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let components = spec.components.as_ref();

    let push = |out: &mut Vec<ScenarioEntry>,
                    seen: &mut BTreeSet<(String, String)>,
                    endpoint: &str,
                    id: String,
                    description: String| {
        let key = (endpoint.to_string(), id.clone());
        if seen.insert(key) {
            out.push(ScenarioEntry {
                endpoint: endpoint.to_string(),
                id,
                description,
            });
        }
    };

    for (path, item) in spec.paths.iter() {
        let item = match item {
            ReferenceOr::Item(it) => it,
            ReferenceOr::Reference { .. } => continue,
        };
        for (method, op) in operations(item) {
            let endpoint = format!("{} {}", method.to_uppercase(), path);

            // Always
            push(
                &mut out,
                &mut seen,
                &endpoint,
                "happy-path".to_string(),
                "happy path".to_string(),
            );

            // Required body fields
            for (field, _) in required_body_fields(op, components) {
                let id = format!("validation-{}-missing", field.to_kebab_case());
                let desc = format!("missing required field '{}' → 4xx", field);
                push(&mut out, &mut seen, &endpoint, id, desc);
            }
            // Required query/path params
            for (loc, name) in required_params(op) {
                let id = match loc.as_str() {
                    "query" => format!("validation-query-{}-missing", name.to_kebab_case()),
                    "path" => format!("validation-path-{}-missing", name.to_kebab_case()),
                    other => format!("validation-{}-{}-missing", other, name.to_kebab_case()),
                };
                let desc = format!("missing required {} param '{}' → 4xx", loc, name);
                push(&mut out, &mut seen, &endpoint, id, desc);
            }

            // Response codes
            let mut local_seen_slug: BTreeSet<String> = BTreeSet::new();
            for (code, resp) in iter_responses(op) {
                let description = response_description(resp);
                let slug = description
                    .as_deref()
                    .map(|d| d.to_kebab_case())
                    .filter(|s| !s.is_empty());
                let (prefix, fallback) = match code.as_str() {
                    "400" | "422" => ("validation-", "validation-bad-request"),
                    "401" => ("auth-", "auth-missing-token"),
                    "403" => ("auth-", "auth-forbidden"),
                    "404" => ("", "not-found"),
                    "409" => ("business-", "business-conflict"),
                    _ => continue,
                };
                let id = if code == "401" {
                    "auth-missing-token".to_string()
                } else if code == "404" {
                    "not-found".to_string()
                } else {
                    match &slug {
                        Some(s) => format!("{}{}", prefix, s),
                        None => fallback.to_string(),
                    }
                };
                if !local_seen_slug.insert(id.clone()) {
                    eprintln!(
                        "sync: duplicate scenario slug '{}' under {}; keeping first",
                        id, endpoint
                    );
                    continue;
                }
                let desc = description.unwrap_or_else(|| format!("response {}", code));
                push(&mut out, &mut seen, &endpoint, id, desc);
            }
        }
    }
    out
}

fn operations(item: &openapiv3::PathItem) -> Vec<(&'static str, &Operation)> {
    let mut v = Vec::new();
    if let Some(op) = &item.get { v.push(("get", op)); }
    if let Some(op) = &item.put { v.push(("put", op)); }
    if let Some(op) = &item.post { v.push(("post", op)); }
    if let Some(op) = &item.delete { v.push(("delete", op)); }
    if let Some(op) = &item.options { v.push(("options", op)); }
    if let Some(op) = &item.head { v.push(("head", op)); }
    if let Some(op) = &item.patch { v.push(("patch", op)); }
    if let Some(op) = &item.trace { v.push(("trace", op)); }
    v
}

fn iter_responses(op: &Operation) -> Vec<(String, &Response)> {
    let mut v = Vec::new();
    for (sc, resp) in &op.responses.responses {
        let code = match sc {
            openapiv3::StatusCode::Code(n) => n.to_string(),
            openapiv3::StatusCode::Range(r) => format!("{}xx", r),
        };
        if let ReferenceOr::Item(r) = resp {
            v.push((code, r));
        }
    }
    v
}

fn response_description(r: &Response) -> Option<String> {
    let d = r.description.trim();
    if d.is_empty() { None } else { Some(d.to_string()) }
}

fn required_params(op: &Operation) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for p in &op.parameters {
        if let ReferenceOr::Item(param) = p {
            let (name, location, required) = match param {
                Parameter::Query { parameter_data, .. } => (
                    parameter_data.name.clone(),
                    "query".to_string(),
                    parameter_data.required,
                ),
                Parameter::Path { parameter_data, .. } => (
                    parameter_data.name.clone(),
                    "path".to_string(),
                    parameter_data.required,
                ),
                Parameter::Header { parameter_data, .. } => (
                    parameter_data.name.clone(),
                    "header".to_string(),
                    parameter_data.required,
                ),
                Parameter::Cookie { parameter_data, .. } => (
                    parameter_data.name.clone(),
                    "cookie".to_string(),
                    parameter_data.required,
                ),
            };
            if required && (location == "query" || location == "path") {
                v.push((location, name));
            }
        }
    }
    v
}

fn required_body_fields(
    op: &Operation,
    components: Option<&Components>,
) -> Vec<(String, ())> {
    let Some(body_ref) = &op.request_body else { return Vec::new() };
    let body: &RequestBody = match body_ref {
        ReferenceOr::Item(b) => b,
        ReferenceOr::Reference { reference } => match resolve_request_body_ref(reference, components) {
            Some(b) => b,
            None => return Vec::new(),
        },
    };
    let mut fields: BTreeSet<String> = BTreeSet::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    for (_ct, mt) in &body.content {
        let Some(schema_ref) = &mt.schema else { continue };
        collect_required_fields_ref(schema_ref, components, &mut fields, &mut visited);
    }
    fields.into_iter().map(|f| (f, ())).collect()
}

fn collect_required_fields_ref(
    schema_ref: &ReferenceOr<Schema>,
    components: Option<&Components>,
    out: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) {
    match schema_ref {
        ReferenceOr::Item(s) => collect_required_fields(s, components, out, visited),
        ReferenceOr::Reference { reference } => {
            if !visited.insert(reference.clone()) {
                return;
            }
            if let Some(s) = resolve_schema_ref(reference, components) {
                collect_required_fields(s, components, out, visited);
            }
        }
    }
}

fn collect_required_fields(
    schema: &Schema,
    components: Option<&Components>,
    out: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) {
    match &schema.schema_kind {
        SchemaKind::Type(Type::Object(obj)) => {
            for r in &obj.required {
                out.insert(r.clone());
            }
            // allOf-style composition is sometimes spelled as object + ref'd
            // properties; nothing else to recurse into here.
        }
        SchemaKind::OneOf { one_of } | SchemaKind::AnyOf { any_of: one_of } => {
            for branch in one_of {
                collect_required_fields_ref(branch, components, out, visited);
            }
        }
        SchemaKind::AllOf { all_of } => {
            for branch in all_of {
                collect_required_fields_ref(branch, components, out, visited);
            }
        }
        _ => {}
    }
}

fn resolve_schema_ref<'a>(
    reference: &str,
    components: Option<&'a Components>,
) -> Option<&'a Schema> {
    let name = reference.strip_prefix("#/components/schemas/")?;
    let comp = components?;
    match comp.schemas.get(name)? {
        ReferenceOr::Item(s) => Some(s),
        ReferenceOr::Reference { reference } => resolve_schema_ref(reference, Some(comp)),
    }
}

fn resolve_request_body_ref<'a>(
    reference: &str,
    components: Option<&'a Components>,
) -> Option<&'a RequestBody> {
    let name = reference.strip_prefix("#/components/requestBodies/")?;
    let comp = components?;
    match comp.request_bodies.get(name)? {
        ReferenceOr::Item(b) => Some(b),
        ReferenceOr::Reference { reference } => resolve_request_body_ref(reference, Some(comp)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> OpenAPI {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn happy_path_always_present() {
        let spec = parse(
            r#"
openapi: 3.0.3
info: {title: x, version: '1'}
paths:
  /a:
    get:
      responses:
        '200': {description: ok}
"#,
        );
        let s = derive_scenarios(&spec);
        assert!(s.iter().any(|e| e.endpoint == "GET /a" && e.id == "happy-path"));
    }

    #[test]
    fn validation_required_body_field() {
        let spec = parse(
            r#"
openapi: 3.0.3
info: {title: x, version: '1'}
paths:
  /a:
    post:
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [customerId]
              properties:
                customerId: {type: string}
      responses:
        '201': {description: created}
"#,
        );
        let s = derive_scenarios(&spec);
        assert!(
            s.iter()
                .any(|e| e.endpoint == "POST /a" && e.id == "validation-customer-id-missing"),
            "got: {:?}",
            s
        );
    }

    #[test]
    fn auth_and_business_codes() {
        let spec = parse(
            r#"
openapi: 3.0.3
info: {title: x, version: '1'}
paths:
  /a:
    post:
      responses:
        '201': {description: ok}
        '401': {description: missing token}
        '409': {description: Duplicate idempotency key}
        '404': {description: not found}
"#,
        );
        let s = derive_scenarios(&spec);
        let ids: Vec<&str> = s.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"auth-missing-token"));
        assert!(ids.contains(&"business-duplicate-idempotency-key"));
        assert!(ids.contains(&"not-found"));
    }

    #[test]
    fn validation_required_body_field_via_ref() {
        let spec = parse(
            r#"
openapi: 3.0.3
info: {title: x, version: '1'}
paths:
  /a:
    post:
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateInvoice'
      responses:
        '201': {description: created}
components:
  schemas:
    CreateInvoice:
      type: object
      required: [customerId, amount]
      properties:
        customerId: {type: string}
        amount: {type: number}
"#,
        );
        let s = derive_scenarios(&spec);
        let ids: Vec<&str> = s.iter().map(|e| e.id.as_str()).collect();
        assert!(
            ids.contains(&"validation-customer-id-missing"),
            "got: {:?}",
            ids
        );
        assert!(ids.contains(&"validation-amount-missing"), "got: {:?}", ids);
    }

    #[test]
    fn validation_required_body_field_via_allof_with_ref() {
        let spec = parse(
            r#"
openapi: 3.0.3
info: {title: x, version: '1'}
paths:
  /a:
    post:
      requestBody:
        required: true
        content:
          application/json:
            schema:
              allOf:
                - $ref: '#/components/schemas/Base'
                - type: object
                  required: [extra]
                  properties:
                    extra: {type: string}
      responses:
        '201': {description: created}
components:
  schemas:
    Base:
      type: object
      required: [baseId]
      properties:
        baseId: {type: string}
"#,
        );
        let s = derive_scenarios(&spec);
        let ids: Vec<&str> = s.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"validation-base-id-missing"), "got: {:?}", ids);
        assert!(ids.contains(&"validation-extra-missing"), "got: {:?}", ids);
    }

    #[test]
    fn required_query_param() {
        let spec = parse(
            r#"
openapi: 3.0.3
info: {title: x, version: '1'}
paths:
  /a:
    get:
      parameters:
        - name: page
          in: query
          required: true
          schema: {type: integer}
      responses:
        '200': {description: ok}
"#,
        );
        let s = derive_scenarios(&spec);
        assert!(s.iter().any(|e| e.id == "validation-query-page-missing"));
    }
}
