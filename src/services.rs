//! Service model: read from `repomap.toml` at the repo root if present,
//! otherwise infer one service per top-level directory.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

#[derive(Clone)]
pub struct Service {
    pub name: String,
    /// Repo-relative directory that roots the service.
    pub path: String,
    pub stack: Option<String>,
    pub purpose: Option<String>,
    pub entrypoints: Vec<String>,
    pub deps: Vec<String>,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    service: Vec<ManifestService>,
}

#[derive(Deserialize)]
struct ManifestService {
    name: String,
    path: String,
    #[serde(default)]
    stack: Option<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    entrypoints: Vec<String>,
    #[serde(default)]
    deps: Vec<String>,
}

/// Manifest-defined services, or `None` to signal "infer from layout".
pub fn from_manifest(root: &Path) -> Result<Option<Vec<Service>>> {
    let manifest_path = root.join("repomap.toml");
    if !manifest_path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&manifest_path)?;
    let m: Manifest = toml::from_str(&text)?;
    let services = m
        .service
        .into_iter()
        .map(|s| Service {
            name: s.name,
            path: s.path,
            stack: s.stack,
            purpose: s.purpose,
            entrypoints: s.entrypoints,
            deps: s.deps,
        })
        .collect();
    Ok(Some(services))
}

/// Resolver mapping a repo-relative file path to its owning service.
/// Longest matching `path` prefix wins; a synthetic "root" service catches
/// files that sit above every declared/inferred service dir.
pub struct Resolver {
    services: Vec<Service>,
}

impl Resolver {
    pub fn new(mut services: Vec<Service>) -> Self {
        // Longest path first so nested services win the prefix match.
        services.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
        Resolver { services }
    }

    /// Build a resolver by inferring services from the top-level dirs that
    /// actually contain indexable files. `tops` are (dir_name) seen on disk;
    /// `stacks` maps a dir to its dominant language for `stack`.
    pub fn infer(tops: &BTreeMap<String, String>) -> Self {
        let services = tops
            .iter()
            .map(|(dir, stack)| Service {
                name: dir.clone(),
                path: dir.clone(),
                stack: Some(stack.clone()),
                purpose: None,
                entrypoints: Vec::new(),
                deps: Vec::new(),
            })
            .collect();
        Resolver::new(services)
    }

    /// Return the owning service for a repo-relative path.
    pub fn resolve(&self, rel: &str) -> &Service {
        for s in &self.services {
            if s.path == "." || rel == s.path || rel.starts_with(&format!("{}/", s.path)) {
                return s;
            }
        }
        // Fallback: treat the first path component as the service.
        // Caller guarantees at least the synthetic root exists.
        &self.services[self.services.len() - 1]
    }

    pub fn all(&self) -> &[Service] {
        &self.services
    }
}

/// First path component of a repo-relative path (the inferred top-level dir).
pub fn top_dir(rel: &str) -> String {
    match rel.split_once('/') {
        Some((head, _)) => head.to_string(),
        None => ".".to_string(),
    }
}
