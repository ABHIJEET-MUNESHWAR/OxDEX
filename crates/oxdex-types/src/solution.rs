//! Solution: a solver's proposed settlement of a [`crate::Batch`].

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::batch::BatchId;
use crate::order::OrderId;
use crate::price::Price;

/// Uniform clearing price for one token within a batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClearingPrice {
    /// Token mint.
    pub mint: Address,
    /// Price of `mint` in the batch's numéraire (atomic units).
    pub price: Price,
}

/// One executed trade in a [`Solution`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TradeExecution {
    /// Order being (partially) filled.
    pub order_id: OrderId,
    /// Atomic units of the order's `sell_mint` actually transferred from the user.
    pub executed_sell: u64,
    /// Atomic units of the order's `buy_mint` actually delivered to the user.
    pub executed_buy: u64,
}

/// A solver's proposed settlement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Solution {
    /// Batch this solution targets.
    pub batch_id: BatchId,
    /// Address (Ed25519 pubkey) of the solver that produced the solution.
    pub solver: Address,
    /// One [`ClearingPrice`] per distinct token referenced by the trades.
    pub clearing_prices: Vec<ClearingPrice>,
    /// Per-order executed amounts.
    pub trades: Vec<TradeExecution>,
    /// Surplus score (in numéraire atomic units). Higher is better.
    pub score: u128,
}

impl Solution {
    /// Look up the clearing price for `mint`.
    pub fn price_of(&self, mint: &Address) -> Option<&Price> {
        self.clearing_prices
            .iter()
            .find(|c| &c.mint == mint)
            .map(|c| &c.price)
    }
}
