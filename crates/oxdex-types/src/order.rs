//! Order (a.k.a. "intent") types.
//!
//! An [`Order`] is what a user signs; a [`SignedOrder`] is the wire
//! representation we accept from the API.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::address::Address;
use crate::error::{OxDexError, Result};

/// Stable identifier of an order (32-byte hash of canonical encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OrderId(pub [u8; 32]);

impl OrderId {
    /// Lower-case hex form, useful for logs / DB keys.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Display for OrderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// `Sell` = exact-in, `Buy` = exact-out. Mirrors CoW's `OrderKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderKind {
    /// User commits to selling exactly `sell_amount`.
    Sell,
    /// User commits to receiving exactly `buy_amount`.
    Buy,
}

/// Lifecycle state stored in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// In the open book, eligible for the next batch.
    Open,
    /// Picked up by an auction, awaiting settlement.
    Auctioned,
    /// Fully settled on-chain.
    Filled,
    /// Partially filled (only valid when `partial_fill = true`).
    PartiallyFilled,
    /// Cancelled by the user.
    Cancelled,
    /// Reached `valid_to` without being filled.
    Expired,
}

impl OrderStatus {
    /// Stable string code used as the Postgres column value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Auctioned => "auctioned",
            Self::Filled => "filled",
            Self::PartiallyFilled => "partially_filled",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
    /// Inverse of [`as_str`].
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "auctioned" => Self::Auctioned,
            "filled" => Self::Filled,
            "partially_filled" => Self::PartiallyFilled,
            "cancelled" => Self::Cancelled,
            "expired" => Self::Expired,
            _ => return None,
        })
    }
}

/// The canonical, content-addressed order. Hashing this struct yields the [`OrderId`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    /// Wallet that signed the order and owns the sell-side balance.
    pub owner: Address,
    /// SPL mint of the token being sold.
    pub sell_mint: Address,
    /// SPL mint of the token being bought.
    pub buy_mint: Address,
    /// Amount of `sell_mint` (atomic units).
    pub sell_amount: u64,
    /// Amount of `buy_mint` (atomic units).
    pub buy_amount: u64,
    /// Unix-second timestamp after which the order is no longer valid.
    pub valid_to: i64,
    /// Per-owner replay nonce.
    pub nonce: u64,
    /// Sell vs Buy (exact-in vs exact-out).
    pub kind: OrderKind,
    /// Whether partial fills are allowed.
    pub partial_fill: bool,
    /// Where the bought tokens should be delivered (often == owner).
    pub receiver: Address,
}

impl Order {
    /// Compute the deterministic [`OrderId`] (sha256 of canonical bincode).
    pub fn id(&self) -> OrderId {
        let bytes = bincode::serialize(self).expect("Order is always serializable");
        let mut hasher = Sha256::new();
        hasher.update(b"oxdex/order/v1");
        hasher.update(&bytes);
        let out = hasher.finalize();
        let mut id = [0u8; 32];
        id.copy_from_slice(&out);
        OrderId(id)
    }

    /// Cheap, side-effect-free semantic checks. Does NOT verify signatures or balances.
    pub fn validate(&self, now_unix_secs: i64) -> Result<()> {
        if self.sell_mint == self.buy_mint {
            return Err(OxDexError::InvalidOrder(
                "sell and buy mints are equal".into(),
            ));
        }
        if self.sell_amount == 0 {
            return Err(OxDexError::InvalidOrder("sell_amount must be > 0".into()));
        }
        if self.buy_amount == 0 {
            return Err(OxDexError::InvalidOrder("buy_amount must be > 0".into()));
        }
        if self.valid_to <= now_unix_secs {
            return Err(OxDexError::InvalidOrder(format!(
                "order already expired (valid_to={}, now={})",
                self.valid_to, now_unix_secs
            )));
        }
        Ok(())
    }

    /// Limit price expressed as `buy_amount / sell_amount`.
    pub fn limit_price(&self) -> crate::price::Price {
        // safe: validate() ensures sell_amount != 0 before this is meaningful
        crate::price::Price {
            num: self.buy_amount as u128,
            den: self.sell_amount.max(1) as u128,
        }
    }
}

/// An order plus the user's Ed25519 signature over `id()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOrder {
    /// The order itself.
    pub order: Order,
    /// 64-byte Ed25519 signature over the [`OrderId`] bytes.
    #[serde(with = "serde_bytes_array")]
    pub signature: [u8; 64],
}

impl SignedOrder {
    /// Verify the signature against `order.owner` (treated as Ed25519 pubkey).
    pub fn verify(&self) -> Result<()> {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let vk = VerifyingKey::from_bytes(self.order.owner.as_bytes())
            .map_err(|e| OxDexError::BadSignature(format!("bad pubkey: {e}")))?;
        let sig = Signature::from_bytes(&self.signature);
        vk.verify(&self.order.id().0, &sig)
            .map_err(|e| OxDexError::BadSignature(e.to_string()))
    }
}

/// Helper to (de)serialize `[u8; 64]` as hex (cleaner JSON than serde_bytes default).
mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(&s).map_err(serde::de::Error::custom)?;
        if v.len() != 64 {
            return Err(serde::de::Error::custom("expected 64-byte signature"));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(now: i64) -> Order {
        Order {
            owner: Address([1u8; 32]),
            sell_mint: Address([2u8; 32]),
            buy_mint: Address([3u8; 32]),
            sell_amount: 1_000_000,
            buy_amount: 2_000_000,
            valid_to: now + 60,
            nonce: 0,
            kind: OrderKind::Sell,
            partial_fill: true,
            receiver: Address([1u8; 32]),
        }
    }

    #[test]
    fn id_is_deterministic() {
        let o = sample(1000);
        assert_eq!(o.id(), o.id());
    }

    #[test]
    fn validate_rejects_expired() {
        let mut o = sample(1000);
        o.valid_to = 999;
        assert!(o.validate(1000).is_err());
    }

    #[test]
    fn validate_rejects_same_mint() {
        let mut o = sample(1000);
        o.buy_mint = o.sell_mint;
        assert!(o.validate(1000).is_err());
    }

    #[test]
    fn signed_order_roundtrip_signature_verifies() {
        use ed25519_dalek::{Signer, SigningKey};
        use rand::rngs::OsRng;
        let sk = SigningKey::generate(&mut OsRng);
        let pk = sk.verifying_key();
        let mut o = sample(1000);
        o.owner = Address(pk.to_bytes());
        o.receiver = o.owner;
        let sig = sk.sign(&o.id().0);
        let signed = SignedOrder {
            order: o,
            signature: sig.to_bytes(),
        };
        signed.verify().unwrap();
    }
}
