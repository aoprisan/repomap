mod cli;
mod db;
mod git;
mod index;
mod install;
mod lang;
mod query;
mod services;
mod skill;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Cmd};

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
    if !matches!(cmd, Cmd::Index { .. }) && !args.no_refresh {
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
        Cmd::Map => query::map(&conn)?,
        Cmd::Find {
            query,
            service,
            kind,
            lang,
            k,
        } => query::find(
            &conn,
            &query,
            &query::FindOpts {
                service,
                kind,
                lang,
                k,
            },
        )?,
        Cmd::Def { symbol } => query::def(&conn, &symbol)?,
        Cmd::Callers { symbol } => query::callers(&conn, &symbol)?,
        Cmd::Callees { symbol } => query::callees(&conn, &symbol)?,
        Cmd::Outline { file } => query::outline(&conn, &file)?,
    }
    Ok(())
}
