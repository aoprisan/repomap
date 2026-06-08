//! Indexing: walk the repo, detect language/service, write symbols + raw
//! edges per file (incremental by git blob hash), then resolve edges by name.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use walkdir::WalkDir;

use crate::git;
use crate::lang::Language;
use crate::services::{self, Resolver, Service};

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".repomap"];

struct Candidate {
    rel: String,
    lang: Language,
}

pub struct Summary {
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub files_removed: usize,
    pub symbols: usize,
    pub edges: usize,
    pub services: usize,
}

pub fn run(conn: &mut Connection, root: &Path, incremental: bool, db_file: &Path) -> Result<Summary> {
    let candidates = scan(root, db_file);
    let resolver = build_services(root, &candidates)?;
    write_services(conn, &resolver)?;

    if !incremental {
        // Full reindex: drop everything derived from files (cascades to
        // symbols/edges). Services were just rewritten above.
        conn.execute("DELETE FROM files", [])?;
    }

    let now = epoch_secs();
    let mut seen: HashSet<String> = HashSet::new();
    let mut indexed = 0usize;
    let mut skipped = 0usize;

    let tx = conn.transaction()?;
    for c in &candidates {
        seen.insert(c.rel.clone());
        let abs = root.join(&c.rel);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let hash = git::blob_hash(&bytes);

        if incremental {
            let unchanged: bool = tx
                .query_row(
                    "SELECT git_hash = ?1 FROM files WHERE path = ?2",
                    rusqlite::params![hash, c.rel],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if unchanged {
                skipped += 1;
                continue;
            }
            // Changed file: clear its prior rows (cascades symbols/edges).
            tx.execute("DELETE FROM files WHERE path = ?1", [&c.rel])?;
        }

        let src = String::from_utf8_lossy(&bytes);
        let service = resolver.resolve(&c.rel);
        index_file(&tx, &c.rel, c.lang, service, &src, &hash, now)?;
        indexed += 1;
    }

    // Purge files that vanished from disk (incremental run only; full reindex
    // already wiped and rewrote everything).
    let mut removed = 0usize;
    if incremental {
        let stale: Vec<String> = {
            let mut stmt = tx.prepare("SELECT path FROM files")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok())
                .filter(|p| !seen.contains(p))
                .collect()
        };
        for p in &stale {
            tx.execute("DELETE FROM files WHERE path = ?1", [p])?;
            removed += 1;
        }
    }

    resolve_edges(&tx)?;
    tx.commit()?;

    let symbols: i64 = conn.query_row("SELECT count(*) FROM symbols", [], |r| r.get(0))?;
    let edges: i64 = conn.query_row("SELECT count(*) FROM edges", [], |r| r.get(0))?;
    Ok(Summary {
        files_indexed: indexed,
        files_skipped: skipped,
        files_removed: removed,
        symbols: symbols as usize,
        edges: edges as usize,
        services: resolver.all().len(),
    })
}

/// Walk the repo collecting indexable files (repo-relative paths).
fn scan(root: &Path, db_file: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    let walker = WalkDir::new(root).into_iter().filter_entry(|e| {
        if e.file_type().is_dir() {
            let name = e.file_name().to_string_lossy();
            !SKIP_DIRS.contains(&name.as_ref())
        } else {
            true
        }
    });
    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path == db_file {
            continue;
        }
        if let Some(lang) = Language::from_path(path) {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(Candidate {
                    rel: rel.to_string_lossy().replace('\\', "/"),
                    lang,
                });
            }
        }
    }
    out
}

/// Manifest services if present, else infer one per top-level dir using the
/// dominant language of the files found under it.
fn build_services(root: &Path, candidates: &[Candidate]) -> Result<Resolver> {
    if let Some(services) = services::from_manifest(root)? {
        return Ok(Resolver::new(services));
    }
    // Tally languages per top-level dir to pick a stack.
    let mut counts: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
    for c in candidates {
        let top = services::top_dir(&c.rel);
        *counts
            .entry(top)
            .or_default()
            .entry(c.lang.name())
            .or_insert(0) += 1;
    }
    let mut tops: BTreeMap<String, String> = BTreeMap::new();
    for (dir, langs) in counts {
        let stack = langs
            .into_iter()
            .max_by_key(|(_, n)| *n)
            .map(|(l, _)| l.to_string())
            .unwrap_or_default();
        tops.insert(dir, stack);
    }
    Ok(Resolver::infer(&tops))
}

fn write_services(conn: &Connection, resolver: &Resolver) -> Result<()> {
    conn.execute("DELETE FROM services", [])?;
    for s in resolver.all() {
        conn.execute(
            "INSERT OR REPLACE INTO services(name, path, stack, purpose, entrypoints, deps)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                s.name,
                s.path,
                s.stack,
                s.purpose,
                json_list(&s.entrypoints),
                json_list(&s.deps),
            ],
        )?;
    }
    Ok(())
}

fn index_file(
    tx: &rusqlite::Transaction,
    rel: &str,
    lang: Language,
    service: &Service,
    src: &str,
    hash: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO files(path, service, language, loc, git_hash, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![rel, service.name, lang.name(), git::loc(src) as i64, hash, now],
    )?;

    let extracted = lang.extract(src)?;

    // Insert symbols, remembering (id, start, end) for enclosing resolution.
    let mut spans: Vec<(i64, usize, usize)> = Vec::with_capacity(extracted.symbols.len());
    {
        let mut stmt = tx.prepare(
            "INSERT INTO symbols(name, kind, file, start_line, end_line, signature,
                                 doc_first_line, service, language)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for s in &extracted.symbols {
            stmt.execute(rusqlite::params![
                s.name,
                s.kind,
                rel,
                s.start_line as i64,
                s.end_line as i64,
                s.signature,
                s.doc_first_line,
                service.name,
                lang.name(),
            ])?;
            spans.push((tx.last_insert_rowid(), s.start_line, s.end_line));
        }
    }

    // Attribute each edge to its innermost enclosing symbol.
    {
        let mut stmt =
            tx.prepare("INSERT INTO edge_raw(src_symbol, dst_name, kind) VALUES (?1, ?2, ?3)")?;
        for e in &extracted.edges {
            if let Some(src_id) = enclosing(&spans, e.src_line) {
                stmt.execute(rusqlite::params![src_id, e.dst_name, e.kind])?;
            }
        }
    }
    Ok(())
}

/// Innermost symbol whose span contains `line`.
fn enclosing(spans: &[(i64, usize, usize)], line: usize) -> Option<i64> {
    spans
        .iter()
        .filter(|(_, s, e)| *s <= line && line <= *e)
        .min_by_key(|(_, s, e)| e - s)
        .map(|(id, _, _)| *id)
}

/// Rebuild `edges` from `edge_raw`, resolving each dst name to a single
/// symbol id (same-service preferred, then lowest id). Best-effort.
fn resolve_edges(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute("DELETE FROM edges", [])?;
    tx.execute(
        "INSERT INTO edges(src_symbol, dst_symbol, kind)
         SELECT er.src_symbol, d.id, er.kind
         FROM edge_raw er
         JOIN symbols d ON d.id = (
             SELECT s.id FROM symbols s
             WHERE s.name = er.dst_name
             ORDER BY (s.service = (SELECT service FROM symbols WHERE id = er.src_symbol)) DESC,
                      s.id
             LIMIT 1
         )",
        [],
    )?;
    Ok(())
}

fn json_list(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", s.replace('"', "\\\"")))
        .collect();
    format!("[{}]", quoted.join(","))
}

fn epoch_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
