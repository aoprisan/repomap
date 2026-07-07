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
    /// Name of the synthetic catch-all appended by `new` when no service
    /// roots the repo (`path = "."`). The indexer drops it again if no file
    /// ends up assigned to it.
    synthetic_root: Option<String>,
}

impl Resolver {
    pub fn new(mut services: Vec<Service>) -> Self {
        // Guarantee a catch-all: without a service rooted at ".", a file
        // outside every declared path would be misattributed (or, with an
        // empty manifest, crash resolve). Pick a name that can't collide
        // with a declared service.
        let synthetic_root = if services.iter().any(|s| s.path == "." || s.path.is_empty()) {
            None
        } else {
            let mut name = "root".to_string();
            while services.iter().any(|s| s.name == name) {
                name.insert(0, '_');
            }
            services.push(Service {
                name: name.clone(),
                path: ".".into(),
                stack: None,
                purpose: None,
                entrypoints: Vec::new(),
                deps: Vec::new(),
            });
            Some(name)
        };
        // Longest path first so nested services win the prefix match.
        services.sort_by_key(|s| std::cmp::Reverse(s.path.len()));
        Resolver { services, synthetic_root }
    }

    /// The synthetic catch-all's name, if one was appended.
    pub fn synthetic_root(&self) -> Option<&str> {
        self.synthetic_root.as_deref()
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
            if s.path == "." || s.path.is_empty() || rel == s.path
                || rel.starts_with(&format!("{}/", s.path))
            {
                return s;
            }
        }
        // Unreachable: `new` guarantees a service rooted at "." exists, and
        // that one matches every path in the loop above.
        unreachable!("Resolver::new always installs a catch-all service")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, path: &str) -> Service {
        Service {
            name: name.into(),
            path: path.into(),
            stack: None,
            purpose: None,
            entrypoints: Vec::new(),
            deps: Vec::new(),
        }
    }

    #[test]
    fn resolve_survives_an_empty_service_list() {
        // An empty manifest (repomap.toml with no [[service]]) used to panic.
        let r = Resolver::new(Vec::new());
        assert_eq!(r.resolve("a.rs").name, "root");
        assert_eq!(r.synthetic_root(), Some("root"));
    }

    #[test]
    fn uncovered_files_land_in_the_synthetic_root_not_a_sibling_service() {
        let r = Resolver::new(vec![svc("svc", "svc")]);
        assert_eq!(r.resolve("svc/a.rs").name, "svc");
        assert_eq!(r.resolve("toplevel.rs").name, "root");
        assert_eq!(r.resolve("other/b.rs").name, "root");
    }

    #[test]
    fn longest_prefix_wins_and_declared_root_suppresses_the_synthetic() {
        let r = Resolver::new(vec![svc("outer", "a"), svc("inner", "a/b"), svc("all", ".")]);
        assert_eq!(r.resolve("a/b/x.rs").name, "inner");
        assert_eq!(r.resolve("a/x.rs").name, "outer");
        assert_eq!(r.resolve("elsewhere.rs").name, "all");
        assert_eq!(r.synthetic_root(), None);
    }

    #[test]
    fn synthetic_root_name_dodges_a_declared_root_service() {
        let r = Resolver::new(vec![svc("root", "svc")]);
        assert_eq!(r.resolve("outside.rs").name, "_root");
        assert_eq!(r.resolve("svc/a.rs").name, "root");
    }
}
