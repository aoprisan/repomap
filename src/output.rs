//! Versioned output contract shared by every command.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use clap::ValueEnum;
use serde_json::{json, Value};

pub const SCHEMA_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Jsonl,
}

static FORMAT: AtomicU8 = AtomicU8::new(0);
static COMMAND: OnceLock<String> = OnceLock::new();

pub fn configure(format: OutputFormat, command: impl Into<String>) {
    FORMAT.store(format as u8, Ordering::Relaxed);
    let _ = COMMAND.set(command.into());
}

pub fn is_jsonl() -> bool {
    FORMAT.load(Ordering::Relaxed) == OutputFormat::Jsonl as u8
}

fn envelope(event: &str, data: Value) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "command": COMMAND.get().map(String::as_str).unwrap_or("unknown"),
        "type": event,
        "data": data,
    })
}

/// Emit one result. JSONL always contains one complete versioned object per
/// line; text mode preserves the compact existing CLI contract.
pub fn emit(event: &str, data: Value, text: impl AsRef<str>) {
    if is_jsonl() {
        println!("{}", envelope(event, data));
    } else {
        println!("{}", text.as_ref());
    }
}

pub fn diagnostic(level: &str, code: &str, message: impl AsRef<str>) {
    let message = message.as_ref();
    if is_jsonl() {
        eprintln!(
            "{}",
            envelope(
                "diagnostic",
                json!({"level": level, "code": code, "message": message})
            )
        );
    } else if level == "error" {
        eprintln!("error: {message}");
    } else {
        eprintln!("{level}: {message}");
    }
}

pub fn note(code: &str, message: impl AsRef<str>) {
    diagnostic("info", code, message);
}

pub fn warning(code: &str, message: impl AsRef<str>) {
    diagnostic("warning", code, message);
}

pub fn no_match(message: impl AsRef<str>) {
    diagnostic("info", "no_match", message);
}
