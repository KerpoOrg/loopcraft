//! Ids for busses, inputs and outputs: a letter for what it is (`b`, `i`,
//! `o`) and eight hex digits, e.g. `b3f9a2c1d`. An id is made once and never
//! changes, whatever the bus or unit is renamed to or wherever it moves.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

/// A new, random id.
pub fn new_id(kind: char) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    h.write_u128(nanos);
    format!("{kind}{:08x}", h.finish() as u32)
}

/// The id of an entry written without one: the same for the same file every
/// time it is read, so a hand-written config keeps its ids until the editor
/// writes them in.
pub(crate) fn derived_id(kind: char, seed: &str) -> String {
    // FNV-1a.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    // Mix it (murmur3's finalizer), so ids from similar seeds look unrelated.
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 33;
    format!("{kind}{:08x}", h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let a = new_id('b');
        assert_eq!(a.len(), 9);
        assert!(a.starts_with('b') && a[1..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_id('b'), new_id('b'));
        assert_eq!(derived_id('i', "m/3"), derived_id('i', "m/3"));
        assert_ne!(derived_id('i', "m/3"), derived_id('i', "m/4"));
    }
}
