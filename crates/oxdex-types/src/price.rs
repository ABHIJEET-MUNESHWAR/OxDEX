//! Non-negative integer price represented as a rational `num/den`.
//!
//! We avoid floating point in matching/settlement: every price comparison
//! must be exact. A [`Price`] holds two `u128`s so we can combine prices
//! across decimals (e.g. USDC has 6 decimals, SOL has 9) without overflow
//! for any realistic order book.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

use crate::error::{OxDexError, Result};

/// Rational price `num / den`. `den` is always non-zero.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Price {
    /// Numerator (units of buy-token, atomic).
    pub num: u128,
    /// Denominator (units of sell-token, atomic). Invariant: `den > 0`.
    pub den: u128,
}

impl Price {
    /// Construct, returning an error if `den == 0`.
    pub fn new(num: u128, den: u128) -> Result<Self> {
        if den == 0 {
            return Err(OxDexError::InvalidOrder("price denominator is zero".into()));
        }
        Ok(Self { num, den })
    }

    /// Multiply a sell-amount by this price to get the (floor) buy-amount.
    /// Saturates at `u128::MAX` to keep us panic-free on adversarial input.
    pub fn apply(&self, sell_amount: u128) -> u128 {
        sell_amount
            .checked_mul(self.num)
            .map(|v| v / self.den)
            .unwrap_or(u128::MAX)
    }

    /// True if `self >= other`, comparing rationals exactly.
    pub fn ge_rational(&self, other: &Price) -> bool {
        // self.num/self.den >= other.num/other.den  <=>  self.num * other.den >= other.num * self.den
        self.num.saturating_mul(other.den) >= other.num.saturating_mul(self.den)
    }
}

impl PartialOrd for Price {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Price {
    fn cmp(&self, o: &Self) -> Ordering {
        // exact rational compare, saturating to avoid panics on huge values
        let l = self.num.saturating_mul(o.den);
        let r = o.num.saturating_mul(self.den);
        l.cmp(&r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_den() {
        assert!(Price::new(1, 0).is_err());
    }

    #[test]
    fn applies_floor() {
        let p = Price::new(3, 2).unwrap();
        assert_eq!(p.apply(10), 15);
        assert_eq!(p.apply(11), 16); // 11*3/2 = 16.5 -> 16
    }

    #[test]
    fn ordering_is_exact() {
        let a = Price::new(2, 3).unwrap(); // 0.6666
        let b = Price::new(3, 5).unwrap(); // 0.6
        assert!(a > b);
        assert!(a.ge_rational(&b));
    }
}
