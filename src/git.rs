//! Git-compatible blob hashing (the incremental-index change key) + loc.

use sha1::{Digest, Sha1};

/// Same bytes `git hash-object` would produce: sha1 of "blob <len>\0<content>".
pub fn blob_hash(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(40);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn loc(src: &str) -> usize {
    src.lines().count()
}
