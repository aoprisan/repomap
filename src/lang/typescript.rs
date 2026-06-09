//! TypeScript grammar binding. The only TS-specific code lives here + queries/typescript.scm.

pub const QUERY: &str = include_str!("../../queries/typescript.scm");

pub fn language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}
