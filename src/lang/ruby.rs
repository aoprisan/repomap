//! Ruby grammar binding. The only Ruby-specific code lives here + queries/ruby.scm.

pub const QUERY: &str = include_str!("../../queries/ruby.scm");

pub fn language() -> tree_sitter::Language {
    tree_sitter_ruby::LANGUAGE.into()
}
