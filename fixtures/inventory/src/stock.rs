//! Warehouse stock levels for the Rust fixture.

/// Units on hand in the demo warehouse.
pub fn available() -> u32 {
    40
}

/// Reserve `n` units, returning what remains.
pub fn reserve(n: u32) -> u32 {
    available().saturating_sub(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stocked() -> u32 {
        available()
    }

    #[test]
    fn reserve_never_underflows() {
        assert_eq!(reserve(stocked() + 1), 0);
    }
}
