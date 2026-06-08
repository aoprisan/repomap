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
/// symbol id. Best-effort, deliberately conservative:
///
/// * Resolution is scoped to the **same service** as the source. Bare names
///   are ambiguous across a monorepo — a Rust `map.get(..)` and a Scala
///   `repo.get(..)` both surface a `get` reference, and there is no reliable
///   way to link those across services by name alone. Rather than guess (and
///   cross-link `get`/`apply`/`new` to whatever unrelated symbol sorts first),
///   we drop a reference that has no same-service definition.
/// * Within a service, a definition in the **same file** wins, then lowest id.
/// * **Self-edges are excluded** so a symbol is never its own caller (e.g. a
///   recursive call, or a method whose body references its own name).
fn resolve_edges(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute("DELETE FROM edges", [])?;
    tx.execute(
        "INSERT INTO edges(src_symbol, dst_symbol, kind)
         SELECT er.src_symbol, d.id, er.kind
         FROM edge_raw er
         JOIN symbols src ON src.id = er.src_symbol
         JOIN symbols d ON d.id = (
             SELECT s.id FROM symbols s
             WHERE s.name = er.dst_name
               AND s.service = src.service
               AND s.id <> er.src_symbol
             ORDER BY (s.file = src.file) DESC, s.id
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::Connection;
    use std::path::PathBuf;

    #[test]
    fn enclosing_picks_the_innermost_span() {
        // (id, start, end): an outer fn (1), a nested block (3) inside it.
        let spans = vec![(1i64, 1usize, 100usize), (2, 10, 40), (3, 12, 15)];
        assert_eq!(enclosing(&spans, 13), Some(3)); // innermost wins
        assert_eq!(enclosing(&spans, 25), Some(2));
        assert_eq!(enclosing(&spans, 5), Some(1));
        assert_eq!(enclosing(&spans, 200), None); // outside every span
    }

    /// Materialize `files` (repo-relative path -> contents) under a temp dir.
    fn build_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
        }
        dir
    }

    fn open_db(dir: &tempfile::TempDir) -> (Connection, PathBuf) {
        let db_path = dir.path().join(".repomap.db");
        let conn = db::open(db_path.to_str().unwrap()).unwrap();
        (conn, db_path)
    }

    /// Full-index a fresh repo and hand back the open connection.
    fn index_repo(files: &[(&str, &str)]) -> (Connection, tempfile::TempDir) {
        let dir = build_repo(files);
        let (mut conn, db_path) = open_db(&dir);
        run(&mut conn, dir.path(), false, &db_path).unwrap();
        (conn, dir)
    }

    fn edge_exists(conn: &Connection, src_name: &str, dst_name: &str) -> bool {
        conn.query_row(
            "SELECT count(*) FROM edges e
             JOIN symbols s ON s.id = e.src_symbol
             JOIN symbols d ON d.id = e.dst_symbol
             WHERE s.name = ?1 AND d.name = ?2",
            rusqlite::params![src_name, dst_name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

    /// File of the symbol that the (single) edge out of `src_name` resolves to.
    fn dst_file_of(conn: &Connection, src_name: &str) -> Option<String> {
        conn.query_row(
            "SELECT d.file FROM edges e
             JOIN symbols s ON s.id = e.src_symbol
             JOIN symbols d ON d.id = e.dst_symbol
             WHERE s.name = ?1",
            [src_name],
            |r| r.get::<_, String>(0),
        )
        .ok()
    }

    fn symbol_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT count(*) FROM symbols WHERE name = ?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

    #[test]
    fn resolves_a_same_service_call() {
        let (conn, _dir) = index_repo(&[(
            "svc/a.rs",
            "pub fn caller() { helper(); }\npub fn helper() {}\n",
        )]);
        assert!(edge_exists(&conn, "caller", "helper"));
    }

    #[test]
    fn drops_cross_service_references() {
        // `caller` in svca calls `helper`, which is only defined in svcb.
        // The old resolver linked across services; the new one drops it.
        let (conn, _dir) = index_repo(&[
            ("svca/a.rs", "pub fn caller() { helper(); }\n"),
            ("svcb/b.rs", "pub fn helper() {}\n"),
        ]);
        assert!(symbol_exists(&conn, "helper"), "helper is still indexed");
        assert!(
            !edge_exists(&conn, "caller", "helper"),
            "a bare-name reference must not cross service boundaries"
        );
    }

    #[test]
    fn same_file_definition_wins_within_a_service() {
        // `target` is defined in both files of the same service; the caller's
        // own file should win the tie.
        let (conn, _dir) = index_repo(&[
            ("svc/a.rs", "pub fn caller() { target(); }\npub fn target() {}\n"),
            ("svc/b.rs", "pub fn target() {}\n"),
        ]);
        assert_eq!(dst_file_of(&conn, "caller").as_deref(), Some("svc/a.rs"));
    }

    #[test]
    fn recursive_call_is_not_a_self_edge() {
        let (conn, _dir) = index_repo(&[("svc/a.rs", "pub fn fib() { fib(); }\n")]);
        assert!(
            !edge_exists(&conn, "fib", "fib"),
            "a symbol must not be its own caller"
        );
    }

    #[test]
    fn incremental_skips_unchanged_then_picks_up_edits() {
        let dir = build_repo(&[("svc/a.rs", "pub fn f() {}\n")]);
        let (mut conn, db_path) = open_db(&dir);

        let s1 = run(&mut conn, dir.path(), false, &db_path).unwrap();
        assert_eq!(s1.files_indexed, 1);

        let s2 = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s2.files_indexed, 0);
        assert_eq!(s2.files_skipped, 1);

        std::fs::write(dir.path().join("svc/a.rs"), "pub fn f() {}\npub fn g() {}\n").unwrap();
        let s3 = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s3.files_indexed, 1);
        assert_eq!(s3.files_skipped, 0);
        assert!(symbol_exists(&conn, "g"));
    }

    #[test]
    fn incremental_purges_deleted_files() {
        let dir = build_repo(&[("svc/a.rs", "pub fn f() {}\n"), ("svc/b.rs", "pub fn g() {}\n")]);
        let (mut conn, db_path) = open_db(&dir);
        run(&mut conn, dir.path(), false, &db_path).unwrap();
        assert!(symbol_exists(&conn, "g"));

        std::fs::remove_file(dir.path().join("svc/b.rs")).unwrap();
        let s = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s.files_removed, 1);
        assert!(!symbol_exists(&conn, "g"), "deleted file's symbols are gone");
    }
}
