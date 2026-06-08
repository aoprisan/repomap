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

/// One result line. `service`/`within` reconstruct the openable repo path.
struct Pointer {
    service: String,
    within: String,
    start_line: i64,
    signature: String,
    enclosing: Option<String>,
}

impl Pointer {
    fn print(&self, suffix: &str) {
        let enc = self.enclosing.as_deref().unwrap_or("-");
        let sig = if self.signature.is_empty() {
            "-"
        } else {
            &self.signature
        };
        println!(
            "{}/{}:L{}  {}  [{}]{}",
            self.service, self.within, self.start_line, sig, enc, suffix
        );
    }
}

/// Strip a service's root-path prefix to get the path within the service,
/// so `service/within` reconstructs the real repo-relative file path.
fn within(service_path: &str, file: &str) -> String {
    if service_path == "." || service_path.is_empty() {
        return file.to_string();
    }
    file.strip_prefix(&format!("{service_path}/"))
        .unwrap_or(file)
        .to_string()
}

// SQL fragment computing the innermost enclosing symbol name for `s`.
const ENCLOSING_SQL: &str = "(SELECT p.name FROM symbols p
     WHERE p.file = s.file AND p.id <> s.id
       AND p.start_line <= s.start_line AND p.end_line >= s.end_line
     ORDER BY (p.end_line - p.start_line) ASC LIMIT 1)";

// SQL fragment for in-degree (incoming edges), the find tie-breaker.
const INDEG_SQL: &str = "(SELECT count(*) FROM edges WHERE dst_symbol = s.id)";

fn row_to_pointer(
    service: String,
    service_path: String,
    file: String,
    start_line: i64,
    signature: Option<String>,
    enclosing: Option<String>,
) -> Pointer {
    Pointer {
        within: within(&service_path, &file),
        service,
        start_line,
        signature: signature.unwrap_or_default(),
        enclosing,
    }
}

pub fn find(conn: &Connection, query: &str, opts: &FindOpts) -> Result<()> {
    let mut sql = format!(
        "SELECT s.service, COALESCE(sv.path, s.service), s.file, s.start_line, s.signature,
                {ENCLOSING_SQL}
         FROM symbols_fts f
         JOIN symbols s ON s.id = f.rowid
         LEFT JOIN services sv ON sv.name = s.service
         WHERE symbols_fts MATCH ?1"
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query(query))];
    if let Some(v) = &opts.service {
        params.push(Box::new(v.clone()));
        sql.push_str(&format!(" AND s.service = ?{}", params.len()));
    }
    if let Some(v) = &opts.kind {
        params.push(Box::new(v.clone()));
        sql.push_str(&format!(" AND s.kind = ?{}", params.len()));
    }
    if let Some(v) = &opts.lang {
        params.push(Box::new(v.clone()));
        sql.push_str(&format!(" AND s.language = ?{}", params.len()));
    }
    sql.push_str(&format!(
        " ORDER BY bm25(symbols_fts), {INDEG_SQL} DESC LIMIT {}",
        opts.k
    ));

    let mut stmt = conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(pref.as_slice(), |r| {
        Ok(row_to_pointer(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })?;
    let mut any = false;
    for p in rows {
        p?.print("");
        any = true;
    }
    if !any {
        eprintln!("no matches");
    }
    Ok(())
}

pub fn def(conn: &Connection, symbol: &str) -> Result<()> {
    let sql = format!(
        "SELECT s.service, COALESCE(sv.path, s.service), s.file, s.start_line, s.signature,
                {ENCLOSING_SQL}
         FROM symbols s
         LEFT JOIN services sv ON sv.name = s.service
         WHERE s.name = ?1
         ORDER BY {INDEG_SQL} DESC, s.file, s.start_line"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([symbol], |r| {
        Ok(row_to_pointer(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })?;
    let mut any = false;
    for p in rows {
        p?.print("");
        any = true;
    }
    if !any {
        eprintln!("no definition for '{symbol}'");
    }
    Ok(())
}

pub fn callers(conn: &Connection, symbol: &str) -> Result<()> {
    // Callers = source symbols of edges whose dst is a symbol named `symbol`.
    let sql = format!(
        "SELECT s.service, COALESCE(sv.path, s.service), s.file, s.start_line, s.signature,
                {ENCLOSING_SQL}, e.kind
         FROM edges e
         JOIN symbols d ON d.id = e.dst_symbol
         JOIN symbols s ON s.id = e.src_symbol
         LEFT JOIN services sv ON sv.name = s.service
         WHERE d.name = ?1
         GROUP BY s.id, e.kind
         ORDER BY s.file, s.start_line"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([symbol], |r| {
        let kind: String = r.get(6)?;
        Ok((
            row_to_pointer(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?),
            kind,
        ))
    })?;
    let mut any = false;
    for row in rows {
        let (p, kind) = row?;
        p.print(&format!("  ({kind})"));
        any = true;
    }
    if !any {
        eprintln!("no callers for '{symbol}'");
    }
    Ok(())
}

pub fn map(conn: &Connection) -> Result<()> {
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
    for row in rows {
        let (name, stack, entrypoints, nfiles) = row?;
        let stack = stack.unwrap_or_else(|| "?".into());
        let entry = first_json_item(entrypoints.as_deref().unwrap_or("[]"))
            .unwrap_or_else(|| "-".into());
        println!("{name}  ({stack})  {nfiles} files  {entry}");
    }
    Ok(())
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
fn fts_query(q: &str) -> String {
    let terms: Vec<String> = q
        .split_whitespace()
        .map(|t| {
            let clean: String = t.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
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
        "\"\"".to_string()
    } else {
        terms.join(" ")
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
    fn fts_query_makes_each_word_a_prefix_term() {
        assert_eq!(fts_query("handle req"), "\"handle\"* \"req\"*");
        // Punctuation is stripped from terms; empty input never yields bad syntax.
        assert_eq!(fts_query("get()"), "\"get\"*");
        assert_eq!(fts_query("   "), "\"\"");
        // FTS5 boolean keywords are quoted to literals, not operators.
        assert_eq!(fts_query("a OR b"), "\"a\"* \"OR\"* \"b\"*");
    }

    #[test]
    fn first_json_item_reads_the_leading_element() {
        assert_eq!(first_json_item(r#"["Main.scala","b"]"#).as_deref(), Some("Main.scala"));
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
