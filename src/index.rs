//! Indexing: walk the repo (honoring .gitignore), detect language/service,
//! parse + extract in parallel, write symbols + raw edges per file
//! (incremental by git blob hash), then resolve edges by name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rusqlite::Connection;

use crate::git;
use crate::lang::{Extracted, Language};
use crate::services::{self, Resolver, Service};

/// Always-skipped directories, even when not gitignored (e.g. a checkout
/// without a .gitignore, or one that commits its lockfile but not its rules).
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".repomap",
    // Python virtualenvs & caches — vendored deps, not project code.
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
];

/// Files larger than this are skipped: hand-written source essentially never
/// reaches 1 MiB, while generated bundles and minified JS routinely do — and
/// they'd pollute every `find` with noise.
const MAX_FILE_SIZE: u64 = 1024 * 1024;

struct Candidate {
    rel: String,
    lang: Language,
}

#[derive(Debug)]
pub struct Summary {
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub files_removed: usize,
    pub symbols: usize,
    pub edges: usize,
    pub services: usize,
    /// The mode the run actually executed in (an incremental request is
    /// upgraded to full when service definitions changed).
    pub mode: &'static str,
}

pub fn run(
    conn: &mut Connection,
    root: &Path,
    incremental: bool,
    db_file: &Path,
) -> Result<Summary> {
    let root = canonical_root(root)?;
    check_root_binding(conn, &root)?;
    let candidates = scan(&root, db_file)?;
    let resolver = build_services(&root, &candidates)?;

    // Incremental runs skip unchanged files — but a file's stored service
    // comes from index time, so a changed repomap.toml (or changed inferred
    // layout) would leave stale attribution on every skipped file and
    // mis-scope edge resolution. Detect that via a fingerprint of the
    // resolved services and upgrade to a full reindex.
    let fingerprint = services_fingerprint(&resolver);
    let mut incremental = incremental;
    let mut mode = if incremental { "incremental" } else { "full" };
    if incremental {
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'services_fingerprint'",
                [],
                |r| r.get(0),
            )
            .ok();
        if stored.as_deref() != Some(fingerprint.as_str()) {
            incremental = false;
            mode = "full: service definitions changed";
        }
    }

    let now = epoch_secs();
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let seen: HashSet<&str> = candidates.iter().map(|c| c.rel.as_str()).collect();

    // Snapshot the stored per-file stats up front so change detection and
    // extraction can run off the database thread.
    let stored: HashMap<String, StoredFile> = if incremental {
        let mut stmt = conn.prepare("SELECT path, git_hash, mtime, size FROM files")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                StoredFile {
                    hash: r.get(1)?,
                    mtime: r.get(2)?,
                    size: r.get(3)?,
                },
            ))
        })?;
        rows.filter_map(|r| r.ok()).collect()
    } else {
        HashMap::new()
    };

    // Determinate bar over the scanned candidates. indicatif draws to stderr
    // and hides itself when stderr is not a TTY, so piped/scripted runs stay
    // clean and only the final summary line (stdout) survives.
    let bar = if crate::output::is_jsonl() {
        ProgressBar::hidden()
    } else {
        ProgressBar::new(candidates.len() as u64)
    };
    bar.set_style(
        ProgressStyle::with_template("{spinner} [{bar:30}] {pos}/{len} files {wide_msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    // Read + hash + parse + extract in parallel (parsing dominates a full
    // index and is embarrassingly parallel). Collected in candidate order so
    // symbol ids stay deterministic; only the writer below touches SQLite.
    let outcomes: Vec<Outcome> = candidates
        .par_iter()
        .map(|c| {
            let out = examine(&root, c, stored.get(&c.rel));
            bar.inc(1);
            bar.set_message(c.rel.clone());
            out
        })
        .collect();

    let tx = conn.transaction()?;
    // All derived-index mutations belong to the same transaction. A failed
    // extraction, service write, or graph rebuild therefore leaves the
    // previously usable index intact instead of exposing a half-rebuilt one.
    write_services(&tx, &resolver)?;
    if !incremental {
        tx.execute("DELETE FROM files", [])?;
    }
    for (c, outcome) in candidates.iter().zip(outcomes) {
        match outcome {
            Outcome::Unchanged => skipped += 1,
            // Same content, moved stat (e.g. touch, checkout): refresh the
            // stat so the next run takes the fast path again.
            Outcome::Touched { mtime, size } => {
                tx.execute(
                    "UPDATE files SET mtime = ?1, size = ?2 WHERE path = ?3",
                    rusqlite::params![mtime, size, c.rel],
                )?;
                skipped += 1;
            }
            Outcome::Unreadable(error) => {
                return Err(error).with_context(|| format!("reading source file '{}'", c.rel));
            }
            Outcome::Index {
                existed,
                loc,
                hash,
                mtime,
                size,
                extracted,
            } => {
                if existed {
                    // Clear the file's prior rows (cascades symbols/edges).
                    tx.execute("DELETE FROM files WHERE path = ?1", [&c.rel])?;
                }
                let service = resolver.resolve(&c.rel);
                write_file(
                    &tx,
                    &c.rel,
                    c.lang,
                    service,
                    loc,
                    &extracted?,
                    &hash,
                    mtime,
                    size,
                    now,
                )?;
                indexed += 1;
            }
        }
    }

    // Purge files that vanished from disk (incremental run only; full reindex
    // already wiped and rewrote everything).
    let mut removed = 0usize;
    if incremental {
        let stale: Vec<String> = {
            let mut stmt = tx.prepare("SELECT path FROM files")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok())
                .filter(|p| !seen.contains(p.as_str()))
                .collect()
        };
        for p in &stale {
            tx.execute("DELETE FROM files WHERE path = ?1", [p])?;
            removed += 1;
        }
    }

    // Unchanged incremental runs preserve both tables. Query commands invoke
    // refresh before every read, so rebuilding the entire graph here would
    // turn even a no-op query into up to 50 edge scans plus one write per
    // symbol.
    if !incremental || indexed > 0 || removed > 0 {
        bar.set_message("resolving edges…");
        resolve_edges(&tx)?;
        crate::graph::compute_ranks(&tx)?;
    }

    // The synthetic catch-all only earns a row if a file actually landed in
    // it; otherwise drop it so `map` shows only real services.
    if let Some(name) = resolver.synthetic_root() {
        tx.execute(
            "DELETE FROM services
             WHERE name = ?1
               AND NOT EXISTS (SELECT 1 FROM files WHERE service = ?1)",
            [name],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('services_fingerprint', ?1)",
        [&fingerprint],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('repository_root', ?1)",
        [root.to_string_lossy().as_ref()],
    )?;
    tx.commit()?;
    bar.finish_and_clear();

    let symbols: i64 = conn.query_row("SELECT count(*) FROM symbols", [], |r| r.get(0))?;
    let edges: i64 = conn.query_row("SELECT count(*) FROM edges", [], |r| r.get(0))?;
    let services: i64 = conn.query_row("SELECT count(*) FROM services", [], |r| r.get(0))?;
    Ok(Summary {
        files_indexed: indexed,
        files_skipped: skipped,
        files_removed: removed,
        symbols: symbols as usize,
        edges: edges as usize,
        services: services as usize,
        mode,
    })
}

/// Stable digest of the resolved service definitions; a mismatch with the
/// stored value means file→service attribution may be stale.
fn services_fingerprint(resolver: &Resolver) -> String {
    let mut buf = String::new();
    for s in resolver.all() {
        buf.push_str(&format!(
            "{}\x1f{}\x1f{:?}\x1f{:?}\x1f{:?}\x1f{:?}\n",
            s.name, s.path, s.stack, s.purpose, s.entrypoints, s.deps
        ));
    }
    git::blob_hash(buf.as_bytes())
}

/// Walk the repo collecting indexable files (repo-relative paths).
///
/// The walk honors `.gitignore`/`.ignore` rules (`require_git(false)` so an
/// exported tree without `.git` behaves the same as the checkout it came
/// from), but not the machine-local global gitignore — what gets indexed must
/// not depend on whose machine ran it. `SKIP_DIRS` stays as a fallback for
/// dependency/cache directories in repos with no ignore rules, and oversized
/// files (generated bundles, minified JS) are dropped. Entries are sorted so
/// symbol-id assignment — and therefore edge-resolution tie-breaks — is
/// deterministic across runs.
fn scan(root: &Path, db_file: &Path) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false) // dot-dirs may hold real code; SKIP_DIRS covers the noisy ones
        .require_git(false)
        .git_global(false)
        .follow_links(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|e| {
            if e.file_type().is_some_and(|t| t.is_dir()) {
                let name = e.file_name().to_string_lossy();
                !SKIP_DIRS.contains(&name.as_ref())
            } else {
                true
            }
        });
    for entry in builder.build() {
        let entry = entry.with_context(|| format!("walking repository '{}'", root.display()))?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if path == db_file {
            continue;
        }
        if entry
            .metadata()
            .map(|m| m.len() > MAX_FILE_SIZE)
            .unwrap_or(false)
        {
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
    Ok(out)
}

/// Resolve and validate a repository root before any database mutation.
/// Canonicalization also gives the database a stable identity across `.` / symlink
/// spellings of the same checkout.
pub fn canonical_root(root: &Path) -> Result<PathBuf> {
    if !root.exists() {
        bail!("repository root '{}' does not exist", root.display());
    }
    if !root.is_dir() {
        bail!("repository root '{}' is not a directory", root.display());
    }
    std::fs::canonicalize(root)
        .with_context(|| format!("canonicalizing repository root '{}'", root.display()))
}

/// Reject accidental reuse of one database for another checkout. Older
/// databases have no binding and are adopted on their next successful index.
pub fn check_root_binding(conn: &Connection, root: &Path) -> Result<()> {
    let Some(stored) = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'repository_root'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
    else {
        return Ok(());
    };
    let current = canonical_root(root)?;
    if stored != current.to_string_lossy() {
        bail!(
            "index belongs to repository '{}', not '{}' (choose the matching --db or clear it)",
            stored,
            current.display()
        );
    }
    Ok(())
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

/// The stored change-detection stats for one indexed file.
struct StoredFile {
    hash: String,
    mtime: i64,
    size: i64,
}

/// What the parallel examine pass decided for one candidate; the serial
/// writer turns these into database rows.
enum Outcome {
    /// Untouched stat — skipped without even reading the file.
    Unchanged,
    /// Stat moved but content hash matched: just refresh the stored stat.
    Touched { mtime: i64, size: i64 },
    /// The file was scanned but could not be read. The run fails closed so an
    /// incremental query never silently retains stale rows for it.
    Unreadable(std::io::Error),
    /// Parse + extraction ran; `existed` means prior rows must be cleared.
    Index {
        existed: bool,
        loc: i64,
        hash: String,
        mtime: i64,
        size: i64,
        extracted: anyhow::Result<Extracted>,
    },
}

/// Change-detect one candidate and, when it changed (or is new), parse and
/// extract it. Pure with respect to the database — safe to run in parallel.
fn examine(root: &Path, c: &Candidate, stored: Option<&StoredFile>) -> Outcome {
    let abs = root.join(&c.rel);
    let (mtime, size) = stat(&abs);

    if let Some(f) = stored {
        // Fast path: an untouched stat means an unchanged file — skip
        // without even reading it.
        if mtime == f.mtime && size == f.size && mtime != 0 {
            return Outcome::Unchanged;
        }
    }
    let bytes = match std::fs::read(&abs) {
        Ok(b) => b,
        Err(error) => return Outcome::Unreadable(error),
    };
    let hash = git::blob_hash(&bytes);
    if let Some(f) = stored {
        // Stat moved (e.g. touch, checkout): confirm via content hash.
        if hash == f.hash {
            return Outcome::Touched { mtime, size };
        }
    }
    let src = String::from_utf8_lossy(&bytes);
    Outcome::Index {
        existed: stored.is_some(),
        loc: git::loc(&src) as i64,
        hash,
        mtime,
        size,
        extracted: c.lang.extract(&src),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_file(
    tx: &rusqlite::Transaction,
    rel: &str,
    lang: Language,
    service: &Service,
    loc: i64,
    extracted: &Extracted,
    hash: &str,
    mtime: i64,
    size: i64,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO files(path, service, language, loc, git_hash, mtime, size, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![rel, service.name, lang.name(), loc, hash, mtime, size, now],
    )?;

    // Establish lexical identities before insertion. These survive database
    // row-id churn and let edge resolution prefer the source's actual owner.
    let parents = symbol_parents(&extracted.symbols);
    let qualified: Vec<String> = (0..extracted.symbols.len())
        .map(|i| qualified_name(&extracted.symbols, &parents, i))
        .collect();
    let mut spans: Vec<IndexedSpan> = Vec::with_capacity(extracted.symbols.len());
    {
        let mut stmt = tx.prepare(
            "INSERT INTO symbols(name, kind, file, start_line, end_line, signature,
                                 doc_first_line, container, qualified_name,
                                 service, language, is_test)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for (i, s) in extracted.symbols.iter().enumerate() {
            let container = parents[i].map(|p| extracted.symbols[p].name.as_str());
            stmt.execute(rusqlite::params![
                s.name,
                s.kind,
                rel,
                s.start_line as i64,
                s.end_line as i64,
                s.signature,
                s.doc_first_line,
                container,
                qualified[i],
                service.name,
                lang.name(),
                crate::lang::is_test_symbol(rel, s),
            ])?;
            spans.push(IndexedSpan {
                id: tx.last_insert_rowid(),
                start: s.start_line,
                end: s.end_line,
                kind: s.kind.clone(),
            });
        }
    }

    // Attribute each edge to its innermost enclosing symbol. Imports are also
    // recorded per *file* (even top-level ones with no enclosing symbol):
    // they license cross-service resolution for that name from this file.
    {
        let mut stmt = tx.prepare(
            "INSERT INTO edge_raw(src_symbol, dst_name, qualifier, kind)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut imp =
            tx.prepare("INSERT OR IGNORE INTO file_imports(file, name) VALUES (?1, ?2)")?;
        for e in &extracted.edges {
            if e.kind == "import" {
                imp.execute(rusqlite::params![rel, e.dst_name])?;
            }
            if let Some(src_id) = edge_owner(&spans, e.src_line, &e.kind) {
                stmt.execute(rusqlite::params![src_id, e.dst_name, e.qualifier, e.kind])?;
            }
        }
    }
    Ok(())
}

struct IndexedSpan {
    id: i64,
    start: usize,
    end: usize,
    kind: String,
}

fn symbol_parents(symbols: &[crate::lang::RawSymbol]) -> Vec<Option<usize>> {
    symbols
        .iter()
        .enumerate()
        .map(|(i, child)| {
            symbols
                .iter()
                .enumerate()
                .filter(|(j, parent)| {
                    *j != i
                        && parent.start_line <= child.start_line
                        && parent.end_line >= child.end_line
                        && (parent.start_line < child.start_line
                            || parent.end_line > child.end_line)
                })
                .min_by_key(|(_, parent)| parent.end_line - parent.start_line)
                .map(|(j, _)| j)
        })
        .collect()
}

fn qualified_name(
    symbols: &[crate::lang::RawSymbol],
    parents: &[Option<usize>],
    mut index: usize,
) -> String {
    let mut parts = vec![symbols[index].name.as_str()];
    while let Some(parent) = parents[index] {
        parts.push(symbols[parent].name.as_str());
        index = parent;
    }
    parts.reverse();
    parts.join("::")
}

fn callable_kind(kind: &str) -> bool {
    matches!(kind, "fn" | "def" | "method" | "function")
}

/// Calls belong to the innermost callable, not a local `val`/`const` whose
/// initializer happens to contain them. Non-call edges retain the old
/// innermost-definition ownership needed by extends/import relationships.
fn edge_owner(spans: &[IndexedSpan], line: usize, edge_kind: &str) -> Option<i64> {
    let containing = || {
        spans
            .iter()
            .filter(move |s| s.start <= line && line <= s.end)
    };
    if edge_kind == "call" {
        if let Some(owner) = containing()
            .filter(|s| callable_kind(&s.kind))
            .min_by_key(|s| s.end - s.start)
        {
            return Some(owner.id);
        }
    }
    containing().min_by_key(|s| s.end - s.start).map(|s| s.id)
}

/// Innermost symbol whose span contains `line`.
#[cfg(test)]
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
/// * A bare name resolves within the **same service** as the source. Bare
///   names are ambiguous across a monorepo — a Rust `map.get(..)` and a Scala
///   `repo.get(..)` both surface a `get` reference, and there is no reliable
///   way to link those across services by name alone.
/// * A name the source **file imports** may additionally resolve across
///   service boundaries: the import is explicit evidence the reference points
///   outside, so `use billing::TaxCalculator` lets a `TaxCalculator` call
///   land on billing's definition. Same-service definitions still win the tie.
/// * A reference that is neither defined in the source's service nor imported
///   is dropped rather than guessed — so we never cross-link `get`/`apply`-
///   style common names to unrelated symbols in other services.
/// * A bare call first resolves to a sibling in the same lexical container.
///   Otherwise it resolves only when the same-file or same-service candidate
///   is unique; ambiguous names are dropped instead of guessed.
/// * A qualified call resolves only to a definition owned by that qualifier
///   (`TaxCalculator.withTax` -> `TaxCalculator::withTax`). Instance receivers
///   without a matching indexed container stay unresolved.
/// * **Self-edges are excluded** so a symbol is never its own caller (e.g. a
///   recursive call, or a method whose body references its own name).
///
/// The preference order is expressed as a COALESCE of three short-circuiting
/// tiers (same file → same service → imported, any service) instead of one
/// `ORDER BY` over the whole `OR`-ed candidate set. Each tier is an exact
/// index-range probe whose entries are already in id order, so `ORDER BY id
/// LIMIT 1` reads a row or two — where the old single query walked (and
/// sorted) *every* same-named symbol per reference, which degenerated badly
/// on repos where the same name repeats across many files.
fn resolve_edges(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute("DELETE FROM edges", [])?;
    tx.execute(
        "INSERT INTO edges(src_symbol, dst_symbol, kind)
         SELECT src_symbol, dst, kind FROM (
             SELECT er.src_symbol AS src_symbol, er.kind AS kind,
                    CASE WHEN er.qualifier IS NOT NULL THEN
                        COALESCE(
                            (SELECT s.id FROM symbols s
                              WHERE s.name = er.dst_name
                                AND s.container = er.qualifier
                                AND s.file = src.file
                                AND s.id <> er.src_symbol
                              ORDER BY s.id LIMIT 1),
                            (SELECT s.id FROM symbols s
                              WHERE s.name = er.dst_name
                                AND s.container = er.qualifier
                                AND s.service = src.service
                                AND s.id <> er.src_symbol
                              ORDER BY s.id LIMIT 1),
                            (CASE WHEN EXISTS (SELECT 1 FROM file_imports fi
                                                WHERE fi.file = src.file
                                                  AND fi.name = er.qualifier)
                                  THEN (SELECT s.id FROM symbols s
                                         WHERE s.name = er.dst_name
                                           AND s.container = er.qualifier
                                           AND s.id <> er.src_symbol
                                         ORDER BY s.id LIMIT 1)
                             END)
                        )
                    ELSE COALESCE(
                        -- A sibling definition in the same lexical owner.
                        (SELECT s.id FROM symbols s
                          WHERE s.file = src.file AND s.name = er.dst_name
                            AND s.container IS src.container
                            AND s.id <> er.src_symbol
                          ORDER BY s.id LIMIT 1),
                        -- Same file, but only when unambiguous.
                        (SELECT CASE WHEN count(*) = 1 THEN min(s.id) END
                           FROM symbols s
                          WHERE s.file = src.file AND s.name = er.dst_name
                            AND s.id <> er.src_symbol),
                        -- Same service, but only when unambiguous.
                        (SELECT CASE WHEN count(*) = 1 THEN min(s.id) END
                           FROM symbols s
                          WHERE s.name = er.dst_name AND s.service = src.service
                            AND s.id <> er.src_symbol),
                        -- Imported names may cross services when unique.
                        (CASE WHEN EXISTS (SELECT 1 FROM file_imports fi
                                            WHERE fi.file = src.file
                                              AND fi.name = er.dst_name)
                              THEN (SELECT CASE WHEN count(*) = 1 THEN min(s.id) END
                                      FROM symbols s
                                     WHERE s.name = er.dst_name
                                       AND s.id <> er.src_symbol)
                         END)
                    ) END AS dst
             FROM edge_raw er
             JOIN symbols src ON src.id = er.src_symbol
         )
         WHERE dst IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// mtime (ns since epoch) + size for the incremental fast path; (0, 0) when
/// the stat fails, which never matches a stored row and forces the hash path.
fn stat(path: &Path) -> (i64, i64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            (mtime, m.len() as i64)
        }
        Err(_) => (0, 0),
    }
}

/// Bring the index up to date before answering a query: a full index when
/// nothing is indexed yet, an incremental one otherwise. Reports to stderr
/// only when something actually changed, so fresh-index queries stay silent.
pub fn refresh(conn: &mut Connection, root: &Path, db_file: &Path) -> Result<()> {
    let files: i64 = conn.query_row("SELECT count(*) FROM files", [], |r| r.get(0))?;
    let s = run(conn, root, files > 0, db_file)?;
    if s.files_indexed > 0 || s.files_removed > 0 {
        crate::output::note(
            "index_refreshed",
            format!(
                "index refreshed: {} files reindexed, {} removed [{}]",
                s.files_indexed, s.files_removed, s.mode
            ),
        );
    }
    Ok(())
}

fn json_list(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", s.replace('"', "\\\"")))
        .collect();
    format!("[{}]", quoted.join(","))
}

pub(crate) fn epoch_secs() -> i64 {
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
            (
                "svc/a.rs",
                "pub fn caller() { target(); }\npub fn target() {}\n",
            ),
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
    fn recursive_call_falls_back_to_another_same_name_definition() {
        // Self is excluded per tier, not just once: with no other same-file
        // candidate, the recursive reference resolves to the same-service
        // definition in the other file.
        let (conn, _dir) = index_repo(&[
            ("svc/a.rs", "pub fn fib() { fib(); }\n"),
            ("svc/b.rs", "pub fn fib() {}\n"),
        ]);
        assert_eq!(dst_file_of(&conn, "fib").as_deref(), Some("svc/b.rs"));
    }

    #[test]
    fn incremental_skips_unchanged_then_picks_up_edits() {
        let dir = build_repo(&[("svc/a.rs", "pub fn f() {}\n")]);
        let (mut conn, db_path) = open_db(&dir);

        let s1 = run(&mut conn, dir.path(), false, &db_path).unwrap();
        assert_eq!(s1.files_indexed, 1);

        // A no-change refresh must not rewrite the graph or its ranks.
        conn.execute("UPDATE symbols SET rank = 0.123", []).unwrap();

        let s2 = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s2.files_indexed, 0);
        assert_eq!(s2.files_skipped, 1);
        let preserved: f64 = conn
            .query_row("SELECT rank FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(preserved, 0.123);

        std::fs::write(
            dir.path().join("svc/a.rs"),
            "pub fn f() {}\npub fn g() {}\n",
        )
        .unwrap();
        let s3 = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s3.files_indexed, 1);
        assert_eq!(s3.files_skipped, 0);
        assert!(symbol_exists(&conn, "g"));
        let stale: i64 = conn
            .query_row("SELECT count(*) FROM symbols WHERE rank = 0.123", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stale, 0, "a changed graph must recompute ranks");
    }

    fn service_of(conn: &Connection, symbol: &str) -> String {
        conn.query_row(
            "SELECT service FROM symbols WHERE name = ?1",
            [symbol],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn service_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM services ORDER BY name")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    #[test]
    fn empty_manifest_indexes_into_a_synthetic_root() {
        // A repomap.toml with no [[service]] used to panic in Resolver::resolve.
        let (conn, _dir) = index_repo(&[
            ("repomap.toml", "# no services declared\n"),
            ("a.rs", "pub fn f() {}\n"),
        ]);
        assert_eq!(service_of(&conn, "f"), "root");
        assert_eq!(service_names(&conn), vec!["root"]);
    }

    #[test]
    fn manifest_gap_files_land_in_root_not_a_sibling_service() {
        let (conn, _dir) = index_repo(&[
            (
                "repomap.toml",
                "[[service]]\nname = \"svc\"\npath = \"svc\"\n",
            ),
            ("svc/a.rs", "pub fn covered() {}\n"),
            ("toplevel.rs", "pub fn orphan() {}\n"),
        ]);
        assert_eq!(service_of(&conn, "covered"), "svc");
        assert_eq!(service_of(&conn, "orphan"), "root");
        assert_eq!(service_names(&conn), vec!["root", "svc"]);
    }

    #[test]
    fn unused_synthetic_root_is_dropped_from_services() {
        let (conn, _dir) = index_repo(&[
            (
                "repomap.toml",
                "[[service]]\nname = \"svc\"\npath = \"svc\"\n",
            ),
            ("svc/a.rs", "pub fn covered() {}\n"),
        ]);
        assert_eq!(service_names(&conn), vec!["svc"]);
    }

    #[test]
    fn incremental_goes_full_when_service_definitions_change() {
        // Two inferred services; `f` calls `g` across them, so no edge yet.
        let dir = build_repo(&[
            ("app/a.rs", "pub fn f() { g(); }\n"),
            ("lib/b.rs", "pub fn g() {}\n"),
        ]);
        let (mut conn, db_path) = open_db(&dir);
        run(&mut conn, dir.path(), false, &db_path).unwrap();
        assert_eq!(service_of(&conn, "f"), "app");
        assert!(!edge_exists(&conn, "f", "g"));

        // Merge everything into one manifest service. An incremental run must
        // notice, reindex fully, reattribute, and re-scope edge resolution.
        std::fs::write(
            dir.path().join("repomap.toml"),
            "[[service]]\nname = \"everything\"\npath = \".\"\n",
        )
        .unwrap();
        let s = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s.mode, "full: service definitions changed");
        assert_eq!(
            s.files_skipped, 0,
            "no file may be skipped with stale services"
        );
        assert_eq!(service_of(&conn, "f"), "everything");
        assert!(
            edge_exists(&conn, "f", "g"),
            "same-service call now resolves"
        );

        // With the manifest unchanged, incremental stays incremental.
        let s2 = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s2.mode, "incremental");
        assert!(s2.files_skipped > 0);
    }

    #[test]
    fn import_licenses_a_cross_service_edge() {
        // Same layout as drops_cross_service_references, but svca explicitly
        // imports `helper` — that evidence lets the edge cross the boundary.
        let (conn, _dir) = index_repo(&[
            (
                "svca/a.rs",
                "use svcb::helper;\npub fn caller() { helper(); }\n",
            ),
            ("svcb/b.rs", "pub fn helper() {}\n"),
        ]);
        assert!(
            edge_exists(&conn, "caller", "helper"),
            "an imported name must resolve across services"
        );
        assert_eq!(dst_file_of(&conn, "caller").as_deref(), Some("svcb/b.rs"));
    }

    #[test]
    fn same_service_definition_beats_an_imported_one() {
        // `helper` exists both in the caller's own service and (imported) in
        // another; the local definition must win.
        let (conn, _dir) = index_repo(&[
            (
                "svca/a.rs",
                "use svcb::helper;\npub fn caller() { helper(); }\npub fn helper() {}\n",
            ),
            ("svcb/b.rs", "pub fn helper() {}\n"),
        ]);
        assert_eq!(dst_file_of(&conn, "caller").as_deref(), Some("svca/a.rs"));
    }

    #[test]
    fn unimported_names_still_do_not_cross_services() {
        // The conservative default survives: importing one name does not open
        // the door for every other bare reference in the file.
        let (conn, _dir) = index_repo(&[
            (
                "svca/a.rs",
                "use svcb::other;\npub fn caller() { helper(); }\n",
            ),
            ("svcb/b.rs", "pub fn helper() {}\npub fn other() {}\n"),
        ]);
        assert!(!edge_exists(&conn, "caller", "helper"));
    }

    #[test]
    fn refresh_indexes_an_empty_db_then_picks_up_edits() {
        let dir = build_repo(&[("svc/a.rs", "pub fn f() {}\n")]);
        let (mut conn, db_path) = open_db(&dir);

        // Nothing indexed yet: refresh runs a full index.
        refresh(&mut conn, dir.path(), &db_path).unwrap();
        assert!(symbol_exists(&conn, "f"));

        // Edit a file: the next refresh (incremental) sees it.
        std::fs::write(
            dir.path().join("svc/a.rs"),
            "pub fn f() {}\npub fn g() {}\n",
        )
        .unwrap();
        refresh(&mut conn, dir.path(), &db_path).unwrap();
        assert!(symbol_exists(&conn, "g"));

        // Delete it: refresh purges its symbols.
        std::fs::remove_file(dir.path().join("svc/a.rs")).unwrap();
        refresh(&mut conn, dir.path(), &db_path).unwrap();
        assert!(!symbol_exists(&conn, "f"));
    }

    #[test]
    fn stat_fast_path_skips_without_reading_but_survives_a_touch() {
        let dir = build_repo(&[("svc/a.rs", "pub fn f() {}\n")]);
        let (mut conn, db_path) = open_db(&dir);
        run(&mut conn, dir.path(), false, &db_path).unwrap();

        // Unchanged: skipped (via the stat fast path when mtimes are stable).
        let s = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!((s.files_indexed, s.files_skipped), (0, 1));

        // Same content, forced-new mtime: the hash fallback still skips it.
        let path = dir.path().join("svc/a.rs");
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(future)
            .unwrap();
        let s = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!((s.files_indexed, s.files_skipped), (0, 1));
    }

    #[test]
    fn gitignored_files_are_not_indexed() {
        let (conn, _dir) = index_repo(&[
            (".gitignore", "gen/\nskipme.rs\n"),
            ("svc/a.rs", "pub fn kept() {}\n"),
            ("svc/skipme.rs", "pub fn ignored_by_name() {}\n"),
            ("gen/b.rs", "pub fn generated() {}\n"),
        ]);
        assert!(symbol_exists(&conn, "kept"));
        assert!(
            !symbol_exists(&conn, "generated"),
            "gitignored dir is skipped"
        );
        assert!(
            !symbol_exists(&conn, "ignored_by_name"),
            "gitignored file is skipped"
        );
    }

    #[test]
    fn newly_gitignored_files_are_purged_on_the_next_run() {
        // The ignored path sits *inside* a service that keeps other files, so
        // the inferred service set — and thus the incremental mode — survives.
        let dir = build_repo(&[
            ("svc/a.rs", "pub fn kept() {}\n"),
            ("svc/gen/b.rs", "pub fn generated() {}\n"),
        ]);
        let (mut conn, db_path) = open_db(&dir);
        run(&mut conn, dir.path(), false, &db_path).unwrap();
        assert!(symbol_exists(&conn, "generated"));

        std::fs::write(dir.path().join(".gitignore"), "svc/gen/\n").unwrap();
        let s = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s.mode, "incremental");
        assert_eq!(s.files_removed, 1);
        assert!(
            !symbol_exists(&conn, "generated"),
            "now-ignored file's symbols are gone"
        );
        assert!(symbol_exists(&conn, "kept"));
    }

    #[test]
    fn skip_dirs_still_apply_without_any_ignore_rules() {
        let (conn, _dir) = index_repo(&[
            ("svc/a.rs", "pub fn kept() {}\n"),
            ("node_modules/dep/x.ts", "export function vendored() {}\n"),
        ]);
        assert!(symbol_exists(&conn, "kept"));
        assert!(!symbol_exists(&conn, "vendored"));
    }

    #[test]
    fn oversized_files_are_skipped() {
        let mut big = String::from("pub fn huge() {}\n");
        big.push_str(&"// padding padding padding\n".repeat(50_000)); // > 1 MiB
        let (conn, _dir) = index_repo(&[
            ("svc/a.rs", "pub fn small() {}\n"),
            ("svc/big.rs", big.as_str()),
        ]);
        assert!(symbol_exists(&conn, "small"));
        assert!(
            !symbol_exists(&conn, "huge"),
            "files over the size cap are not indexed"
        );
    }

    #[test]
    fn incremental_purges_deleted_files() {
        let dir = build_repo(&[
            ("svc/a.rs", "pub fn f() {}\n"),
            ("svc/b.rs", "pub fn g() {}\n"),
        ]);
        let (mut conn, db_path) = open_db(&dir);
        run(&mut conn, dir.path(), false, &db_path).unwrap();
        assert!(symbol_exists(&conn, "g"));

        std::fs::remove_file(dir.path().join("svc/b.rs")).unwrap();
        let s = run(&mut conn, dir.path(), true, &db_path).unwrap();
        assert_eq!(s.files_removed, 1);
        assert!(
            !symbol_exists(&conn, "g"),
            "deleted file's symbols are gone"
        );
    }

    #[test]
    fn invalid_root_cannot_erase_an_existing_index() {
        let dir = build_repo(&[("svc/a.rs", "pub fn kept() {}\n")]);
        let (mut conn, db_path) = open_db(&dir);
        run(&mut conn, dir.path(), false, &db_path).unwrap();

        let missing = dir.path().join("does-not-exist");
        let error = run(&mut conn, &missing, false, &db_path).unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        assert!(symbol_exists(&conn, "kept"));
    }

    #[test]
    fn database_is_bound_to_one_canonical_repository() {
        let first = build_repo(&[("svc/a.rs", "pub fn first() {}\n")]);
        let second = build_repo(&[("svc/b.rs", "pub fn second() {}\n")]);
        let (mut conn, db_path) = open_db(&first);
        run(&mut conn, first.path(), false, &db_path).unwrap();

        let error = run(&mut conn, second.path(), false, &db_path).unwrap_err();
        assert!(error.to_string().contains("index belongs to repository"));
        assert!(symbol_exists(&conn, "first"));
        assert!(!symbol_exists(&conn, "second"));
    }

    #[test]
    fn failed_full_rebuild_rolls_back_the_previous_index() {
        let dir = build_repo(&[("svc/a.rs", "pub fn kept() {}\n")]);
        let (mut conn, db_path) = open_db(&dir);
        run(&mut conn, dir.path(), false, &db_path).unwrap();
        conn.execute_batch(
            "CREATE TEMP TRIGGER reject_service_delete
             BEFORE DELETE ON services BEGIN
               SELECT RAISE(ABORT, 'injected rebuild failure');
             END;",
        )
        .unwrap();

        assert!(run(&mut conn, dir.path(), false, &db_path).is_err());
        assert!(symbol_exists(&conn, "kept"));
        assert_eq!(service_names(&conn), vec!["svc"]);
    }

    #[test]
    fn qualified_calls_and_callable_ownership_avoid_false_get_edges() {
        let (conn, _dir) = index_repo(&[(
            "src/Invoice.scala",
            "trait Repository[A] {\n  def get(id: String): Option[A]\n}\n\
             object TaxCalculator {\n  def withTax(n: Long): Long = n\n}\n\
             object InvoiceService extends Repository[String] {\n\
               val store = scala.collection.mutable.Map.empty[String, String]\n\
               def get(id: String): Option[String] = store.get(id)\n\
               def total(id: String): Long = {\n\
                 val base = get(id).map(_.length.toLong).getOrElse(0L)\n\
                 TaxCalculator.withTax(base)\n\
               }\n\
             }\n",
        )]);

        let mut stmt = conn
            .prepare(
                "SELECT src.qualified_name, dst.qualified_name
                 FROM edges e
                 JOIN symbols src ON src.id = e.src_symbol
                 JOIN symbols dst ON dst.id = e.dst_symbol
                 ORDER BY 1, 2",
            )
            .unwrap();
        let edges: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            edges,
            vec![
                ("InvoiceService::total".into(), "InvoiceService::get".into()),
                (
                    "InvoiceService::total".into(),
                    "TaxCalculator::withTax".into()
                ),
            ]
        );
        assert!(!edges
            .iter()
            .any(|(src, dst)| { src == "InvoiceService::get" && dst == "Repository::get" }));
    }
}
