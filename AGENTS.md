# Repository Guidelines

## Project Structure & Module Organization

`repomap` is a Rust CLI that indexes source trees into a SQLite/FTS5 database. The binary entry point is `src/main.rs`; command parsing lives in `src/cli.rs`. Core indexing, storage, service detection, and query behavior are in `src/index.rs`, `src/db.rs`, `src/services.rs`, and `src/query.rs`. Language support is split between Rust bindings in `src/lang/` and Tree-sitter capture queries in `queries/<language>.scm`. Use `fixtures/billing/` for the sample Scala project used by integration-like indexing tests. The embedded agent guide is `skills/repomap/SKILL.md`.

## Build, Test, and Development Commands

- `cargo build` compiles the debug CLI.
- `cargo test` runs the module unit tests.
- `cargo fmt --check` verifies standard Rust formatting; run `cargo fmt` to apply it.
- `cargo clippy --all-targets -- -D warnings` checks for lint regressions.
- `cargo run -- index --root fixtures/billing` manually exercises indexing against the fixture.
- `cargo build --release` produces `target/release/repomap`; `./install.sh` builds and installs that binary.

## Coding Style & Naming Conventions

Use Rust 2021 and `rustfmt` defaults (four-space indentation). Follow existing conventions: `snake_case` for functions, variables, and modules; `PascalCase` for types and enum variants; concise, verb-oriented helper names such as `resolve_edges`. Keep CLI behavior in `cli.rs`, database access in `db.rs`, and avoid mixing language-specific extraction logic into generic code. When adding a language, add `src/lang/<language>.rs`, `queries/<language>.scm`, its Cargo dependency, and the matching registration in `src/lang/mod.rs`.

## Testing Guidelines

Place focused unit tests in the module they cover under `#[cfg(test)]`, using descriptive `snake_case` names (for example, `incremental_removes_deleted_files`). Use `tempfile` for isolated filesystem/database cases. Update or add fixture coverage when changing parsing, service discovery, or extraction query captures. Run `cargo test` and formatting checks before opening a PR.

## Commit & Pull Request Guidelines

Use short imperative commit subjects, following the existing history: `Add ...`, `Fix ...`, `docs: ...`, or `Honor ...`. Keep each commit focused. PRs should state the user-visible change and testing performed, link any related issue, and include command output or a CLI example for behavior changes. Add screenshots only when documentation or rendered UI assets actually change.

## Generated Files & Local Configuration

Do not commit `.repomap.db` or its SQLite `-wal`/`-shm` sidecars. Treat `repomap.toml` as the repository-level service configuration and update it alongside changes to service layout assumptions.
