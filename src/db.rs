//! SQLite storage: schema, FTS5 index, and connection setup.

use anyhow::Result;
use rusqlite::Connection;

pub fn open(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
  path       TEXT PRIMARY KEY,
  service    TEXT NOT NULL,
  language   TEXT NOT NULL,
  loc        INTEGER NOT NULL,
  git_hash   TEXT NOT NULL,
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
  language       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sym_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_sym_file ON symbols(file);

-- Best-effort references keyed by dst *name*; resolved into `edges` after
-- each index run. Cascades away when its source symbol (and file) is dropped.
CREATE TABLE IF NOT EXISTS edge_raw (
  src_symbol INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
  dst_name   TEXT NOT NULL,
  kind       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_edgeraw_src ON edge_raw(src_symbol);

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
