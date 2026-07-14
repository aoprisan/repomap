mod cli;
mod context;
mod db;
mod git;
mod graph;
mod history;
mod index;
mod install;
mod lang;
mod query;
mod services;
mod skill;
mod usage;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Cmd};

fn record_query(conn: &rusqlite::Connection, command: &str, results: usize) -> Result<()> {
    let saved = usage::estimate_tokens_saved(conn, results);
    usage::record(conn, command, results, saved)
}

fn main() -> Result<()> {
    let args = Cli::parse();

    // `--install` is a one-shot side task: copy the binary onto PATH and exit
    // before we open the database or touch the repo.
    if args.install {
        return install::install();
    }

    let root = PathBuf::from(&args.root);

    // `--install-skill [AGENT]` writes the bundled repomap guide into the
    // target agent's conventional location and exits, before touching the db.
    if let Some(agent) = args.install_skill {
        return skill::install_skill(&root, agent);
    }
    let db_path = args
        .db
        .clone()
        .unwrap_or_else(|| root.join(".repomap.db").to_string_lossy().to_string());
    let db_file = PathBuf::from(&db_path);

    // `--show-db` is a read-only diagnostic: report the path we resolved (and
    // its contents if it exists) without creating an empty database first.
    if args.show_db {
        return query::show_db(&db_path);
    }

    // `--clear-db` removes the index file (incl. SQLite's WAL/SHM sidecars) and
    // exits, so we never reopen/recreate an empty database afterward.
    if args.clear_db {
        return query::clear_db(&db_path);
    }

    let mut conn = db::open(&db_path)?;

    let cmd = match args.cmd {
        Some(c) => c,
        None => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            return Ok(());
        }
    };

    // Query commands answer from a live index: refresh first (full when
    // nothing is indexed yet, incremental otherwise) so an agent that just
    // edited files never follows stale line numbers. `--no-refresh` opts out,
    // and a refresh failure (e.g. read-only checkout) degrades to a warning
    // rather than blocking the query.
    if !matches!(cmd, Cmd::Index { .. } | Cmd::Usage { .. }) && !args.no_refresh {
        if let Err(e) = index::refresh(&mut conn, &root, &db_file) {
            eprintln!("warning: index refresh failed ({e}); answering from the existing index");
        }
    }

    match cmd {
        Cmd::Index { incremental } => {
            let s = index::run(&mut conn, &root, incremental, &db_file)?;
            println!(
                "indexed {} files ({} skipped, {} removed), {} symbols, {} edges, {} services [{}]",
                s.files_indexed,
                s.files_skipped,
                s.files_removed,
                s.symbols,
                s.edges,
                s.services,
                s.mode
            );
        }
        Cmd::Map => {
            let results = query::map(&conn)?;
            record_query(&conn, "map", results)?;
        }
        Cmd::Find {
            query,
            service,
            kind,
            lang,
            k,
        } => {
            let results = query::find(
                &conn,
                &query,
                &query::FindOpts {
                    service,
                    kind,
                    lang,
                    k,
                },
            )?;
            record_query(&conn, "find", results)?;
        }
        Cmd::Def { symbol } => {
            let results = query::def(&conn, &symbol)?;
            record_query(&conn, "def", results)?;
        }
        Cmd::Callers { symbol } => {
            let results = query::callers(&conn, &symbol)?;
            record_query(&conn, "callers", results)?;
        }
        Cmd::Callees { symbol } => {
            let results = query::callees(&conn, &symbol)?;
            record_query(&conn, "callees", results)?;
        }
        Cmd::Outline { file } => {
            let results = query::outline(&conn, &file)?;
            record_query(&conn, "outline", results)?;
        }
        Cmd::Rank { service, k } => {
            let results = query::rank(&conn, service.as_deref(), k)?;
            record_query(&conn, "rank", results)?;
        }
        Cmd::Impact { symbol, depth, k } => {
            let results = query::impact(&conn, &symbol, depth, k)?;
            record_query(&conn, "impact", results)?;
        }
        Cmd::Cochange { file, commits, k } => {
            let results = history::cochange(&conn, &root, &file, commits, k)?;
            record_query(&conn, "cochange", results)?;
        }
        Cmd::Context { query, budget } => {
            let results = context::context(&conn, &query, budget)?;
            record_query(&conn, "context", results)?;
        }
        Cmd::Usage { reset } => {
            if reset {
                usage::reset(&conn)?;
            } else {
                usage::report(&conn)?;
            }
        }
    }
    Ok(())
}
