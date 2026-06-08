//! Scala grammar binding. The only Scala-specific code lives here + queries/scala.scm.

pub const QUERY: &str = include_str!("../../queries/scala.scm");

pub fn language() -> tree_sitter::Language {
    tree_sitter_scala::LANGUAGE.into()
}
