//! Load/save for the top-level `docs/test-scenarios.yaml`.
//!
//! Mirrors the jkit-cli `scenarios_yaml` module — kept in sync by convention
//! since the two crates live in separate repos. The schema is owned by the
//! formal-docs redesign spec.
//!
//! ```yaml
//! domains:
//!   billing:                    # single-API-type domain — flat list
//!     - endpoint: ...
//!       id: ...
//!       description: ...
//!   payment:                    # multi-API-type domain — keyed by api-type
//!     web-api:
//!       - ...
//!     microservice-api:
//!       - ...
//! ```

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const FILE_RELATIVE: &str = "docs/test-scenarios.yaml";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenarioEntry {
    pub endpoint: String,
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlugSection {
    Flat(Vec<ScenarioEntry>),
    PerApiType(Vec<(String, Vec<ScenarioEntry>)>),
}

impl SlugSection {
    /// Yield each (api_type, entry). For Flat, api_type is None.
    pub fn iter_with_type(&self) -> Box<dyn Iterator<Item = (Option<&str>, &ScenarioEntry)> + '_> {
        match self {
            SlugSection::Flat(v) => Box::new(v.iter().map(|e| (None, e))),
            SlugSection::PerApiType(buckets) => Box::new(
                buckets
                    .iter()
                    .flat_map(|(ty, v)| v.iter().map(move |e| (Some(ty.as_str()), e))),
            ),
        }
    }
}

pub struct ScenariosFile {
    pub path: PathBuf,
    top: Mapping,
}

impl ScenariosFile {
    pub fn load(project_root: &Path) -> Result<Self> {
        let path = project_root.join(FILE_RELATIVE);
        if !path.exists() {
            return Ok(Self {
                path,
                top: Mapping::new(),
            });
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let parsed: Value = if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&text)
                .with_context(|| format!("parsing {} as YAML", path.display()))?
        };
        let top = match parsed {
            Value::Null => Mapping::new(),
            Value::Mapping(m) => m,
            _ => return Err(anyhow!(
                "{}: expected a YAML mapping at the top level",
                path.display()
            )),
        };
        Ok(Self { path, top })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let s = serde_yaml::to_string(&Value::Mapping(self.top.clone()))
            .context("serializing docs/test-scenarios.yaml")?;
        fs::write(&self.path, s.as_bytes())
            .with_context(|| format!("writing {}", self.path.display()))
    }

    fn domains_map(&self) -> Mapping {
        match self.top.get(Value::String("domains".into())) {
            Some(Value::Mapping(m)) => m.clone(),
            _ => Mapping::new(),
        }
    }

    fn put_domains_map(&mut self, m: Mapping) {
        self.top
            .insert(Value::String("domains".into()), Value::Mapping(m));
    }

    #[allow(dead_code)]
    pub fn slugs(&self) -> Vec<String> {
        self.domains_map()
            .into_iter()
            .filter_map(|(k, _)| k.as_str().map(str::to_string))
            .collect()
    }

    pub fn section(&self, slug: &str) -> Result<Option<SlugSection>> {
        let m = self.domains_map();
        let raw = match m.get(Value::String(slug.into())) {
            Some(v) => v.clone(),
            None => return Ok(None),
        };
        match raw {
            Value::Null => Ok(Some(SlugSection::Flat(Vec::new()))),
            Value::Sequence(seq) => Ok(Some(SlugSection::Flat(parse_entries(&seq, slug, None)?))),
            Value::Mapping(buckets) => {
                let mut out = Vec::new();
                for (k, v) in buckets {
                    let ty = k
                        .as_str()
                        .ok_or_else(|| anyhow!(
                            "{}: domains.{slug} has a non-string api-type key",
                            FILE_RELATIVE
                        ))?
                        .to_string();
                    let seq = match v {
                        Value::Sequence(s) => s,
                        Value::Null => Vec::new(),
                        _ => return Err(anyhow!(
                            "{}: domains.{slug}.{ty} must be a list of scenario entries",
                            FILE_RELATIVE
                        )),
                    };
                    let entries = parse_entries(&seq, slug, Some(&ty))?;
                    out.push((ty, entries));
                }
                Ok(Some(SlugSection::PerApiType(out)))
            }
            _ => Err(anyhow!(
                "{}: domains.{slug} must be either a list (single api-type) or mapping",
                FILE_RELATIVE
            )),
        }
    }

    /// Replace the entries for (slug, api_type). When `api_type=None`, writes
    /// a flat list at `domains.<slug>`. When `api_type=Some(t)`, sets
    /// `domains.<slug>.<t>` (creating the parent mapping if absent).
    /// Refuses to convert a flat-with-entries section to per-api-type.
    pub fn put_entries(
        &mut self,
        slug: &str,
        api_type: Option<&str>,
        entries: &[ScenarioEntry],
    ) -> Result<()> {
        let mut domains = self.domains_map();
        let key = Value::String(slug.into());
        let new_value = match (api_type, domains.get(&key).cloned()) {
            (None, _) => Value::Sequence(entries.iter().map(entry_to_value).collect()),
            (Some(ty), existing) => {
                let mut buckets = match existing {
                    Some(Value::Mapping(m)) => m,
                    Some(Value::Sequence(seq)) if seq.is_empty() => Mapping::new(),
                    None | Some(Value::Null) => Mapping::new(),
                    Some(Value::Sequence(_)) => return Err(anyhow!(
                        "domains.{slug} is currently a flat list with entries; \
cannot append api-type-keyed entries without an explicit layout migration"
                    )),
                    _ => return Err(anyhow!("domains.{slug} is malformed; cannot update")),
                };
                buckets.insert(
                    Value::String(ty.into()),
                    Value::Sequence(entries.iter().map(entry_to_value).collect()),
                );
                Value::Mapping(buckets)
            }
        };
        domains.insert(key, new_value);
        self.put_domains_map(domains);
        Ok(())
    }
}

fn parse_entries(
    seq: &[Value],
    slug: &str,
    api_type: Option<&str>,
) -> Result<Vec<ScenarioEntry>> {
    let mut out = Vec::with_capacity(seq.len());
    for (i, v) in seq.iter().enumerate() {
        let entry: ScenarioEntry = serde_yaml::from_value(v.clone()).with_context(|| {
            let where_ = match api_type {
                Some(t) => format!("domains.{slug}.{t}[{i}]"),
                None => format!("domains.{slug}[{i}]"),
            };
            format!("{}: malformed entry at {where_}", FILE_RELATIVE)
        })?;
        out.push(entry);
    }
    Ok(out)
}

fn entry_to_value(e: &ScenarioEntry) -> Value {
    let mut m = Mapping::new();
    m.insert(
        Value::String("endpoint".into()),
        Value::String(e.endpoint.clone()),
    );
    m.insert(Value::String("id".into()), Value::String(e.id.clone()));
    m.insert(
        Value::String("description".into()),
        Value::String(e.description.clone()),
    );
    Value::Mapping(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_flat_section() {
        let tmp = tempdir().unwrap();
        let mut f = ScenariosFile::load(tmp.path()).unwrap();
        f.put_entries(
            "billing",
            None,
            &[ScenarioEntry {
                endpoint: "GET /a".into(),
                id: "happy-path".into(),
                description: "ok".into(),
            }],
        )
        .unwrap();
        f.save().unwrap();
        let f2 = ScenariosFile::load(tmp.path()).unwrap();
        match f2.section("billing").unwrap().unwrap() {
            SlugSection::Flat(v) => assert_eq!(v.len(), 1),
            _ => panic!(),
        }
    }

    #[test]
    fn round_trip_per_api_type_section() {
        let tmp = tempdir().unwrap();
        let mut f = ScenariosFile::load(tmp.path()).unwrap();
        f.put_entries(
            "payment",
            Some("web-api"),
            &[ScenarioEntry {
                endpoint: "POST /w".into(),
                id: "a".into(),
                description: "ok".into(),
            }],
        )
        .unwrap();
        f.put_entries(
            "payment",
            Some("microservice-api"),
            &[ScenarioEntry {
                endpoint: "POST /m".into(),
                id: "b".into(),
                description: "ok".into(),
            }],
        )
        .unwrap();
        f.save().unwrap();
        let f2 = ScenariosFile::load(tmp.path()).unwrap();
        match f2.section("payment").unwrap().unwrap() {
            SlugSection::PerApiType(buckets) => assert_eq!(buckets.len(), 2),
            _ => panic!(),
        }
    }
}
