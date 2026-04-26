use anyhow::{Context, Result};
use clap::Parser;
use pulldown_cmark::{Event, HeadingLevel, Parser as MdParser, Tag, TagEnd};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
pub struct Args {
    /// Plugin name (e.g. `billing-contract` or bare `billing`).
    plugin_name: String,
}

#[derive(Serialize)]
struct Sdk {
    present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Serialize)]
struct Output {
    plugin_name: String,
    installed: bool,
    plugin_path: Option<String>,
    plugin_version: Option<String>,
    skill_name: Option<String>,
    contract_yaml_path: Option<String>,
    sdk: Option<Sdk>,
    repo_url: Option<String>,
    issues_url: Option<String>,
    warnings: Vec<String>,
}

pub fn run(args: Args) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("failed to read current dir")?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let out = compute(&cwd, home.as_deref(), &args.plugin_name)?;
    crate::envelope::print_ok(serde_json::to_value(&out)?)
}

fn compute(cwd: &Path, home: Option<&Path>, name: &str) -> Result<Output> {
    let candidates = candidate_paths(cwd, home, name);

    let mut hits: Vec<(String, PathBuf)> = Vec::new();
    for (resolved_name, path) in &candidates {
        if path.is_dir() || path.is_symlink() {
            // Resolve symlink target if symlink.
            let resolved = if path.is_symlink() {
                std::fs::canonicalize(path).unwrap_or_else(|_| path.clone())
            } else {
                path.clone()
            };
            hits.push((resolved_name.clone(), resolved));
        }
    }

    if hits.is_empty() {
        return Ok(Output {
            plugin_name: name.to_string(),
            installed: false,
            plugin_path: None,
            plugin_version: None,
            skill_name: None,
            contract_yaml_path: None,
            sdk: None,
            repo_url: None,
            issues_url: None,
            warnings: Vec::new(),
        });
    }

    // First hit wins; project-level (cwd-rooted) entries come first in the candidate order.
    let (resolved_name, plugin_path) = hits[0].clone();
    let mut warnings: Vec<String> = Vec::new();

    if hits.len() > 1 {
        // Surface duplicates (project + user level).
        let dupes: Vec<String> = hits
            .iter()
            .skip(1)
            .map(|(_, p)| display_path(cwd, p))
            .collect();
        warnings.push(format!(
            "duplicate plugin install dirs found; using first: also at {}",
            dupes.join(", ")
        ));
    }

    // Read plugin.json
    let plugin_json_path = plugin_path.join(".claude-plugin").join("plugin.json");
    let mut plugin_version: Option<String> = None;
    let mut skill_name: Option<String> = None;
    let mut repo_url: Option<String> = None;
    let mut issues_url: Option<String> = None;
    if plugin_json_path.is_file() {
        let raw = std::fs::read_to_string(&plugin_json_path)
            .with_context(|| format!("failed to read {}", plugin_json_path.display()))?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("invalid JSON in {}", plugin_json_path.display()))?;
        plugin_version = v.get("version").and_then(|x| x.as_str()).map(String::from);
        if plugin_version.is_none() {
            warnings.push("plugin.json missing 'version'".into());
        }
        repo_url = v.get("repository").and_then(extract_url_field);
        issues_url = v
            .get("bugs")
            .and_then(extract_url_field)
            .or_else(|| repo_url.as_deref().and_then(derive_github_issues_url));
        // skill name: prefer first entry of skills[]; fall back to bare service name guess.
        if let Some(skills) = v.get("skills").and_then(|x| x.as_array()) {
            if let Some(first) = skills.first() {
                // skills entries may be strings ("billing") or objects ({"name":"billing", ...}).
                let s = first
                    .as_str()
                    .map(String::from)
                    .or_else(|| {
                        first
                            .get("name")
                            .and_then(|x| x.as_str())
                            .map(String::from)
                    });
                skill_name = s;
            }
        }
    } else {
        warnings.push("plugin.json missing".into());
    }

    // If skill_name still unknown, look at skills/ subdirs.
    if skill_name.is_none() {
        let skills_dir = plugin_path.join("skills");
        if let Ok(rd) = std::fs::read_dir(&skills_dir) {
            let mut subs: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            subs.sort();
            skill_name = subs.into_iter().next();
        }
    }

    // SKILL.md → SDK block.
    let mut sdk: Option<Sdk> = None;
    if let Some(sk) = &skill_name {
        let skill_md = plugin_path.join("skills").join(sk).join("SKILL.md");
        if skill_md.is_file() {
            let text = std::fs::read_to_string(&skill_md)
                .with_context(|| format!("failed to read {}", skill_md.display()))?;
            sdk = Some(parse_sdk_block(&text, &mut warnings));
        }
    }

    // contract.yaml
    let contract_yaml = plugin_path.join("reference").join("contract.yaml");
    let contract_yaml_path = if contract_yaml.is_file() {
        Some(display_path(cwd, &contract_yaml))
    } else {
        warnings.push("reference/contract.yaml missing".into());
        None
    };

    Ok(Output {
        plugin_name: resolved_name,
        installed: true,
        plugin_path: Some(display_path(cwd, &plugin_path)),
        plugin_version,
        skill_name,
        contract_yaml_path,
        sdk,
        repo_url,
        issues_url,
        warnings,
    })
}

/// Extract a URL from a JSON field that may be either a bare string
/// (`"https://github.com/foo/bar"`) or an object with a `url` field
/// (`{"type": "git", "url": "git+https://github.com/foo/bar.git"}`).
/// Strips `git+` prefix and `.git` suffix to yield a canonical browsable URL.
fn extract_url_field(v: &serde_json::Value) -> Option<String> {
    let raw = v
        .as_str()
        .map(String::from)
        .or_else(|| v.get("url").and_then(|x| x.as_str()).map(String::from))?;
    Some(normalize_repo_url(&raw))
}

fn normalize_repo_url(s: &str) -> String {
    let s = s.strip_prefix("git+").unwrap_or(s);
    let s = s.strip_suffix(".git").unwrap_or(s);
    s.to_string()
}

/// Best-effort issues URL derivation for GitHub-hosted repos.
/// Returns None for non-GitHub hosts; consumers should display `repo_url` instead.
fn derive_github_issues_url(repo: &str) -> Option<String> {
    if repo.contains("github.com") {
        Some(format!("{}/issues", repo))
    } else {
        None
    }
}

fn candidate_paths(cwd: &Path, home: Option<&Path>, name: &str) -> Vec<(String, PathBuf)> {
    let mut bases: Vec<PathBuf> = vec![cwd.join(".claude").join("plugins")];
    if let Some(h) = home {
        bases.push(h.join(".claude").join("plugins"));
    }
    let mut names: Vec<String> = vec![name.to_string()];
    if !name.ends_with("-contract") {
        names.push(format!("{}-contract", name));
    }
    let mut out = Vec::new();
    for base in &bases {
        for n in &names {
            out.push((n.clone(), base.join(n)));
        }
    }
    out
}

fn display_path(cwd: &Path, p: &Path) -> String {
    p.strip_prefix(cwd)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

/// Parse the `## SDK` heading and extract groupId/artifactId/version
/// from the first `<dependency>...</dependency>` block underneath.
fn parse_sdk_block(md: &str, warnings: &mut Vec<String>) -> Sdk {
    // First locate the `## SDK` heading text-line position so we can take the
    // raw substring after it (until the next ## heading).
    let parser = MdParser::new(md).into_offset_iter();
    let mut sdk_start: Option<usize> = None;
    let mut sdk_end: Option<usize> = None;
    let mut events_iter = parser.peekable();
    while let Some((ev, range)) = events_iter.next() {
        if let Event::Start(Tag::Heading { level, .. }) = ev {
            if level == HeadingLevel::H2 {
                let mut text = String::new();
                let mut heading_range_end = range.end;
                while let Some((iev, irange)) = events_iter.next() {
                    heading_range_end = irange.end;
                    match iev {
                        Event::End(TagEnd::Heading(_)) => break,
                        Event::Text(t) | Event::Code(t) => text.push_str(&t),
                        _ => {}
                    }
                }
                if sdk_start.is_some() && sdk_end.is_none() {
                    sdk_end = Some(range.start);
                    break;
                }
                if text.trim().eq_ignore_ascii_case("SDK") && sdk_start.is_none() {
                    sdk_start = Some(heading_range_end);
                }
            }
        }
    }
    let Some(start) = sdk_start else {
        return Sdk { present: false, group_id: None, artifact_id: None, version: None };
    };
    let end = sdk_end.unwrap_or(md.len());
    let block = &md[start..end];

    let dep_start = match block.find("<dependency>") {
        Some(i) => i,
        None => {
            warnings.push("SKILL.md has SDK heading but no <dependency> block".into());
            return Sdk { present: false, group_id: None, artifact_id: None, version: None };
        }
    };
    let dep_end = block[dep_start..].find("</dependency>").map(|i| dep_start + i);
    let dep_block = match dep_end {
        Some(e) => &block[dep_start..e],
        None => &block[dep_start..],
    };

    let group_id = extract_xml_tag(dep_block, "groupId");
    let artifact_id = extract_xml_tag(dep_block, "artifactId");
    let version = extract_xml_tag(dep_block, "version");

    Sdk {
        present: group_id.is_some() && artifact_id.is_some(),
        group_id,
        artifact_id,
        version,
    }
}

fn extract_xml_tag(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let i = s.find(&open)? + open.len();
    let j = s[i..].find(&close)? + i;
    Some(s[i..j].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_block_parsed() {
        let md = "# Foo\n\n## Overview\n\nstuff\n\n## SDK\n\n```xml\n<dependency>\n  <groupId>com.example</groupId>\n  <artifactId>billing-api</artifactId>\n  <version>1.2.0</version>\n</dependency>\n```\n\n## Endpoints\n\n";
        let mut w = Vec::new();
        let sdk = parse_sdk_block(md, &mut w);
        assert!(sdk.present);
        assert_eq!(sdk.group_id.unwrap(), "com.example");
        assert_eq!(sdk.artifact_id.unwrap(), "billing-api");
        assert_eq!(sdk.version.unwrap(), "1.2.0");
        assert!(w.is_empty());
    }

    #[test]
    fn sdk_heading_no_block() {
        let md = "## SDK\n\nNothing here yet.\n";
        let mut w = Vec::new();
        let sdk = parse_sdk_block(md, &mut w);
        assert!(!sdk.present);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("no <dependency> block"));
    }

    #[test]
    fn no_sdk_heading() {
        let md = "## Other\n\nstuff\n";
        let mut w = Vec::new();
        let sdk = parse_sdk_block(md, &mut w);
        assert!(!sdk.present);
        assert!(w.is_empty());
    }
}
