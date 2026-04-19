//! Strongly-typed Solana-style 32-byte address (base58 wire format).
//!
//! We avoid pulling in `solana-sdk` here so the `oxdex-types` crate stays
//! light-weight and usable from WASM or no-std contexts later on.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

use crate::error::OxDexError;

/// A 32-byte public key (Solana / Ed25519 compatible).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address(pub [u8; 32]);

impl Address {
    /// Construct an [`Address`] from raw bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// All-zero "system" address (useful as a sentinel/default).
    pub const fn zero() -> Self {
        Self([0u8; 32])
    }

    /// Borrow the underlying bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", bs58::encode(self.0).into_string())
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&bs58::encode(self.0).into_string())
    }
}

impl FromStr for Address {
    type Err = OxDexError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = bs58::decode(s)
            .into_vec()
            .map_err(|e| OxDexError::InvalidAddress(format!("base58 decode: {e}")))?;
        if bytes.len() != 32 {
            return Err(OxDexError::InvalidAddress(format!(
                "expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Self(arr))
    }
}

impl Serialize for Address {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_base58() {
        let a = Address([7u8; 32]);
        let s = a.to_string();
        let b: Address = s.parse().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(Address::from_str("abc").is_err());
    }

    #[test]
    fn json_roundtrip() {
        let a = Address([3u8; 32]);
        let j = serde_json::to_string(&a).unwrap();
        let b: Address = serde_json::from_str(&j).unwrap();
        assert_eq!(a, b);
    }
}
