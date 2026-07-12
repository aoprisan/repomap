# repomap

A compact code-navigation index for LLM agents (and humans). `repomap` parses a
polyglot monorepo with [tree-sitter](https://tree-sitter.github.io/), stores
**compact pointers** — `file:line` + signature + owning service — in a single
SQLite/FTS5 database, and answers navigation queries against it.

It deliberately stores **pointers, never code bodies**. The output is sized for
dropping into an agent's context: one symbol per line, enough to locate and
disambiguate, nothing more.

Languages supported today: **Rust**, **Scala**, **Ruby**, **Python**, and
**TypeScript**.

## Install

Prebuilt binaries for Linux (gnu + static musl) and macOS (Intel + Apple
Silicon) are attached to [GitHub
releases](https://github.com/aoprisan/repomap/releases) — download, extract,
and drop `repomap` on your `PATH`. (Tagging `v*` builds them via
`.github/workflows/release.yml`.)

Or build from source and copy it onto your `PATH` in one step:

```sh
cargo build --release
./target/release/repomap --install
```

`--install` copies (does not symlink) the binary into the first of
`~/.local/bin`, `~/bin`, or `~/.cargo/bin` that is already on your `PATH`,
falling back to `~/.local/bin`. It warns if the chosen directory isn't on
`PATH`. Because it copies, the installed tool keeps working after `cargo clean`
or moving the source tree.

### Install the agent skill

`repomap` ships with a guide that teaches a coding agent how and when to drive
the CLI for code navigation. The guide is embedded in the binary; drop it into a
repo for [Claude Code](https://claude.com/claude-code), [GitHub
Copilot](https://docs.github.com/en/copilot/customizing-copilot/adding-repository-custom-instructions-for-github-copilot),
or [OpenAI Codex](https://github.com/openai/codex) with:

```sh
repomap --install-skill                  # claude (default) -> ./.claude/skills/repomap/SKILL.md
repomap --install-skill copilot          # -> ./.github/copilot-instructions.md
repomap --install-skill codex            # -> ./AGENTS.md
repomap --root path/to/repo --install-skill copilot
```

The target defaults to `claude` when no agent is given, so existing usage is
unchanged. Each agent gets the guide in its conventional location:

| Agent | File | Behavior |
|-------|------|----------|
| `claude` | `<root>/.claude/skills/repomap/SKILL.md` | repomap-owned; written whole (with skill frontmatter), overwriting any prior copy |
| `copilot` | `<root>/.github/copilot-instructions.md` | shared file; spliced into a marked block, preserving your other content |
| `codex` | `<root>/AGENTS.md` | shared file; spliced into a marked block, preserving your other content |

Because Copilot and Codex read a *shared* instructions file that may already
hold your own content, `repomap` writes its section between
`<!-- BEGIN repomap … -->` / `<!-- END repomap -->` markers: a re-install
replaces just that block and leaves everything else untouched (the frontmatter
is dropped there, since those agents read the file as plain instructions). Once
installed, the agent will reach for `repomap` to locate definitions, find
callers, and map services instead of broad file reads.

## Quick start

```sh
cd your-monorepo
repomap map                    # list services
repomap find Invoice           # search symbols
repomap rank                   # most important symbols (PageRank)
repomap impact InvoiceService  # blast radius of changing a symbol
repomap cochange billing.rs    # files that historically change with it
repomap context "tax rounding" # token-budgeted orientation pack for a task
```

No setup step: **query commands refresh the index automatically** before
answering — a full build on first use (writing `./.repomap.db`), an
incremental one after — so results always reflect the working tree, even right
after you edit files. When a refresh actually reindexed something, a one-line
note goes to stderr; pass `--no-refresh` to answer from the index as-is.
`repomap index` is still there for building explicitly (e.g. in CI or a
pre-commit hook).

Global flags (valid on any subcommand):

| Flag | Default | Meaning |
|------|---------|---------|
| `--root <dir>` | `.` | Repo root to index/query |
| `--db <path>` | `<root>/.repomap.db` | Index database location |
| `--no-refresh` | | Skip the automatic index refresh before a query |
| `--show-db` | | Print the resolved database path and stats, then exit |
| `--clear-db` | | Delete the resolved database (and its WAL/SHM sidecars), then exit |

`--show-db` answers "which database am I actually using?" — it prints the
resolved path and, if the file exists, its size and row counts. It opens the
file read-only and never creates one:

```
$ repomap --show-db
./.repomap.db
  size      144 KiB
  services  2
  files     14
  symbols   120
  edges     109
  indexed   1780942229 (epoch seconds)

$ repomap --db /tmp/other.db --show-db
/tmp/other.db  (not indexed yet — run `repomap index`)
```

`--clear-db` removes the resolved database file along with SQLite's `-wal` and
`-shm` sidecars, then exits without recreating an empty one. It's a no-op if the
file doesn't exist:

```
$ repomap --clear-db
./.repomap.db  (cleared)

$ repomap --clear-db
./.repomap.db  (nothing to clear)
```

## Commands

### `index [--incremental]`

(Re)indexes the repo. The scan honors `.gitignore`/`.ignore` rules (even in a
tree exported without `.git`, though not your machine-global gitignore), always
skips dependency/cache directories (`node_modules`, `target`, virtualenvs, …)
as a fallback, and drops files over 1 MiB — hand-written source essentially
never gets that big, generated bundles routinely do. Files are parsed and
extracted in parallel across CPU cores.

`--incremental` skips unchanged files — first on a cheap
stat (mtime + size), falling back to the git blob hash when the stat moved —
and drops symbols for deleted files. If the service definitions changed since
the last run (edited `repomap.toml`, or a changed inferred layout), an
incremental request is upgraded to a full reindex — skipped files would
otherwise keep stale service attribution.

```
$ repomap index
indexed 14 files (0 skipped, 0 removed), 97 symbols, 114 edges, 2 services [full]
$ repomap index --incremental        # after editing repomap.toml
indexed 14 files (0 skipped, 0 removed), 97 symbols, 114 edges, 1 services [full: service definitions changed]
```

### `map`

Lists services as `name  (stack)  N files  entrypoint`.

```
$ repomap map
billing  (scala)  2 files  src/main/scala/billing/InvoiceService.scala
repomap  (rust)  12 files  main.rs
```

### `find <query> [--service S] [--kind K] [--lang L] [-k N]`

Full-text symbol search. Each hit is one line:
`path:Lstart  <signature>  [enclosing]`, where `path` is the repo-relative
file path — openable as printed. `-k` caps results (default 10). `--kind`
accepts the language-native kind (`fn`, `def`, `struct`, `object`, …) or a
generic alias: `function` matches `fn`/`def`/`method`, `module` matches
`mod`.

```
$ repomap find Invoice -k 3
fixtures/billing/src/main/scala/billing/Invoice.scala:L14  object InvoiceService extends Repository[Invoice]  [-]
fixtures/billing/src/main/scala/billing/Invoice.scala:L7  case class Invoice(id: String, amountCents: Long, currency: String)  [-]
fixtures/billing/src/main/scala/billing/Invoice.scala:L18  def get(id: String): Option[Invoice] = store.get(id)  [InvoiceService]
```

### `def <symbol>`

Definition site(s) of a symbol, one line each.

```
$ repomap def TaxCalculator
fixtures/billing/src/main/scala/billing/tax/TaxCalculator.scala:L3  object TaxCalculator  [-]
```

### `callers <symbol>`

Symbols that have an edge pointing at `<symbol>` (calls, `extends`, imports),
one line each.

```
$ repomap callers get
fixtures/billing/src/main/scala/billing/Invoice.scala:L23  val base = get(id).map(_.amountCents).getOrElse(0L)  [total]  (call)
```

> **Note:** edges are resolved best-effort by name. A bare reference resolves
> within the source's **own service** (same-file definition preferred). A name
> the source file **imports** may additionally resolve across service
> boundaries — the import is explicit evidence the reference points outside.
> Anything else is dropped rather than guessed, so `get`/`apply`-style common
> names never cross-link to unrelated symbols in other services.

### `callees <symbol>`

The inverse of `callers`: symbols that `<symbol>` has an edge pointing at —
what it calls, extends, or imports.

```
$ repomap callees total
fixtures/billing/src/main/scala/billing/Invoice.scala:L18  def get(id: String): Option[Invoice] = store.get(id)  [InvoiceService]  (call)
```

### `outline <file>`

All symbols defined in one file, in source order. `<file>` is the exact
repo-relative path, or a path suffix when that's unambiguous enough
(`outline Invoice.scala`).

```
$ repomap outline fixtures/billing/src/main/scala/billing/Invoice.scala
fixtures/billing/src/main/scala/billing/Invoice.scala:L7  case class Invoice(id: String, amountCents: Long, currency: String)  [-]
fixtures/billing/src/main/scala/billing/Invoice.scala:L14  object InvoiceService extends Repository[Invoice]  [-]
fixtures/billing/src/main/scala/billing/Invoice.scala:L18  def get(id: String): Option[Invoice] = store.get(id)  [InvoiceService]
```

### `rank [--service S] [-k N]`

The structurally most important symbols, by **PageRank over the reference
graph** — a symbol used by important symbols is important, which raw caller
counts can't express. Scores are normalized so the top symbol in scope is 100.
Every index run recomputes ranks, and `find`/`def` use them to break ties, so
the definition a repo actually leans on surfaces before same-named helpers.

Run it (optionally per `--service`) to learn what a codebase revolves around
before reading anything:

```
$ repomap rank -k 3
src/services.rs:L76  pub fn new(mut services: Vec<Service>) -> Self  [Resolver]  (score 100, 41 callers)
src/lang/mod.rs:L75  fn get(cell: &'static OnceLock<Query>, ...) -> &'static Query  [compiled_query]  (score 69, 69 callers)
src/query.rs:L350  pub fn map(conn: &Connection) -> Result<()>  [-]  (score 32, 31 callers)
```

(Note the second line: more callers, lower score — *who* references you
matters, not just how many.)

### `impact <symbol> [--depth N] [-k N]`

The transitive blast radius of changing `<symbol>`: its callers, their
callers, and so on up to `--depth` hops (default 2), each line tagged with its
distance, nearest and most important first, with a closing summary. Run it
**before** modifying a shared symbol to see what could break:

```
$ repomap impact resolve_edges
src/index.rs:L56  pub fn run(...) -> Result<Summary>  [-]  (depth 1)
src/index.rs:L541  pub fn refresh(...) -> Result<()>  [-]  (depth 2)
src/main.rs:L21  fn main() -> Result<()>  [-]  (depth 2)
impact: 9 symbols in 2 files across 1 services (depth ≤ 2)
```

### `cochange <file> [--commits N] [-k N]`

Files that **historically change in the same commit** as `<file>`, mined from
the last `--commits` (default 1000) non-merge commits. This is coupling no
static analysis can see — a schema file and its DAO, a config and the code
reading it, mirrored client/server types — and exactly the "when you edit X,
don't forget Y" signal to check before a change. Bulk commits (>30 files:
mass renames, format sweeps) are skipped as noise, and partners deleted from
the working tree are dropped. `<file>` is an exact repo-relative path or an
unambiguous suffix; unindexed files (`.sql`, `.yml`, …) work too when the
path exists on disk.

```
$ repomap cochange src/query.rs
src/cli.rs  5/7 commits (71%)
src/main.rs  5/7 commits (71%)
README.md  4/7 commits (57%)
```

Requires `git` on PATH and commit history; without either it explains itself
instead of answering.

### `context <query> [--budget N]`

A one-shot, token-budgeted **orientation pack** for a task: seed symbols
matching `<query>` (any-word match, fullest matches first), each with its most
important callers (`<-`) and callees (`->`), plus the services involved —
packed greedily to fit `--budget` tokens (default 2000, estimated at ~4
chars/token). One command replaces the find → def → callers → callees dance
when starting work, and the output is still pointers-only, sized for pasting
straight into an agent's context:

```
$ repomap context "index refresh" --budget 300
# context: index refresh
## services
repomap  (rust)  19 files
## symbols
src/index.rs:L541  pub fn refresh(conn: &mut Connection, ...) -> Result<()>
  <- src/main.rs:L21  main  (call)
  -> src/index.rs:L56  run  (call)
[~153 tokens / budget 300; 2/2 seeds]
```

The footer reports actual usage; when seeds are cut for budget it says so
(`raise --budget for more`).

## Services

`repomap` groups files into services. It reads `repomap.toml` at the repo root
if present; otherwise it infers one service per top-level directory.

```toml
[[service]]
name = "billing"
path = "fixtures/billing"
stack = "scala"
purpose = "Invoice + tax domain"
entrypoints = ["src/main/scala/billing/InvoiceService.scala"]
deps = []
```

A file is assigned to the service whose `path` is the longest matching prefix.
Files that fall under no declared `path` land in a synthetic `root` service
(shown in `map` only if it actually owns files), so a partial manifest never
misattributes them to a sibling service.

## How it works

- **Scan** — the walk (via the `ignore` crate) honors `.gitignore`/`.ignore`,
  falls back to a built-in skip list for dependency/cache directories, and
  drops files over 1 MiB. Entries are sorted so symbol ids — and edge-
  resolution tie-breaks — are deterministic across runs.
- **Parse** — tree-sitter parses files in parallel (rayon); one `.scm` query
  per language under `queries/` declares what to capture, compiled once per
  grammar and shared across threads.
- **Extract** — `src/lang/extract.rs` is language-agnostic, driven by
  capture-name conventions: `@def.<kind>` + `@name` → a symbol;
  `@call.name` / `@extends.name` / `@import.*` → best-effort edges.
- **Store** — symbols and FTS live in SQLite (bundled rusqlite, FTS5). Edges are
  stored name-keyed in `edge_raw`, then rebuilt into `edges` on each index run
  by resolving destination names to symbol ids — same service, or cross-service
  when the source file imports the name; same-file preferred, self-edges
  excluded (see the note under `callers`). Imported names are recorded per file
  in `file_imports`, including top-level imports.
- **Rank** — after edges are resolved, PageRank runs over the symbol graph
  (damping 0.85, importance flowing along references) and is stored on each
  symbol; `rank` reads it directly and `find`/`def` use it as a tie-breaker.
  `impact` BFS-walks the same graph in reverse; `cochange` is independent of
  the graph — it mines `git log` at query time.
- **Incremental** — a file is skipped when its stat (mtime + size) is
  untouched, or — when the stat moved — its git blob hash (pure-Rust,
  `git hash-object`-compatible) still matches. Query commands run an
  incremental pass automatically before answering.
- **Migrations** — the database is a cache: on a schema-version bump, `repomap`
  drops and rebuilds it on the next (auto-)index instead of migrating in place.

## Adding a language

The extractor is generic; adding a language is a three-step seam:

1. Add the grammar binding in `src/lang/<x>.rs` (and its crate to `Cargo.toml`).
2. Add `queries/<x>.scm` using the `@def.*` / `@name` / `@call.*` / `@extends.*`
   / `@import.*` capture conventions.
3. Add one match arm per `match` in `src/lang/mod.rs`.

## Development

```sh
cargo build
cargo test
```
