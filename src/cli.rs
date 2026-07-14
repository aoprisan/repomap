use clap::{Args, Parser, Subcommand};

use crate::output::OutputFormat;
use crate::skill::Agent;

#[derive(Parser)]
#[command(
    name = "repomap",
    version,
    about = "Compact code-navigation index for LLM agents"
)]
pub struct Cli {
    /// Repo root to index/query.
    #[arg(long, global = true, default_value = ".")]
    pub root: String,

    /// Index database path (default: <root>/.repomap.db).
    #[arg(long, global = true)]
    pub db: Option<String>,

    /// Copy this binary into a `bin` directory on your PATH and exit.
    #[arg(long, global = true)]
    pub install: bool,

    /// Install the bundled repomap guide for a coding agent into `<root>` and
    /// exit. Target one of `claude` (default), `copilot`, or `codex`.
    #[arg(
        long,
        global = true,
        value_name = "AGENT",
        num_args = 0..=1,
        default_missing_value = "claude"
    )]
    pub install_skill: Option<Agent>,

    /// Print the resolved index database path (with stats) and exit.
    #[arg(long, global = true)]
    pub show_db: bool,

    /// Delete the resolved index database and exit.
    #[arg(long, global = true)]
    pub clear_db: bool,

    /// Skip the automatic index refresh that query commands run first;
    /// answer from the index as-is (possibly stale).
    #[arg(long, global = true)]
    pub no_refresh: bool,

    /// If automatic refresh fails, explicitly allow querying the older index.
    /// The command still reports a distinct stale-result exit status.
    #[arg(long, global = true, conflicts_with = "no_refresh")]
    pub allow_stale: bool,

    /// Output contract: compact human text or versioned JSON Lines.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

/// Select one or more definitions. A qualified name such as
/// `InvoiceService::get` may be passed as SYMBOL; filters make repeated names
/// in large repositories explicit and scriptable.
#[derive(Args, Clone)]
pub struct SymbolSelectorArgs {
    /// Exact bare or qualified symbol name (for example `get` or `InvoiceService::get`).
    pub symbol: String,
    /// Restrict the selected definition to one service.
    #[arg(long)]
    pub service: Option<String>,
    /// Restrict by exact repo-relative path or path suffix.
    #[arg(long)]
    pub file: Option<String>,
    /// Restrict by native kind or generic `function`/`module` alias.
    #[arg(long)]
    pub kind: Option<String>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// (Re)index the repo. --incremental skips files whose git hash is unchanged.
    Index {
        #[arg(long)]
        incremental: bool,
    },
    /// List services: `name  (stack)  N files  entrypoint`.
    Map,
    /// Search symbols; each hit one line: `path:Lstart  <sig>  [enclosing]`.
    Find {
        query: String,
        #[arg(long)]
        service: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(short = 'k', default_value_t = 10)]
        k: usize,
    },
    /// Definition site(s) of a symbol, one line each.
    Def {
        #[command(flatten)]
        selector: SymbolSelectorArgs,
    },
    /// Symbols with an edge pointing at <symbol>, one line each.
    Callers {
        #[command(flatten)]
        selector: SymbolSelectorArgs,
    },
    /// Symbols that <symbol> points at (calls, extends, imports), one line each.
    Callees {
        #[command(flatten)]
        selector: SymbolSelectorArgs,
    },
    /// All symbols defined in <file> (exact repo-relative path, or a path
    /// suffix like `Invoice.scala`), in source order.
    Outline { file: String },
    /// Most important symbols by PageRank over the reference graph
    /// (score 100 = top symbol in scope). Orient in an unfamiliar codebase.
    Rank {
        #[arg(long)]
        service: Option<String>,
        #[arg(short = 'k', default_value_t = 20)]
        k: usize,
    },
    /// Blast radius of changing <symbol>: transitive callers up to --depth
    /// hops, nearest and most important first.
    Impact {
        #[command(flatten)]
        selector: SymbolSelectorArgs,
        #[arg(long, default_value_t = 2)]
        depth: usize,
        #[arg(short = 'k', default_value_t = 40)]
        k: usize,
    },
    /// Files that historically change in the same commit as <file> (exact
    /// repo-relative path, or a suffix), from git history.
    Cochange {
        file: String,
        /// How many recent commits to mine.
        #[arg(long, default_value_t = 1000)]
        commits: usize,
        #[arg(short = 'k', default_value_t = 10)]
        k: usize,
    },
    /// One-shot orientation pack for a task: seed symbols matching <query>,
    /// their callers/callees, and the services involved — packed to fit a
    /// token budget.
    Context {
        query: String,
        /// Approximate token budget for the pack.
        #[arg(long, default_value_t = 2000)]
        budget: usize,
    },
    /// Analyze the working-tree diff as symbols: classify API/body changes,
    /// trace affected callers (including callers of deleted definitions), and
    /// select graph-linked tests for review.
    Changes {
        /// Git revision to compare the working tree against.
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Maximum caller depth for the review and test surface.
        #[arg(long, default_value_t = 3)]
        depth: usize,
        /// Maximum changed/affected/test rows printed per section.
        #[arg(short = 'k', default_value_t = 30)]
        k: usize,
    },
    /// Report lifetime query usage and estimated tokens saved.
    Usage {
        /// Clear all recorded usage statistics.
        #[arg(long)]
        reset: bool,
    },
}
