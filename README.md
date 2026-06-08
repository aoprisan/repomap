# repomap

A compact code-navigation index for LLM agents (and humans). `repomap` parses a
polyglot monorepo with [tree-sitter](https://tree-sitter.github.io/), stores
**compact pointers** — `file:line` + signature + owning service — in a single
SQLite/FTS5 database, and answers navigation queries against it.

It deliberately stores **pointers, never code bodies**. The output is sized for
dropping into an agent's context: one symbol per line, enough to locate and
disambiguate, nothing more.

Languages supported today: **Rust** and **Scala**.

## Install

Build a release binary and copy it onto your `PATH` in one step:

```sh
cargo build --release
./target/release/repomap --install
```

`--install` copies (does not symlink) the binary into the first of
`~/.local/bin`, `~/bin`, or `~/.cargo/bin` that is already on your `PATH`,
falling back to `~/.local/bin`. It warns if the chosen directory isn't on
`PATH`. Because it copies, the installed tool keeps working after `cargo clean`
or moving the source tree.

## Quick start

```sh
cd your-monorepo
repomap index        # build/refresh the index (writes ./.repomap.db)
repomap map          # list services
repomap find Invoice # search symbols
```

Global flags (valid on any subcommand):

| Flag | Default | Meaning |
|------|---------|---------|
| `--root <dir>` | `.` | Repo root to index/query |
| `--db <path>` | `<root>/.repomap.db` | Index database location |

## Commands

### `index [--incremental]`

(Re)indexes the repo. `--incremental` skips files whose git blob hash is
unchanged since the last run, and drops symbols for deleted files.

```
$ repomap index
indexed 14 files (0 skipped, 0 removed), 97 symbols, 114 edges, 2 services [full]
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
`service/path:Lstart  <signature>  [enclosing]`. `-k` caps results (default 10).

```
$ repomap find Invoice -k 3
billing/src/main/scala/billing/Invoice.scala:L14  object InvoiceService extends Repository[Invoice]  [-]
billing/src/main/scala/billing/Invoice.scala:L7  case class Invoice(id: String, amountCents: Long, currency: String)  [-]
billing/src/main/scala/billing/Invoice.scala:L18  def get(id: String): Option[Invoice] = store.get(id)  [InvoiceService]
```

### `def <symbol>`

Definition site(s) of a symbol, one line each.

```
$ repomap def TaxCalculator
billing/src/main/scala/billing/tax/TaxCalculator.scala:L3  object TaxCalculator  [-]
```

### `callers <symbol>`

Symbols that have an edge pointing at `<symbol>` (calls, `extends`, imports),
one line each.

```
$ repomap callers get
billing/src/main/scala/billing/Invoice.scala:L23  val base = get(id).map(_.amountCents).getOrElse(0L)  [total]  (call)
```

> **Note:** edges are resolved best-effort by name (same-service preferred), so
> common names like `get` or `apply` can cross-link to unrelated symbols.

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

## How it works

- **Parse** — tree-sitter parses each file; one `.scm` query per language under
  `queries/` declares what to capture.
- **Extract** — `src/lang/extract.rs` is language-agnostic, driven by
  capture-name conventions: `@def.<kind>` + `@name` → a symbol;
  `@call.name` / `@extends.name` / `@import.*` → best-effort edges.
- **Store** — symbols and FTS live in SQLite (bundled rusqlite, FTS5). Edges are
  stored name-keyed in `edge_raw`, then rebuilt into `edges` on each index run
  by resolving destination names to symbol ids.
- **Incremental** — indexing keys on each file's git blob hash (pure-Rust,
  `git hash-object`-compatible) to skip unchanged files.

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
