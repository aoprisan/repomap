//! Query commands. Every result is ONE compact line: a pointer, never a body.

use anyhow::Result;
use rusqlite::Connection;

/// Filters for `find`.
pub struct FindOpts {
    pub service: Option<String>,
    pub kind: Option<String>,
    pub lang: Option<String>,
    pub k: usize,
}

/// One result line. `file` is the repo-relative path as indexed, so the
/// pointer is directly openable from the repo root.
struct Pointer {
    file: String,
    start_line: i64,
    signature: String,
    enclosing: Option<String>,
}

impl Pointer {
    fn line(&self, suffix: &str) -> String {
        let enc = self.enclosing.as_deref().unwrap_or("-");
        let sig = if self.signature.is_empty() {
            "-"
        } else {
            &self.signature
        };
        format!(
            "{}:L{}  {}  [{}]{}",
            self.file, self.start_line, sig, enc, suffix
        )
    }

    fn print(&self, suffix: &str) {
        println!("{}", self.line(suffix));
    }
}

// SQL fragment computing the innermost enclosing symbol name for the symbol
// aliased `s` (the alias used by every query that selects pointers directly).
const ENCLOSING_SQL: &str = "(SELECT p.name FROM symbols p
     WHERE p.file = s.file AND p.id <> s.id
       AND p.start_line <= s.start_line AND p.end_line >= s.end_line
     ORDER BY (p.end_line - p.start_line) ASC LIMIT 1)";

/// `ENCLOSING_SQL` for an arbitrary symbols alias (e.g. the edge dst in
/// `callees`, where the pointer row is aliased `d`).
fn enclosing_sql(alias: &str) -> String {
    ENCLOSING_SQL.replace("s.", &format!("{alias}."))
}

// SQL fragment for in-degree (incoming edges), shown by `rank`.
const INDEG_SQL: &str = "(SELECT count(*) FROM edges WHERE dst_symbol = s.id)";

fn row_to_pointer(
    file: String,
    start_line: i64,
    signature: Option<String>,
    enclosing: Option<String>,
) -> Pointer {
    Pointer {
        file,
        start_line,
        signature: signature.unwrap_or_default(),
        enclosing,
    }
}

/// Expand a generic `--kind` into the language-native kinds stored in the
/// index (Rust stores `fn`, Python/Scala `def`, TypeScript `function`, …).
/// Native kinds pass through unchanged via the identity entry.
fn kind_synonyms(kind: &str) -> Vec<&str> {
    let mut kinds = vec![kind];
    match kind {
        "function" => kinds.extend(["fn", "def", "method"]),
        "fn" | "def" | "method" => kinds.push("function"),
        "module" => kinds.push("mod"),
        "mod" => kinds.push("module"),
        _ => {}
    }
    kinds
}

pub fn find(conn: &Connection, query: &str, opts: &FindOpts) -> Result<usize> {
    let mut sql = format!(
        "SELECT s.file, s.start_line, s.signature,
                {ENCLOSING_SQL}
         FROM symbols_fts f
         JOIN symbols s ON s.id = f.rowid
         WHERE symbols_fts MATCH ?1"
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query(query))];
    if let Some(v) = &opts.service {
        params.push(Box::new(v.clone()));
        sql.push_str(&format!(" AND s.service = ?{}", params.len()));
    }
    if let Some(v) = &opts.kind {
        let mut placeholders = Vec::new();
        for k in kind_synonyms(v) {
            params.push(Box::new(k.to_string()));
            placeholders.push(format!("?{}", params.len()));
        }
        sql.push_str(&format!(" AND s.kind IN ({})", placeholders.join(", ")));
    }
    if let Some(v) = &opts.lang {
        params.push(Box::new(v.clone()));
        sql.push_str(&format!(" AND s.language = ?{}", params.len()));
    }
    // Ties in text relevance break toward graph importance (PageRank), so the
    // symbol the repo actually leans on surfaces before same-named helpers.
    sql.push_str(&format!(
        " ORDER BY bm25(symbols_fts), s.rank DESC LIMIT {}",
        opts.k
    ));

    let mut stmt = conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(pref.as_slice(), |r| {
        Ok(row_to_pointer(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    let mut count = 0;
    for p in rows {
        p?.print("");
        count += 1;
    }
    if count == 0 {
        eprintln!("no matches");
    }
    Ok(count)
}

pub fn def(conn: &Connection, symbol: &str) -> Result<usize> {
    let sql = format!(
        "SELECT s.file, s.start_line, s.signature,
                {ENCLOSING_SQL}
         FROM symbols s
         WHERE s.name = ?1
         ORDER BY s.rank DESC, s.file, s.start_line"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([symbol], |r| {
        Ok(row_to_pointer(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    let mut count = 0;
    for p in rows {
        p?.print("");
        count += 1;
    }
    if count == 0 {
        eprintln!("no definition for '{symbol}'");
    }
    Ok(count)
}

pub fn callers(conn: &Connection, symbol: &str) -> Result<usize> {
    // Callers = source symbols of edges whose dst is a symbol named `symbol`.
    let sql = format!(
        "SELECT s.file, s.start_line, s.signature,
                {ENCLOSING_SQL}, e.kind
         FROM edges e
         JOIN symbols d ON d.id = e.dst_symbol
         JOIN symbols s ON s.id = e.src_symbol
         WHERE d.name = ?1
         GROUP BY s.id, e.kind
         ORDER BY s.file, s.start_line"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([symbol], |r| {
        let kind: String = r.get(4)?;
        Ok((
            row_to_pointer(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?),
            kind,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (p, kind) = row?;
        p.print(&format!("  ({kind})"));
        count += 1;
    }
    if count == 0 {
        eprintln!("no callers for '{symbol}'");
    }
    Ok(count)
}

pub fn callees(conn: &Connection, symbol: &str) -> Result<usize> {
    // Callees = destination symbols of edges whose src is named `symbol`.
    let enclosing_d = enclosing_sql("d");
    let sql = format!(
        "SELECT d.file, d.start_line, d.signature,
                {enclosing_d}, e.kind
         FROM edges e
         JOIN symbols s ON s.id = e.src_symbol
         JOIN symbols d ON d.id = e.dst_symbol
         WHERE s.name = ?1
         GROUP BY d.id, e.kind
         ORDER BY d.file, d.start_line"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([symbol], |r| {
        let kind: String = r.get(4)?;
        Ok((
            row_to_pointer(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?),
            kind,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (p, kind) = row?;
        p.print(&format!("  ({kind})"));
        count += 1;
    }
    if count == 0 {
        eprintln!("no callees for '{symbol}'");
    }
    Ok(count)
}

/// Structurally most important symbols, by PageRank over the reference graph.
/// The score is normalized so the top symbol in scope is 100 — comparable
/// within one invocation, not across repos. Orientation: run this (optionally
/// per service) to learn what a codebase actually revolves around.
pub fn rank(conn: &Connection, service: Option<&str>, k: usize) -> Result<usize> {
    let mut sql = format!(
        "SELECT s.file, s.start_line, s.signature,
                {ENCLOSING_SQL}, s.rank, {INDEG_SQL}
         FROM symbols s"
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(v) = service {
        params.push(Box::new(v.to_string()));
        sql.push_str(" WHERE s.service = ?1");
    }
    sql.push_str(&format!(
        " ORDER BY s.rank DESC, s.file, s.start_line LIMIT {k}"
    ));

    let mut stmt = conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(pref.as_slice(), |r| {
        Ok((
            row_to_pointer(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?),
            r.get::<_, f64>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;
    let results: Vec<(Pointer, f64, i64)> = rows.filter_map(|r| r.ok()).collect();
    let max = results.iter().map(|(_, r, _)| *r).fold(0.0f64, f64::max);
    if results.is_empty() || max <= 0.0 {
        eprintln!("no ranked symbols (index empty, or no references resolved yet)");
        return Ok(0);
    }
    for (p, r, indeg) in &results {
        let score = r / max * 100.0;
        let callers = if *indeg == 1 {
            "1 caller".into()
        } else {
            format!("{indeg} callers")
        };
        p.print(&format!("  (score {score:.0}, {callers})"));
    }
    Ok(results.len())
}

/// Transitive blast radius of changing `symbol`: its callers, their callers,
/// and so on up to `depth` hops, most important first within each hop.
pub fn impact(conn: &Connection, symbol: &str, depth: usize, k: usize) -> Result<usize> {
    let reached = crate::graph::impact(conn, symbol, depth)?;
    if reached.is_empty() {
        let defined: i64 = conn.query_row(
            "SELECT count(*) FROM symbols WHERE name = ?1",
            [symbol],
            |r| r.get(0),
        )?;
        if defined == 0 {
            eprintln!("no definition for '{symbol}'");
        } else {
            eprintln!("no impact: nothing references '{symbol}'");
        }
        return Ok(0);
    }

    // Pull pointer rows for every reached symbol, then order by (depth, rank
    // desc): nearest and most load-bearing dependents first.
    let sql = format!(
        "SELECT s.file, s.start_line, s.signature, {ENCLOSING_SQL}, s.rank, s.service
         FROM symbols s WHERE s.id = ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows: Vec<(Pointer, usize, f64, String)> = Vec::with_capacity(reached.len());
    for r in &reached {
        let (p, rank, service) = stmt.query_row([r.id], |row| {
            Ok((
                row_to_pointer(row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?),
                row.get::<_, f64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        rows.push((p, r.depth, rank, service));
    }
    rows.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| {
                (a.0.file.as_str(), a.0.start_line).cmp(&(b.0.file.as_str(), b.0.start_line))
            })
    });

    let files: std::collections::HashSet<&str> =
        rows.iter().map(|(p, ..)| p.file.as_str()).collect();
    let services: std::collections::HashSet<&str> = rows.iter().map(|(.., s)| s.as_str()).collect();
    let total = rows.len();
    for (p, d, ..) in rows.iter().take(k) {
        p.print(&format!("  (depth {d})"));
    }
    if total > k {
        println!("… and {} more (raise -k)", total - k);
    }
    println!(
        "impact: {total} symbols in {} files across {} services (depth ≤ {depth})",
        files.len(),
        services.len()
    );
    Ok(total.min(k))
}

pub fn outline(conn: &Connection, file: &str) -> Result<usize> {
    // Exact repo-relative path first; fall back to a suffix match so
    // `outline Invoice.scala` works without spelling the full path.
    let base = format!(
        "SELECT s.file, s.start_line, s.signature,
                {ENCLOSING_SQL}
         FROM symbols s"
    );
    let exact = format!("{base} WHERE s.file = ?1 ORDER BY s.start_line");
    let suffix = format!("{base} WHERE s.file LIKE '%' || ?1 ORDER BY s.file, s.start_line");

    for sql in [&exact, &suffix] {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([file], |r| {
            Ok(row_to_pointer(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        let mut count = 0;
        for p in rows {
            p?.print("");
            count += 1;
        }
        if count > 0 {
            return Ok(count);
        }
    }
    eprintln!("no symbols for '{file}' (not indexed, or no definitions in it)");
    Ok(0)
}

pub fn map(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT sv.name, sv.stack, sv.entrypoints,
                (SELECT count(*) FROM files f WHERE f.service = sv.name)
         FROM services sv
         ORDER BY sv.name",
    )?;
    let rows = stmt.query_map([], |r| {
        let name: String = r.get(0)?;
        let stack: Option<String> = r.get(1)?;
        let entrypoints: Option<String> = r.get(2)?;
        let nfiles: i64 = r.get(3)?;
        Ok((name, stack, entrypoints, nfiles))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, stack, entrypoints, nfiles) = row?;
        let stack = stack.unwrap_or_else(|| "?".into());
        let entry =
            first_json_item(entrypoints.as_deref().unwrap_or("[]")).unwrap_or_else(|| "-".into());
        println!("{name}  ({stack})  {nfiles} files  {entry}");
        count += 1;
    }
    Ok(count)
}

/// Report the index database in use: its path, and — if it exists — size and
/// row counts. A diagnostic for "which db am I actually hitting?". Opens the
/// file read-only and never creates it, so it stays a pure inspection.
pub fn show_db(path: &str) -> Result<()> {
    let file = std::path::Path::new(path);
    if !file.exists() {
        println!("{path}  (not indexed yet — run `repomap index`)");
        return Ok(());
    }

    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let conn = Connection::open(path)?;
    let count = |table: &str| -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or(0)
    };
    let indexed_at: Option<i64> = conn
        .query_row("SELECT max(indexed_at) FROM files", [], |r| r.get(0))
        .ok()
        .flatten();

    println!("{path}");
    println!("  size      {} KiB", size / 1024);
    println!("  services  {}", count("services"));
    println!("  files     {}", count("files"));
    println!("  symbols   {}", count("symbols"));
    println!("  edges     {}", count("edges"));
    if let Some(summary) = crate::usage::summary_line(&conn) {
        println!("  usage     {summary}");
    }
    match indexed_at {
        Some(ts) => println!("  indexed   {ts} (epoch seconds)"),
        None => println!("  indexed   never"),
    }
    Ok(())
}

/// Delete the index database file (and SQLite's `-wal`/`-shm` sidecars) so the
/// next `index` run starts clean. A no-op if nothing was indexed yet.
pub fn clear_db(path: &str) -> Result<()> {
    let file = std::path::Path::new(path);
    if !file.exists() {
        println!("{path}  (nothing to clear)");
        return Ok(());
    }
    std::fs::remove_file(file)?;
    for ext in ["-wal", "-shm"] {
        let sidecar = format!("{path}{ext}");
        if std::path::Path::new(&sidecar).exists() {
            std::fs::remove_file(&sidecar)?;
        }
    }
    println!("{path}  (cleared)");
    Ok(())
}

/// Turn free text into a tolerant FTS5 prefix query: each bareword becomes a
/// prefix term so `find handle` matches `handleRequest`.
pub(crate) fn fts_query(q: &str) -> String {
    fts_terms(q).join(" ")
}

/// Like `fts_query`, but any-term (OR) semantics: made for task-shaped free
/// text ("edge resolution refresh") where demanding every word kills recall —
/// bm25 still ranks fuller matches first.
pub(crate) fn fts_query_any(q: &str) -> String {
    fts_terms(q).join(" OR ")
}

fn fts_terms(q: &str) -> Vec<String> {
    let terms: Vec<String> = q
        .split_whitespace()
        .map(|t| {
            let clean: String = t
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            clean
        })
        .filter(|t| !t.is_empty())
        // Quote each term so FTS5 boolean keywords (OR/AND/NOT/NEAR) are treated
        // as literal search words, not operators. Cleaned terms hold only
        // alphanumerics/underscore, so there are no embedded quotes to escape.
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if terms.is_empty() {
        // Fall back to a never-matching token rather than invalid syntax.
        vec!["\"\"".to_string()]
    } else {
        terms
    }
}

/// First element of a simple JSON string array like `["Main.scala", ...]`.
fn first_json_item(s: &str) -> Option<String> {
    let inner = s.trim().strip_prefix('[')?.strip_suffix(']')?;
    let first = inner.split(',').next()?.trim();
    let unquoted = first.trim_matches('"');
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_line_is_the_openable_repo_relative_path() {
        // The path must be the file as indexed (repo-relative), NOT prefixed
        // by the owning service's name — those diverge for manifest services.
        let p = Pointer {
            file: "fixtures/billing/src/Invoice.scala".into(),
            start_line: 7,
            signature: "case class Invoice(id: String)".into(),
            enclosing: None,
        };
        assert_eq!(
            p.line(""),
            "fixtures/billing/src/Invoice.scala:L7  case class Invoice(id: String)  [-]"
        );
        let q = Pointer {
            file: "a.rs".into(),
            start_line: 1,
            signature: String::new(),
            enclosing: Some("outer".into()),
        };
        assert_eq!(q.line("  (call)"), "a.rs:L1  -  [outer]  (call)");
    }

    #[test]
    fn enclosing_sql_rebinds_the_symbol_alias() {
        let d = enclosing_sql("d");
        assert!(d.contains("p.file = d.file"));
        assert!(d.contains("d.start_line") && d.contains("d.end_line"));
        assert!(!d.contains("s."), "no stale references to the old alias");
    }

    #[test]
    fn fts_query_makes_each_word_a_prefix_term() {
        assert_eq!(fts_query("handle req"), "\"handle\"* \"req\"*");
        // Punctuation is stripped from terms; empty input never yields bad syntax.
        assert_eq!(fts_query("get()"), "\"get\"*");
        assert_eq!(fts_query("   "), "\"\"");
        // FTS5 boolean keywords are quoted to literals, not operators.
        assert_eq!(fts_query("a OR b"), "\"a\"* \"OR\"* \"b\"*");
    }

    #[test]
    fn fts_query_any_ors_terms_for_task_shaped_text() {
        assert_eq!(
            fts_query_any("edge resolution"),
            "\"edge\"* OR \"resolution\"*"
        );
        assert_eq!(fts_query_any("one"), "\"one\"*");
        assert_eq!(fts_query_any(""), "\"\"");
    }

    #[test]
    fn kind_synonyms_expands_generic_kinds_and_passes_native_through() {
        assert_eq!(
            kind_synonyms("function"),
            vec!["function", "fn", "def", "method"]
        );
        assert_eq!(kind_synonyms("fn"), vec!["fn", "function"]);
        assert_eq!(kind_synonyms("module"), vec!["module", "mod"]);
        assert_eq!(kind_synonyms("struct"), vec!["struct"]);
    }

    #[test]
    fn first_json_item_reads_the_leading_element() {
        assert_eq!(
            first_json_item(r#"["Main.scala","b"]"#).as_deref(),
            Some("Main.scala")
        );
        assert_eq!(first_json_item("[]"), None);
        assert_eq!(first_json_item("not json"), None);
    }

    #[test]
    fn show_db_handles_missing_and_existing_databases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let path_str = path.to_str().unwrap();

        // Missing file: reports, does not create the database.
        show_db(path_str).unwrap();
        assert!(!path.exists(), "show_db must not create the database");

        // Existing (empty schema) database: summarizes without error.
        crate::db::open(path_str).unwrap();
        show_db(path_str).unwrap();
    }

    #[test]
    fn clear_db_removes_the_database_and_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let path_str = path.to_str().unwrap();

        // Missing file: no-op, no error.
        clear_db(path_str).unwrap();

        // Existing database plus WAL/SHM sidecars: all removed.
        crate::db::open(path_str).unwrap();
        std::fs::write(format!("{path_str}-wal"), b"").unwrap();
        std::fs::write(format!("{path_str}-shm"), b"").unwrap();
        clear_db(path_str).unwrap();
        assert!(!path.exists());
        assert!(!std::path::Path::new(&format!("{path_str}-wal")).exists());
        assert!(!std::path::Path::new(&format!("{path_str}-shm")).exists());
    }
}
