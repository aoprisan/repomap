//! Python grammar binding. The only Python-specific code lives here + queries/python.scm.

pub const QUERY: &str = include_str!("../../queries/python.scm");

pub fn language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}
