//! Rust grammar binding. The only Rust-specific code lives here + queries/rust.scm.

pub const QUERY: &str = include_str!("../../queries/rust.scm");

pub fn language() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}
