//! TypeScript grammar binding. The only TS-specific code lives here + queries/typescript.scm.

pub const QUERY: &str = include_str!("../../queries/typescript.scm");

pub fn language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

/// TSX is a distinct grammar: the plain-TS parser chokes on JSX and silently
/// drops every symbol in the file. `.tsx` files must use this one.
pub fn language_tsx() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}
