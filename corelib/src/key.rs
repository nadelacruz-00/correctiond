//! The 32-byte raw SQLCipher key. Deliberately has no `Debug`, `Display`,
//! `Clone` or `Serialize`, so it cannot leak through formatting or logs.

use zeroize::Zeroizing;

pub struct StoreKey(Zeroizing<[u8; 32]>);

impl StoreKey {
    /// 32 fresh bytes from the OS RNG.
    pub fn generate() -> StoreKey {
        let mut b = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut b[..]).expect("OS random source unavailable");
        StoreKey(b)
    }

    /// Exactly 64 hex characters (either case), else `None`.
    pub fn from_hex(s: &str) -> Option<StoreKey> {
        let s = s.as_bytes();
        if s.len() != 64 {
            return None;
        }
        let mut b = Zeroizing::new([0u8; 32]);
        for (i, pair) in s.chunks(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            b[i] = (hi * 16 + lo) as u8;
        }
        Some(StoreKey(b))
    }

    pub fn to_hex(&self) -> Zeroizing<String> {
        const H: &[u8; 16] = b"0123456789abcdef";
        let mut out = Zeroizing::new(String::with_capacity(64));
        for byte in self.0.iter() {
            out.push(H[(byte >> 4) as usize] as char);
            out.push(H[(byte & 15) as usize] as char);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_hex_roundtrip() {
        let k = StoreKey::generate();
        assert!(StoreKey::from_hex(&k.to_hex()).is_some());
        assert!(StoreKey::from_hex("zz").is_none());
        assert!(StoreKey::from_hex(&"g".repeat(64)).is_none());
        assert!(StoreKey::from_hex(&"a".repeat(63)).is_none());
        let k2 = StoreKey::generate();
        assert_ne!(*k.to_hex(), *k2.to_hex());
        assert_eq!(k.to_hex().len(), 64);
    }
}
