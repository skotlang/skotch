//! Content hashing for incremental compilation.
//!
//! Uses blake3 for fast, cryptographically-strong content hashing.
//! The salsa database handles memoization of compilation results;
//! this module provides standalone hashing for filesystem-level caching
//! (e.g., skipping source reads when mtime hasn't changed).

/// Compute the blake3 hex digest of source text.
pub fn content_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_deterministic() {
        let h1 = content_hash("fun main() {}");
        let h2 = content_hash("fun main() {}");
        let h3 = content_hash("fun main() { println(1) }");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 64);
    }
}
