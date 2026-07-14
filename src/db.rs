//! SQLite storage: schema, FTS5 index, and connection setup.

use anyhow::Result;
use rusqlite::Connection;

/// Bumped whenever the derived-index schema changes shape. The index is a
/// cache over the working tree, so migration drops and rebuilds those tables;
/// user-owned lifetime usage data is retained.
const SCHEMA_VERSION: i32 = 5;

pub fn open(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // Query commands auto-refresh the index first, so two concurrent repomap
    // invocations (an agent running commands in parallel is the normal case)
    // can hit the database at once. Without a busy timeout the loser gets an
    // immediate SQLITE_BUSY error instead of briefly waiting its turn.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;

    let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != SCHEMA_VERSION {
        // Old (or brand-new) database: wipe any previous-shape tables and
        // recreate. Dropped child-first so this works even with FKs enforced.
        conn.execute_batch(
            "DROP TABLE IF EXISTS symbols_fts;
             DROP TABLE IF EXISTS edges;
             DROP TABLE IF EXISTS edge_raw;
             DROP TABLE IF EXISTS file_imports;
             DROP TABLE IF EXISTS symbols;
             DROP TABLE IF EXISTS files;
             DROP TABLE IF EXISTS services;
             DROP TABLE IF EXISTS meta;",
        )?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }

    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

const SCHEMA: &str = r#"
-- Indexer bookkeeping (e.g. the service-definition fingerprint that gates
-- incremental runs).
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- Lifetime CLI usage is user data, not a derived part of the index. It is
-- deliberately omitted from the schema-mismatch drop list above.
CREATE TABLE IF NOT EXISTS usage (
  command      TEXT PRIMARY KEY,
  runs         INTEGER NOT NULL,
  results      INTEGER NOT NULL,
  tokens_saved INTEGER NOT NULL,
  last_used    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS files (
  path       TEXT PRIMARY KEY,
  service    TEXT NOT NULL,
  language   TEXT NOT NULL,
  loc        INTEGER NOT NULL,
  git_hash   TEXT NOT NULL,
  -- mtime (ns) + size let incremental runs skip a file on a cheap stat,
  -- falling back to the content hash only when they moved.
  mtime      INTEGER NOT NULL DEFAULT 0,
  size       INTEGER NOT NULL DEFAULT 0,
  indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
  id             INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  kind           TEXT NOT NULL,
  file           TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  start_line     INTEGER NOT NULL,
  end_line       INTEGER NOT NULL,
  signature      TEXT,
  doc_first_line TEXT,
  service        TEXT NOT NULL,
  language       TEXT NOT NULL,
  -- PageRank over the resolved edge graph, recomputed on every index run.
  -- Importance flows along references: a symbol used by important symbols is
  -- important. Ranks sum to ~1 across the repo; 0 until the first compute.
  rank           REAL NOT NULL DEFAULT 0
);
-- Composite indexes sized for edge resolution: each resolver tier is an
-- exact-range probe (name+service / file+name) whose entries are already in
-- id order, so `ORDER BY id LIMIT 1` stops after a row or two. Name-only and
-- file-only lookups (def, outline, enclosing) use the same indexes as
-- prefixes.
CREATE INDEX IF NOT EXISTS idx_sym_name_service ON symbols(name, service);
CREATE INDEX IF NOT EXISTS idx_sym_file_name ON symbols(file, name);

-- Best-effort references keyed by dst *name*; resolved into `edges` after
-- each index run. Cascades away when its source symbol (and file) is dropped.
CREATE TABLE IF NOT EXISTS edge_raw (
  src_symbol INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
  dst_name   TEXT NOT NULL,
  kind       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_edgeraw_src ON edge_raw(src_symbol);

-- Names a file imports (from any import/use statement, including top-level
-- ones that have no enclosing symbol). An import licenses cross-service edge
-- resolution for that name from that file.
CREATE TABLE IF NOT EXISTS file_imports (
  file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  name TEXT NOT NULL,
  PRIMARY KEY (file, name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS edges (
  src_symbol INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
  dst_symbol INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
  kind       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst_symbol);
CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src_symbol);

CREATE TABLE IF NOT EXISTS services (
  name        TEXT PRIMARY KEY,
  path        TEXT NOT NULL,
  stack       TEXT,
  purpose     TEXT,
  entrypoints TEXT,
  deps        TEXT
);

CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
  name, signature, doc_first_line,
  content='symbols', content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
  INSERT INTO symbols_fts(rowid, name, signature, doc_first_line)
  VALUES (new.id, new.name, new.signature, new.doc_first_line);
END;
CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
  INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, doc_first_line)
  VALUES ('delete', old.id, old.name, old.signature, old.doc_first_line);
END;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_sets_a_busy_timeout_for_concurrent_invocations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let conn = open(path.to_str().unwrap()).unwrap();
        let ms: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(ms >= 1000, "busy_timeout must be set, got {ms} ms");
    }

    #[test]
    fn open_stamps_the_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let conn = open(path.to_str().unwrap()).unwrap();
        let v: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn open_wipes_an_older_schema_instead_of_failing_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let path_str = path.to_str().unwrap().to_string();

        // Simulate a pre-versioning database: old `files` shape (no
        // mtime/size), user_version 0, with a row in it.
        {
            let conn = Connection::open(&path_str).unwrap();
            conn.execute_batch(
                "CREATE TABLE files (path TEXT PRIMARY KEY, service TEXT NOT NULL,
                     language TEXT NOT NULL, loc INTEGER NOT NULL,
                     git_hash TEXT NOT NULL, indexed_at INTEGER NOT NULL);
                 INSERT INTO files VALUES ('a.rs', 'svc', 'rust', 1, 'h', 0);",
            )
            .unwrap();
        }

        // Reopening must migrate (drop + recreate), leaving a usable, empty,
        // current-shape database rather than erroring on the missing columns.
        let conn = open(&path_str).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "old-shape data is dropped, not carried over");
        conn.execute(
            "INSERT INTO files(path, service, language, loc, git_hash, mtime, size, indexed_at)
             VALUES ('b.rs', 'svc', 'rust', 1, 'h', 1, 2, 0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn schema_rebuild_preserves_lifetime_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let path_str = path.to_str().unwrap();
        let conn = open(path_str).unwrap();
        conn.execute(
            "INSERT INTO usage(command, runs, results, tokens_saved, last_used)
             VALUES ('find', 2, 3, 400, 1)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
            .unwrap();
        drop(conn);

        let conn = open(path_str).unwrap();
        let runs: i64 = conn
            .query_row("SELECT runs FROM usage WHERE command = 'find'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(runs, 2);
    }
}
