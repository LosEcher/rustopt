//! A tiny, dependency-free fingerprint.
//!
//! Used for ledger evidence (compiler stderr hashes, work-dir names). This is
//! **not** a cryptographic hash and is not used for security decisions.

pub const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
pub const PRIME: u64 = 0x0000_0100_0000_01b3;

#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = OFFSET;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

#[must_use]
pub fn hex64(h: u64) -> String {
    format!("{h:016x}")
}

/// Fingerprint of a byte slice, hex-encoded.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> String {
    hex64(fnv1a64(bytes))
}

/// Fingerprint of a string, hex-encoded.
#[must_use]
pub fn hash_str(s: &str) -> String {
    hash_bytes(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn hex_is_stable_width() {
        assert_eq!(hash_str("x").len(), 16);
        assert_ne!(hash_str("x"), hash_str("y"));
    }
}
