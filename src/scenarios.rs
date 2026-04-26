use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use heck::ToKebabCase;
use openapiv3::{
    OpenAPI, Operation, Parameter, ReferenceOr, RequestBody, Response, Schema, SchemaKind, Type,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Subcommand)]
pub enum ScenarioCmd {
    /// Derive required scenarios from api-spec.yaml; append-only into test-scenarios.yaml.
    Sync {
        /// Domain name; resolves to docs/domains/<domain>/.
        domain: String,
    },
    /// Record a per-run scenario skip.
    Skip {
        /// Path to a .jkit/<run>/ directory.
        #[arg(long)]
        run: PathBuf,
        /// Domain name; resolves to docs/domains/<domain>/.
        domain: String,
        /// Scenario id from test-scenarios.yaml.
        id: String,
    },
}

pub fn run(cmd: ScenarioCmd) -> Result<ExitCode> {
    match cmd {
        ScenarioCmd::Sync { domain } => sync(&domain),
        ScenarioCmd::Skip { run, domain, id } => skip(&run, &domain, &id),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenarioEntry {
    pub endpoint: String,
    pub id: String,
    pub description: String,
}

pub fn sync(domain: &str) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;
    let domain_dir = cwd.join("docs").join("domains").join(domain);
    let spec_path = domain_dir.join("api-spec.yaml");
    let scen_path = domain_dir.join("test-scenarios.yaml");

    if !spec_path.is_file() {
        return Err(anyhow!(
            "missing api-spec.yaml at {}",
            spec_path.display()
        ));
    }

    let spec_text = std::fs::read_to_string(&spec_path)
        .with_context(|| format!("failed to read {}", spec_path.display()))?;
    let spec: OpenAPI = serde_yaml::from_str(&spec_text)
        .with_context(|| format!("failed to parse OpenAPI v3 from {}", spec_path.display()))?;

    let derived = derive_scenarios(&spec);

    let existing: Vec<ScenarioEntry> = if scen_path.is_file() {
        let raw = std::fs::read_to_string(&scen_path)
            .with_context(|| format!("failed to read {}", scen_path.display()))?;
        if raw.trim().is_empty() {
            Vec::new()
        } else {
            serde_yaml::from_str(&raw)
                .with_context(|| format!("failed to parse {}", scen_path.display()))?
        }
    } else {
        Vec::new()
    };

    let existing_keys: BTreeSet<(String, String)> = existing
        .iter()
        .map(|e| (e.endpoint.clone(), e.id.clone()))
        .collect();

    // New entries to append, in derivation order, deduped.
    let mut to_append: Vec<ScenarioEntry> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = existing_keys.clone();
    for e in derived.iter() {
        let key = (e.endpoint.clone(), e.id.clone());
        if !seen.contains(&key) {
            to_append.push(e.clone());
            seen.insert(key);
        }
    }

    // Orphans: yaml entries whose endpoint isn't in the spec.
    let spec_endpoints: BTreeSet<String> = derived.iter().map(|e| e.endpoint.clone()).collect();
    let mut orphan_count = 0usize;
    for e in &existing {
        if !spec_endpoints.contains(&e.endpoint) {
            eprintln!(
                "sync: orphaned entry — endpoint '{}' (id '{}') no longer in spec",
                e.endpoint, e.id
            );
            orphan_count += 1;
        }
    }

    let n_added = to_append.len();
    let n_present = derived.len() - n_added;

    if n_added > 0 {
        // Append: write existing entries followed by appended entries, preserving blank lines.
        let mut combined = existing.clone();
        combined.extend(to_append.into_iter());
        write_scenarios_yaml(&scen_path, &combined)?;
    }

    eprintln!(
        "sync: {} added, {} already present, {} orphaned",
        n_added, n_present, orphan_count
    );

    crate::envelope::print_ok(serde_json::json!({
        "domain": domain,
        "added": n_added,
        "already_present": n_present,
        "orphaned": orphan_count,
    }))
}

pub fn skip(run_dir: &Path, domain: &str, id: &str) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;
    let run = if run_dir.is_absolute() {
        run_dir.to_path_buf()
    } else {
        cwd.join(run_dir)
    };
    if !run.is_dir() {
        return Err(anyhow!("run dir missing: {}", run_dir.display()));
    }
    let domain_dir = cwd.join("docs").join("domains").join(domain);
    let scen_path = domain_dir.join("test-scenarios.yaml");
    if !scen_path.is_file() {
        return Err(anyhow!(
            "test-scenarios.yaml missing for domain '{}'",
            domain
        ));
    }
    let raw = std::fs::read_to_string(&scen_path)?;
    let entries: Vec<ScenarioEntry> = serde_yaml::from_str(&raw)?;
    let entry = entries
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| anyhow!("scenario id '{}' not found in {}", id, scen_path.display()))?;

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
    });

    if !already {
        current.push(serde_json::json!({
            "domain": domain,
            "endpoint": entry.endpoint,
            "id": id,
        }));
        let pretty = serde_json::to_string_pretty(&current)?;
        std::fs::write(&skipped_path, format!("{}\n", pretty))?;
    }

    crate::envelope::print_ok(serde_json::json!({
        "domain": domain,
        "endpoint": entry.endpoint,
        "id": id,
        "already_present": already,
        "path": skipped_path.display().to_string(),
    }))
}

fn write_scenarios_yaml(path: &Path, entries: &[ScenarioEntry]) -> Result<()> {
    // Render with a blank line between entries to match hand-maintained style.
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "- endpoint: {}\n  id: {}\n  description: {}\n",
            yaml_scalar(&e.endpoint),
            yaml_scalar(&e.id),
            yaml_scalar(&e.description),
        ));
    }
    std::fs::write(path, out)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn yaml_scalar(s: &str) -> String {
    // Quote when it contains characters that need escaping or starts with a special token.
    let needs_quote = s.is_empty()
        || s.contains(':')
        || s.contains('#')
        || s.contains('\n')
        || s.contains('"')
        || s.starts_with('-')
        || s.starts_with('?')
        || s.starts_with('*')
        || s.starts_with('&')
        || s.starts_with('!')
        || s.starts_with('|')
        || s.starts_with('>')
        || s.starts_with('@')
        || s.starts_with('`')
        || s.starts_with('\'')
        || s.starts_with(' ')
        || s.ends_with(' ');
    if needs_quote {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        s.to_string()
    }
}

pub fn derive_scenarios(spec: &OpenAPI) -> Vec<ScenarioEntry> {
    let mut out: Vec<ScenarioEntry> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();

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
            for (field, _) in required_body_fields(op) {
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

fn required_body_fields(op: &Operation) -> Vec<(String, ())> {
    let Some(body_ref) = &op.request_body else { return Vec::new() };
    let body: &RequestBody = match body_ref {
        ReferenceOr::Item(b) => b,
        ReferenceOr::Reference { .. } => return Vec::new(),
    };
    let mut fields: BTreeSet<String> = BTreeSet::new();
    for (_ct, mt) in &body.content {
        let Some(schema_ref) = &mt.schema else { continue };
        let schema = match schema_ref {
            ReferenceOr::Item(s) => s,
            ReferenceOr::Reference { .. } => continue,
        };
        collect_required_fields(schema, &mut fields);
    }
    fields.into_iter().map(|f| (f, ())).collect()
}

fn collect_required_fields(schema: &Schema, out: &mut BTreeSet<String>) {
    match &schema.schema_kind {
        SchemaKind::Type(Type::Object(obj)) => {
            for r in &obj.required {
                out.insert(r.clone());
            }
        }
        SchemaKind::OneOf { one_of } | SchemaKind::AnyOf { any_of: one_of } => {
            for branch in one_of {
                if let ReferenceOr::Item(s) = branch {
                    collect_required_fields(s, out);
                }
            }
        }
        SchemaKind::AllOf { all_of } => {
            for branch in all_of {
                if let ReferenceOr::Item(s) = branch {
                    collect_required_fields(s, out);
                }
            }
        }
        _ => {}
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
